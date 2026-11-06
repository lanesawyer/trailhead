//! Deeper search (`GET /v1/search`): after a pause in typing, the board's query is also put to the server, whose
//! full-text index covers descriptions and comments. Answers for a query that has since changed are dropped.
//!
//! The local part of searching (key and title, as you type) is [`crate::board::build`]'s; this adds the tasks that
//! match only deeper down.

use std::time::Duration;

use bevy::prelude::*;
use serde_json::Value;

use crate::board::Filter;
use crate::connection::{Connection, SendNet};
use crate::net::NetCommand;

/// How long the query has to sit unchanged before it is put to the server.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// How many hits one search asks for.
pub const LIMIT: u32 = 100;

/// A task the server found, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepHit {
    /// The task's id.
    pub id: String,
    /// `title`, `description` or `comment`.
    pub field: String,
    /// A short excerpt around the match.
    pub snippet: String,
}

/// The server's answer to a search, as the net thread passes it on.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct SearchReply {
    /// Which ask this answers.
    pub seq: u64,
    pub result: Result<Vec<DeepHit>, String>,
}

/// Reads a `/v1/search` body: tasks, each with a `match` object.
pub fn parse_hits(body: &Value) -> Vec<DeepHit> {
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_string();
    body.as_array()
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            let id = hit["id"].as_str()?.to_string();
            Some(DeepHit { id, field: text(&hit["match"], "field"), snippet: text(&hit["match"], "snippet") })
        })
        .collect()
}

/// Asks the server for `query`'s hits.
pub async fn fetch(http: &reqwest::Client, base: &str, token: &str, query: &str) -> Result<Vec<DeepHit>, String> {
    let res = http
        .get(format!("{base}/v1/search"))
        .bearer_auth(token)
        .query(&[("q", query), ("limit", &LIMIT.to_string())])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("the server answered {}", res.status()));
    }
    let body: Value = res.json().await.map_err(|e| e.to_string())?;
    Ok(parse_hits(&body))
}

/// What the server found, and for which query.
#[derive(Resource, Default, Debug)]
pub struct Deep {
    /// The (trimmed) query `hits` answer; empty when none has.
    pub query: String,
    pub hits: Vec<DeepHit>,
    /// The ask in flight, or answered: its number and query.
    asked: Option<(u64, String)>,
    next: u64,
    /// How long the query has been as it is.
    quiet: Duration,
    /// The query as of the last look, to tell when it changes.
    seen: String,
}

impl Deep {
    /// The hits for `query`, if they are the answer to it.
    pub fn for_query(&self, query: &str) -> &[DeepHit] {
        if !query.is_empty() && self.query == query { &self.hits } else { &[] }
    }
}

/// Keeps [`Deep`]; runs headless.
pub struct SearchPlugin;

impl Plugin for SearchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Deep>()
            .init_resource::<Filter>()
            .add_message::<SearchReply>()
            .add_message::<SendNet>()
            .add_systems(Update, (take_replies, ask).chain().run_if(in_state(Connection::Live)))
            .add_systems(Update, forget_when_empty)
            .add_systems(OnExit(Connection::Live), |mut deep: ResMut<Deep>| deep.asked = None);
    }
}

/// An emptied query leaves nothing of the last one.
fn forget_when_empty(filter: Res<Filter>, mut deep: ResMut<Deep>) {
    if filter.is_changed() && filter.query.trim().is_empty() && (!deep.hits.is_empty() || deep.asked.is_some()) {
        // Keep the counter: a reply to an ask from before must not match a later ask's number.
        *deep = Deep { next: deep.next, ..Deep::default() };
    }
}

/// Puts the query to the server once it has been left alone for [`DEBOUNCE`].
fn ask(filter: Res<Filter>, time: Res<Time>, mut deep: ResMut<Deep>, mut net: MessageWriter<SendNet>) {
    let wanted = filter.query.trim().to_string();
    if deep.seen != wanted {
        deep.seen = wanted.clone();
        deep.quiet = Duration::ZERO;
    } else {
        deep.quiet += time.delta();
    }
    let answered = deep.query == wanted || deep.asked.as_ref().is_some_and(|(_, q)| *q == wanted);
    if wanted.is_empty() || answered || deep.quiet < DEBOUNCE {
        return;
    }
    deep.next += 1;
    let seq = deep.next;
    deep.asked = Some((seq, wanted.clone()));
    net.write(SendNet(NetCommand::Search { seq, query: wanted }));
}

/// Keeps an answer only if it is to the ask in flight and the query is still the one asked.
fn take_replies(mut replies: MessageReader<SearchReply>, filter: Res<Filter>, mut deep: ResMut<Deep>) {
    for reply in replies.read() {
        let Some((seq, query)) = deep.asked.clone() else { continue };
        if seq != reply.seq || query != filter.query.trim() {
            continue;
        }
        if let Ok(hits) = &reply.result {
            deep.query = query;
            deep.hits = hits.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use bevy::time::TimeUpdateStrategy;
    use serde_json::json;

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(100)))
            .init_state::<Connection>()
            .init_resource::<Filter>()
            .add_plugins(SearchPlugin);
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
        app
    }

    /// Runs `frames` updates and returns the searches asked in them (messages live only two frames).
    fn pass(app: &mut App, frames: usize) -> Vec<(u64, String)> {
        let mut asked = Vec::new();
        for _ in 0..frames {
            app.update();
            asked.extend(drain(app));
        }
        asked
    }

    fn drain(app: &mut App) -> Vec<(u64, String)> {
        let sent: Vec<_> = app.world_mut().resource_mut::<Messages<SendNet>>().drain().collect();
        sent.into_iter()
            .filter_map(|SendNet(c)| match c {
                NetCommand::Search { seq, query } => Some((seq, query)),
                _ => None,
            })
            .collect()
    }

    fn type_query(app: &mut App, query: &str) {
        app.world_mut().resource_mut::<Filter>().query = query.into();
    }

    fn hit(id: &str, field: &str) -> DeepHit {
        DeepHit { id: id.into(), field: field.into(), snippet: "…".into() }
    }

    #[test]
    fn parses_the_servers_hits() {
        let body =
            json!([{"id": "t1", "key": "TRAIL-1", "match": {"field": "comment", "snippet": "a …b"}}, {"nope": 1}]);
        assert_eq!(
            parse_hits(&body),
            vec![DeepHit { id: "t1".into(), field: "comment".into(), snippet: "a …b".into() }]
        );
        assert!(parse_hits(&json!({"error": "x"})).is_empty());
    }

    #[test]
    fn asks_only_after_a_pause_and_once() {
        let mut app = app();
        type_query(&mut app, "mig");
        assert!(pass(&mut app, 2).is_empty(), "still inside the pause");
        type_query(&mut app, "migrat");
        assert!(pass(&mut app, 3).is_empty(), "typing again restarts the pause");
        assert_eq!(pass(&mut app, 4), vec![(1, "migrat".to_string())]);
        assert!(pass(&mut app, 6).is_empty(), "the same query is not asked twice");
    }

    #[test]
    fn an_answer_to_the_current_query_is_kept() {
        let mut app = app();
        type_query(&mut app, "migrat");
        let (seq_, _) = pass(&mut app, 6).remove(0);
        let seq = seq_;
        app.world_mut().write_message(SearchReply { seq, result: Ok(vec![hit("t1", "description")]) });
        app.update();
        let deep = app.world().resource::<Deep>();
        assert_eq!(deep.for_query("migrat"), &[hit("t1", "description")]);
        assert!(deep.for_query("migrate").is_empty());
    }

    #[test]
    fn an_answer_for_a_query_that_has_changed_is_ignored() {
        let mut app = app();
        type_query(&mut app, "migrat");
        let (seq_, _) = pass(&mut app, 6).remove(0);
        let old = seq_;
        type_query(&mut app, "migration");
        app.world_mut().write_message(SearchReply { seq: old, result: Ok(vec![hit("t1", "description")]) });
        app.update();
        assert!(app.world().resource::<Deep>().for_query("migrat").is_empty());
        assert!(app.world().resource::<Deep>().for_query("migration").is_empty());
        let (new, query) = pass(&mut app, 6).remove(0);
        assert_eq!(query, "migration");
        // The old answer arriving late still changes nothing; the new one is kept.
        app.world_mut().write_message(SearchReply { seq: old, result: Ok(vec![hit("t9", "comment")]) });
        app.world_mut().write_message(SearchReply { seq: new, result: Ok(vec![hit("t2", "comment")]) });
        app.update();
        assert_eq!(app.world().resource::<Deep>().for_query("migration"), &[hit("t2", "comment")]);
    }

    #[test]
    fn clearing_the_query_forgets_the_hits() {
        let mut app = app();
        type_query(&mut app, "migrat");
        let (seq_, _) = pass(&mut app, 6).remove(0);
        let seq = seq_;
        app.world_mut().write_message(SearchReply { seq, result: Ok(vec![hit("t1", "description")]) });
        app.update();
        type_query(&mut app, "");
        app.update();
        assert!(app.world().resource::<Deep>().hits.is_empty());
    }

    #[test]
    fn an_answer_from_before_a_clear_is_not_taken_for_the_next_query() {
        let mut app = app();
        type_query(&mut app, "abc");
        let (old, _) = pass(&mut app, 6).remove(0);
        type_query(&mut app, "");
        app.update();
        type_query(&mut app, "xyz");
        let (new, query) = pass(&mut app, 6).remove(0);
        assert_eq!(query, "xyz");
        assert_ne!(old, new);
        app.world_mut().write_message(SearchReply { seq: old, result: Ok(vec![hit("t1", "description")]) });
        app.update();
        assert!(app.world().resource::<Deep>().for_query("xyz").is_empty());
    }
}

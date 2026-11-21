//! A task's pictures (`docs/factory/ui.md#views`): screenshots the factory attaches before and after a change, which
//! the task modal shows under "Pictures".
//!
//! - [`group`] is a pure function from a task's attachments to what the modal shows: sets (a run, a commit) newest
//!   first, and in each set the pictures paired by name into a before and an after. A name with only an after is new,
//!   one with only a before is removed, and a pair whose bytes are the same is unchanged, which the modal folds away.
//! - The bytes are fetched only for a picture something shows: a frame wanting one carries [`WantsPicture`], and
//!   [`PicturesPlugin`] reads each distinct picture (by its hash) once, a few at a time, decodes it, and keeps it for
//!   the session in [`PictureCache`], from which every frame wanting it is set.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use jiff::Timestamp;
use trailhead_model::{Attachment, AttachmentRole};
use trailhead_shell::connection::{Connection, SendNet};
use trailhead_shell::net::{Fetch, NetCommand};
use trailhead_ui::{Picture, png_image};
use uuid::Uuid;

/// One picture as the modal shows it: which to fetch, and its hash, which says whether two are the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureRef {
    pub id: Uuid,
    pub sha256: String,
}

/// What became of a view between the before and the after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureChange {
    /// Both, with different bytes.
    Changed,
    /// Only an after.
    New,
    /// Only a before.
    Removed,
    /// Both, the same bytes.
    Unchanged,
    /// A picture with no role, shown on its own.
    Plain,
}

impl PictureChange {
    pub fn label(self) -> &'static str {
        match self {
            PictureChange::Changed => "changed",
            PictureChange::New => "new",
            PictureChange::Removed => "removed",
            PictureChange::Unchanged => "unchanged",
            PictureChange::Plain => "",
        }
    }
}

/// One name in a set: its before, its after (or, with no role, the picture), and what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PicturePair {
    pub name: String,
    pub before: Option<PictureRef>,
    pub after: Option<PictureRef>,
    pub change: PictureChange,
}

/// The pictures taken together, under their set (none for those given no set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureSet {
    pub set: Option<String>,
    /// Changed, new, removed and plain pictures, by name.
    pub pairs: Vec<PicturePair>,
    /// The pairs whose bytes didn't change, by name: folded away unless `open`.
    pub unchanged: Vec<PicturePair>,
    /// Whether a person opened the fold; [`group`] leaves it shut.
    pub open: bool,
}

impl PictureSet {
    /// What the fold is kept by.
    pub fn key(&self) -> String {
        self.set.clone().unwrap_or_default()
    }

    /// `run-7`, or "No set".
    pub fn title(&self) -> String {
        self.set.clone().unwrap_or_else(|| "No set".into())
    }
}

/// A name's newest before, after and picture with no role.
type Roles<'a> = (Option<&'a Attachment>, Option<&'a Attachment>, Option<&'a Attachment>);

/// The set's pictures as pairs. Of two pictures with the same name and role in a set, the newer is shown.
fn pairs_of(pictures: Vec<&Attachment>) -> Vec<PicturePair> {
    let mut by_name: BTreeMap<&str, Roles> = BTreeMap::new();
    for a in pictures {
        let (before, after, plain) = by_name.entry(a.name.as_str()).or_default();
        let slot = match a.role {
            Some(AttachmentRole::Before) => before,
            Some(AttachmentRole::After) => after,
            None => plain,
        };
        if slot.is_none_or(|have| (have.created_at, have.id) < (a.created_at, a.id)) {
            *slot = Some(a);
        }
    }
    let reference = |a: &Attachment| PictureRef { id: a.id, sha256: a.sha256.clone() };
    let mut out = Vec::new();
    for (name, (before, after, plain)) in by_name {
        let change = match (before, after) {
            (Some(b), Some(a)) if b.sha256 == a.sha256 => Some(PictureChange::Unchanged),
            (Some(_), Some(_)) => Some(PictureChange::Changed),
            (None, Some(_)) => Some(PictureChange::New),
            (Some(_), None) => Some(PictureChange::Removed),
            (None, None) => None,
        };
        if let Some(change) = change {
            let (before, after) = (before.map(reference), after.map(reference));
            out.push(PicturePair { name: name.to_string(), before, after, change });
        }
        if let Some(p) = plain {
            out.push(PicturePair {
                name: name.to_string(),
                before: None,
                after: Some(reference(p)),
                change: PictureChange::Plain,
            });
        }
    }
    out
}

/// A task's live attachments as the modal shows them: by set, the newest set first, each paired by name.
pub fn group(attachments: &[Attachment]) -> Vec<PictureSet> {
    let mut sets: HashMap<Option<&str>, Vec<&Attachment>> = HashMap::new();
    for a in attachments.iter().filter(|a| a.deleted_at.is_none()) {
        sets.entry(a.set.as_deref()).or_default().push(a);
    }
    let mut sets: Vec<(Timestamp, Option<String>, Vec<&Attachment>)> = sets
        .into_iter()
        .map(|(set, pictures)| {
            let newest = pictures.iter().map(|a| a.created_at).max().expect("a set has a picture");
            (newest, set.map(str::to_string), pictures)
        })
        .collect();
    sets.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    sets.into_iter()
        .map(|(_, set, pictures)| {
            let (unchanged, pairs) = pairs_of(pictures).into_iter().partition(|p| p.change == PictureChange::Unchanged);
            PictureSet { set, pairs, unchanged, open: false }
        })
        .collect()
}

/// The sets whose unchanged pictures a person unfolded, by [`PictureSet::key`], for the session.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct OpenFolds(pub std::collections::HashSet<String>);

impl OpenFolds {
    /// Marks each of `sets` open or shut as kept here.
    pub fn apply(&self, sets: &mut [PictureSet]) {
        for set in sets {
            set.open = self.0.contains(&set.key());
        }
    }
}

/// On a kit picture frame that should show this picture: [`PicturesPlugin`] fetches it (once per hash) and sets the
/// frame's [`Picture`].
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct WantsPicture(pub PictureRef);

/// Where each picture's bytes stand, by hash, for the session.
#[derive(Resource, Default, Debug)]
pub struct PictureCache {
    entries: HashMap<String, Picture>,
    /// Hashes asked for and not answered yet.
    asked: Vec<String>,
}

impl PictureCache {
    /// What a frame wanting `sha256` shows now.
    pub fn picture(&self, sha256: &str) -> Picture {
        self.entries.get(sha256).cloned().unwrap_or_default()
    }

    /// How many reads are in flight.
    pub fn in_flight(&self) -> usize {
        self.asked.len()
    }
}

/// At most this many pictures are read at once, so a long list doesn't ask for everything together.
pub const MAX_IN_FLIGHT: usize = 3;

/// A picture's bytes as the net thread hands them back.
#[derive(Message, Debug, Clone)]
pub struct PictureBytes {
    pub sha256: String,
    pub result: Result<Vec<u8>, String>,
}

/// Fetches the pictures frames want and keeps them; runs headless.
pub struct PicturesPlugin;

impl Plugin for PicturesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PictureCache>()
            .add_message::<PictureBytes>()
            .add_message::<SendNet>()
            .add_systems(Update, (take_pictures, ask_pictures, show_pictures).chain())
            .add_systems(OnEnter(Connection::Live), |mut cache: ResMut<PictureCache>| {
                // Whatever was asked before the link dropped won't be answered, and a failure may have been the link's.
                let cache = &mut *cache;
                for sha in cache.asked.drain(..) {
                    cache.entries.remove(&sha);
                }
                cache.entries.retain(|_, p| !matches!(p, Picture::Failed(_)));
            });
    }
}

/// Asks for each picture a frame wants that nobody has asked for, up to [`MAX_IN_FLIGHT`] at once.
fn ask_pictures(
    connection: Res<State<Connection>>,
    frames: Query<&WantsPicture>,
    mut cache: ResMut<PictureCache>,
    mut net: MessageWriter<SendNet>,
) {
    if *connection.get() != Connection::Live {
        return;
    }
    for WantsPicture(picture) in &frames {
        if cache.asked.len() >= MAX_IN_FLIGHT {
            return;
        }
        if cache.entries.contains_key(&picture.sha256) {
            continue;
        }
        let cache = cache.as_mut();
        cache.entries.insert(picture.sha256.clone(), Picture::Loading);
        cache.asked.push(picture.sha256.clone());
        net.write(SendNet(NetCommand::Fetch(read(picture.clone()))));
    }
}

/// The read of one picture's bytes.
fn read(picture: PictureRef) -> Fetch {
    Fetch::new(move |api| {
        let picture = picture.clone();
        async move {
            let result = match api {
                Some(api) => api.attachment_bytes(picture.id).await.map_err(|e| e.chain()),
                None => Err("not connected".into()),
            };
            PictureBytes { sha256: picture.sha256, result }
        }
    })
}

/// Decodes each picture that arrived into an image to draw, or says why it can't be shown.
fn take_pictures(
    mut replies: MessageReader<PictureBytes>,
    mut cache: ResMut<PictureCache>,
    images: Option<ResMut<Assets<Image>>>,
) {
    let mut images = images;
    for reply in replies.read() {
        let Some(at) = cache.asked.iter().position(|s| *s == reply.sha256) else { continue };
        cache.asked.remove(at);
        let picture = match (&reply.result, images.as_deref_mut()) {
            (Err(why), _) => Picture::Failed(short(why)),
            (Ok(_), None) => Picture::Failed("nothing to draw it with".into()),
            (Ok(bytes), Some(images)) => match png_image(bytes) {
                Ok(image) => {
                    let size = image.size();
                    Picture::Ready { image: images.add(image), width: size.x, height: size.y }
                }
                Err(why) => Picture::Failed(why),
            },
        };
        cache.entries.insert(reply.sha256.clone(), picture);
    }
}

/// A failure in a few words: the API's code and message are enough beside a thumbnail.
fn short(why: &str) -> String {
    if why.contains("404") { "Removed".into() } else { "Couldn't load".into() }
}

/// Sets every frame wanting a picture from the cache, writing only what differs.
fn show_pictures(cache: Res<PictureCache>, mut frames: Query<(Ref<WantsPicture>, &mut Picture)>) {
    for (wants, mut shown) in &mut frames {
        if cache.is_changed() || wants.is_added() {
            shown.set_if_neq(cache.picture(&wants.0.sha256));
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use serde_json::json;
    use trailhead_shell::fixture::{attachment, sid};

    use super::*;

    fn picture(id: &str, name: &str, role: Option<&str>, set: Option<&str>, sha: &str, at: u8) -> Attachment {
        attachment(json!({
            "id": id, "task_id": "t7", "name": name, "role": role, "set": set,
            "sha256": sha, "created_at": trailhead_shell::fixture::at(at),
        }))
    }

    fn names(pairs: &[PicturePair]) -> Vec<(&str, PictureChange)> {
        pairs.iter().map(|p| (p.name.as_str(), p.change)).collect()
    }

    #[test]
    fn a_set_pairs_its_pictures_by_name() {
        let set = Some("run-2");
        let pictures = [
            picture("a1", "board", Some("before"), set, "b1", 1),
            picture("a2", "board", Some("after"), set, "b2", 2),
            picture("a3", "map", Some("before"), set, "m1", 1),
            picture("a4", "map", Some("after"), set, "m1", 2),
            picture("a5", "inbox", Some("after"), set, "i1", 2),
            picture("a6", "ask", Some("before"), set, "k1", 1),
            picture("a7", "logo", None, set, "l1", 3),
        ];
        let sets = group(&pictures);
        assert_eq!(sets.len(), 1);
        let s = &sets[0];
        assert_eq!(s.title(), "run-2");
        assert_eq!(
            names(&s.pairs),
            [
                ("ask", PictureChange::Removed),
                ("board", PictureChange::Changed),
                ("inbox", PictureChange::New),
                ("logo", PictureChange::Plain)
            ]
        );
        assert_eq!(names(&s.unchanged), [("map", PictureChange::Unchanged)]);
        let board = &s.pairs[1];
        assert_eq!(board.before.as_ref().unwrap().id, trailhead_shell::fixture::id("a1"));
        assert_eq!(board.after.as_ref().unwrap().sha256, "b2");
        assert!(!s.open);
    }

    #[test]
    fn sets_come_newest_first_and_the_newer_of_two_pictures_wins() {
        let pictures = [
            picture("a1", "board", Some("after"), Some("run-1"), "x", 1),
            picture("a2", "board", Some("before"), Some("run-2"), "x", 5),
            picture("a3", "board", Some("after"), Some("run-2"), "y", 6),
            picture("a4", "board", Some("after"), Some("run-2"), "z", 7),
            picture("a5", "loose", None, None, "w", 3),
        ];
        let sets = group(&pictures);
        let titles: Vec<String> = sets.iter().map(PictureSet::title).collect();
        assert_eq!(titles, ["run-2", "No set", "run-1"]);
        assert_eq!(sets[0].pairs[0].after.as_ref().unwrap().sha256, "z");
        assert_eq!(names(&sets[2].pairs), [("board", PictureChange::New)]);
        let gone = attachment(json!({"id": "a9", "task_id": "t7", "deleted_at": "2026-10-02T00:00:00Z"}));
        assert!(group(&[gone]).is_empty(), "a removed picture isn't shown");
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_state::<Connection>()
            .add_plugins(PicturesPlugin);
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
        app
    }

    fn sent(app: &mut App) -> usize {
        app.world_mut().resource_mut::<Messages<SendNet>>().drain().count()
    }

    fn wants(app: &mut App, id: &str, sha: &str) -> Entity {
        let wanted = WantsPicture(PictureRef { id: trailhead_shell::fixture::id(id), sha256: sha.into() });
        app.world_mut().spawn((wanted, Picture::default())).id()
    }

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().and_then(|mut w| w.write_image_data(&[255; 8])).unwrap();
        bytes
    }

    #[test]
    fn bytes_are_fetched_only_for_a_picture_shown_and_once_for_its_hash() {
        let mut app = app();
        app.update();
        assert_eq!(sent(&mut app), 0, "nothing shown, nothing fetched");

        let frames: Vec<Entity> = [("a1", "s1"), ("a2", "s1"), ("a3", "s2"), ("a4", "s3"), ("a5", "s4")]
            .into_iter()
            .map(|(id, sha)| wants(&mut app, id, sha))
            .collect();
        app.update();
        assert_eq!(sent(&mut app), MAX_IN_FLIGHT, "a few at a time, and the same bytes once");
        assert_eq!(app.world().get::<Picture>(frames[0]), Some(&Picture::Loading));

        app.world_mut().write_message(PictureBytes { sha256: "s1".into(), result: Ok(png()) });
        app.world_mut().write_message(PictureBytes { sha256: "s2".into(), result: Err("404 not_found".into()) });
        app.update();
        assert_eq!(sent(&mut app), 1, "room for the last one");
        for frame in &frames[..2] {
            let shown = app.world().get::<Picture>(*frame).unwrap();
            assert!(matches!(shown, Picture::Ready { width: 2, height: 1, .. }), "{shown:?}");
        }
        assert_eq!(app.world().get::<Picture>(frames[2]), Some(&Picture::Failed("Removed".into())));
        app.world_mut().write_message(PictureBytes { sha256: "s3".into(), result: Ok(b"not a png".to_vec()) });
        app.update();
        assert!(matches!(app.world().get::<Picture>(frames[3]), Some(Picture::Failed(_))));

        // A frame shown later for a picture already here is set at once, with no fetch.
        let again = wants(&mut app, &sid("a6"), "s1");
        app.update();
        assert_eq!(sent(&mut app), 0);
        assert!(matches!(app.world().get::<Picture>(again), Some(Picture::Ready { .. })));
    }
}

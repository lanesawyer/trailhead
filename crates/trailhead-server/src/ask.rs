//! Asking for work from the window (`docs/app-platform.md`, "Asking for an app"): what can be asked about, the
//! projects pointed at the apps repo, and an ask turned into one graded task in such a project, or into a new app's
//! project and its first task.
//!
//! Only a person asks. The server then acts as that person, with their context, through the same store calls the API
//! makes, so giving a task its first grade and pointing a project at the apps repo need no rule of their own. Everything
//! one ask writes shares the request's batch, so undoing the batch undoes the whole ask.

use std::ops::RangeInclusive;

use serde_json::json;
use trailhead_core::shell::{Ask, AskTarget, AskTargets, Asked, AskedProject, AskedTask, MAX_ASK_CHARS};
use trailhead_core::{
    Ctx, Error, FACTORY_AGENT, Grade, NewProject, NewTask, PROJECT_REPO_KEY, Priority, ProjectPatch, Result, Store,
};

/// App ids the shell or the `trailhead` repo already uses.
const RESERVED: &[&str] = &["shell", "lookout", "example"];

/// Names cargo refuses for a package, or that would shadow a crate every app links: Rust's keywords, the standard
/// library's crates and the device names Windows reserves.
const NOT_A_CRATE: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "become",
    "box",
    "break",
    "const",
    "continue",
    "crate",
    "do",
    "dyn",
    "else",
    "enum",
    "extern",
    "false",
    "final",
    "fn",
    "for",
    "gen",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "macro",
    "match",
    "mod",
    "move",
    "mut",
    "override",
    "priv",
    "pub",
    "ref",
    "return",
    "self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "try",
    "type",
    "typeof",
    "unsafe",
    "unsized",
    "use",
    "virtual",
    "where",
    "while",
    "yield",
    "std",
    "core",
    "alloc",
    "test",
    "proc-macro",
    "con",
    "prn",
    "aux",
    "nul",
];

/// How long an app's id may be.
const ID_CHARS: RangeInclusive<usize> = 2..=32;

/// How long a project key made from a name may be, before a digit is added to make it unique.
const KEY_CHARS: RangeInclusive<usize> = 2..=6;

/// The longest task title an ask's first line makes; a longer line is cut at a word.
const TITLE_CHARS: usize = 80;

/// The longest name a new app may have.
const NAME_CHARS: usize = 60;

/// The active projects pointed at `repo`, by name. `configured` says whether the server has an apps repo at all.
pub async fn targets(store: &Store, ctx: &Ctx, configured: bool, repo: Option<String>) -> Result<AskTargets> {
    let targets = match (configured, &repo) {
        (true, Some(slug)) => pointed_at(store, ctx, slug).await?,
        _ => Vec::new(),
    };
    Ok(AskTargets { configured, repo: repo.filter(|_| configured), targets })
}

async fn pointed_at(store: &Store, ctx: &Ctx, slug: &str) -> Result<Vec<AskTarget>> {
    let mut targets: Vec<AskTarget> = store
        .list_projects(ctx, false)
        .await?
        .into_iter()
        .filter_map(|p| {
            let pointed = p.repo().filter(|r| r.is(slug))?;
            Some(AskTarget { project: p.key, name: p.name, path: pointed.path })
        })
        .collect();
    targets.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.project.cmp(&b.project)));
    Ok(targets)
}

/// Makes what `ask` asks for, as the person `ctx` is. `configured` and `repo` are as for [`targets`].
pub async fn ask(store: &Store, ctx: &Ctx, configured: bool, repo: Option<&str>, ask: Ask) -> Result<Asked> {
    if !ctx.is_human() {
        return Err(Error::Forbidden("only a person can ask for work from the window".into()));
    }
    let repo = match (configured, repo) {
        (false, _) => {
            return Err(Error::Invalid("the server has no apps repo to ask about ([shell] repo in its config)".into()));
        }
        (true, None) => {
            return Err(Error::Invalid(
                "the apps repo isn't known yet: the builder hasn't read its origin, or it isn't on GitHub".into(),
            ));
        }
        (true, Some(repo)) => repo,
    };
    let text = ask.text.trim();
    if text.is_empty() {
        return Err(Error::Invalid("say what you want: the ask is empty".into()));
    }
    if text.chars().count() > MAX_ASK_CHARS {
        return Err(Error::Invalid(format!("an ask is at most {MAX_ASK_CHARS} characters")));
    }
    let today = jiff::Zoned::now().date();
    match (ask.project, ask.new_app) {
        (Some(project), None) => change(store, ctx, repo, &project, text, today).await,
        (None, Some(app)) => new_app(store, ctx, repo, app.name.trim(), text, today).await,
        _ => Err(Error::Invalid("ask about one thing: a `project` or a `new_app`".into())),
    }
}

/// One graded task in the app `project` is.
async fn change(
    store: &Store,
    ctx: &Ctx,
    repo: &str,
    project: &str,
    text: &str,
    today: jiff::civil::Date,
) -> Result<Asked> {
    let targets = pointed_at(store, ctx, repo).await?;
    let Some(target) = targets.iter().find(|t| t.project.eq_ignore_ascii_case(project.trim())) else {
        return Err(Error::Invalid(format!(
            "{} isn't an app in {repo}, so it can't be asked from here",
            project.trim()
        )));
    };
    let new = graded_task(&target.project, title_of(text), format!("Asked from the window, {today}:\n\n{text}"));
    let task = store.create_task(ctx, new).await?;
    Ok(Asked {
        project: AskedProject { key: target.project.clone(), name: target.name.clone(), created: false },
        task: Some(AskedTask { key: task.key.0, title: task.title }),
        notes: Vec::new(),
    })
}

/// A new app's project, pointed at `apps/<id>` of `repo` with the factory as its reviewer, and its first task.
async fn new_app(
    store: &Store,
    ctx: &Ctx,
    repo: &str,
    name: &str,
    text: &str,
    today: jiff::civil::Date,
) -> Result<Asked> {
    if name.is_empty() {
        return Err(Error::Invalid("a new app needs a name".into()));
    }
    if name.chars().count() > NAME_CHARS {
        return Err(Error::Invalid(format!("an app's name is at most {NAME_CHARS} characters")));
    }
    let id = app_id(name).map_err(Error::Invalid)?;
    let path = format!("apps/{id}");
    if let Some(t) = pointed_at(store, ctx, repo).await?.into_iter().find(|t| t.path.as_deref() == Some(&path)) {
        return Err(Error::Invalid(format!(
            "{path} is already {} ({}): ask for a change to it, or give the new app another name",
            t.name, t.project
        )));
    }
    let mut taken: Vec<String> = store.list_projects(ctx, true).await?.into_iter().map(|p| p.key).collect();
    let project = loop {
        let key = project_key(name, &taken).map_err(Error::Invalid)?;
        let new = NewProject {
            key: key.clone(),
            name: name.to_string(),
            brief: brief(name, &id, &key, repo, text, today),
            require_review: true,
            claim_ttl: None,
            metadata: Some(json!({ PROJECT_REPO_KEY: { "repo": repo, "path": path } })),
        };
        match store.create_project(ctx, new).await {
            Err(Error::AlreadyExists(_)) => taken.push(key),
            made => break made?,
        }
    };
    let mut notes = Vec::new();
    if let Some(note) = review_by_factory(store, ctx, &project.key).await {
        notes.push(note);
    }
    let title = format!("Create the {name} app: scaffold {path}, register it in the shell, build its first version");
    let task = match store.create_task(ctx, graded_task(&project.key, title, first_task(name, &id, text, today))).await
    {
        Ok(task) => Some(AskedTask { key: task.key.0, title: task.title }),
        Err(e) => {
            notes.push(format!(
                "The project {} was made, but its first task wasn't ({e}). Add it by hand: \
                 `trailhead --as-me add \"Create the {name} app\" -p {} --grade 2`.",
                project.key, project.key
            ));
            None
        }
    };
    Ok(Asked { project: AskedProject { key: project.key, name: project.name, created: true }, task, notes })
}

/// Makes the factory's agent a reviewer of `project`, so it can approve the work, or says why it isn't one.
async fn review_by_factory(store: &Store, ctx: &Ctx, project: &str) -> Option<String> {
    let agents = match store.list_agents(ctx).await {
        Ok(agents) => agents,
        Err(e) => return Some(format!("Couldn't look for the factory's agent ({e}), so {project} has no reviewer.")),
    };
    if !agents.iter().any(|a| a.name == FACTORY_AGENT && a.revoked_at.is_none()) {
        return Some(format!(
            "There is no `{FACTORY_AGENT}` agent yet (the factory hasn't run here), so {project} has no reviewer and \
             the factory can't approve its work. Once it has run: `trailhead --as-me project edit {project} \
             --reviewers {FACTORY_AGENT}`."
        ));
    }
    let patch = ProjectPatch { reviewers: Some(vec![FACTORY_AGENT.into()]), ..ProjectPatch::default() };
    match store.update_project(ctx, project, patch, None).await {
        Ok(_) => None,
        Err(e) => Some(format!(
            "Couldn't make the factory a reviewer of {project} ({e}): `trailhead --as-me project edit {project} \
             --reviewers {FACTORY_AGENT}`."
        )),
    }
}

/// A task at priority 2 and grade 2: a person's ask is the approval, so it carries the grade that queues it.
fn graded_task(project: &str, title: String, description: String) -> NewTask {
    NewTask {
        project: project.to_string(),
        title,
        description,
        priority: Some(Priority::new(2).expect("2 is a priority")),
        grade: Some(Grade::new(2).expect("2 is a grade")),
        ..NewTask::default()
    }
}

fn brief(name: &str, id: &str, key: &str, repo: &str, text: &str, today: jiff::civil::Date) -> String {
    format!(
        "{name} is an app in the Trailhead shell, asked for from the window on {today}. It lives in `apps/{id}` of \
         the apps repo, {repo}, and is built only from the trailhead-ui kit (the kit's GUIDE.md and the shell's \
         GUIDE.md at the revision the repo pins). Commit subjects are `type({id}): message [{key}-n]`.\n\n\
         What was asked:\n\n{text}"
    )
}

fn first_task(name: &str, id: &str, text: &str, today: jiff::civil::Date) -> String {
    format!(
        "Asked from the window, {today}, as a new app called {name}:\n\n{text}\n\n\
         Done looks like:\n\n\
         - `apps/{id}` is a new crate made as the repo's AGENTS.md and apps/README.md say, its app id `{id}`.\n\
         - It is added to the one list of apps in `shell/src/lib.rs`.\n\
         - It has its design-system lint test and its in-place (redraws) test, both with empty allowlists.\n\
         - The repo's checks pass.\n\
         - The first version does what was asked above. Anything left out is listed as follow-ups in your summary.\n\n\
         This task is grade 2, but adding a crate changes `Cargo.toml` and `shell/`, which the repo's path rules put \
         at grade 3, so a person approves the new app before it merges."
    )
}

/// The ask's first line that says anything, cut at a word to [`TITLE_CHARS`].
pub(crate) fn title_of(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    if line.chars().count() <= TITLE_CHARS {
        return line.to_string();
    }
    let head: String = line.chars().take(TITLE_CHARS - 1).collect();
    let at = head.rfind(char::is_whitespace).filter(|&i| i > 0).unwrap_or(head.len());
    format!("{}…", head[..at].trim_end())
}

/// A name's words: its letters and digits, accented Latin letters taken as their plain ones, everything else a break.
fn words(name: &str) -> Vec<String> {
    let mut words = vec![String::new()];
    for c in name.chars() {
        let plain = fold(c);
        match plain {
            Some(p) => words.last_mut().expect("never empty").push_str(p),
            None if c.is_ascii_alphanumeric() => words.last_mut().expect("never empty").push(c),
            None if c == '\'' || c == '\u{2019}' => {}
            None => words.push(String::new()),
        }
    }
    words.retain(|w| !w.is_empty());
    words
}

/// The plain letters an accented Latin one is written with.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' => "A",
        'æ' => "ae",
        'Æ' => "AE",
        'ç' | 'č' | 'ć' => "c",
        'Ç' | 'Č' | 'Ć' => "C",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' => "e",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ę' => "E",
        'ì' | 'í' | 'î' | 'ï' | 'ī' => "i",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' => "I",
        'ł' => "l",
        'Ł' => "L",
        'ñ' | 'ń' => "n",
        'Ñ' | 'Ń' => "N",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => "o",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' => "O",
        'œ' => "oe",
        'Œ' => "OE",
        'š' | 'ś' => "s",
        'Š' | 'Ś' => "S",
        'ß' => "ss",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' => "u",
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ū' => "U",
        'ý' | 'ÿ' => "y",
        'Ý' | 'Ÿ' => "Y",
        'ž' | 'ź' | 'ż' => "z",
        'Ž' | 'Ź' | 'Ż' => "Z",
        _ => return None,
    })
}

/// An app's id from its name: its words in lowercase joined by `-`, starting with a letter, [`ID_CHARS`] long, and a
/// name cargo takes for a crate once `-` is `_`. `Err` says why a name makes no id.
pub(crate) fn app_id(name: &str) -> std::result::Result<String, String> {
    let id = words(name).join("-").to_ascii_lowercase();
    if id.is_empty() {
        return Err(format!("{name:?} has no letters or digits to make an app's id from"));
    }
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(format!("an app's id starts with a letter, and {name:?} makes `{id}`: start the name with a word"));
    }
    if !ID_CHARS.contains(&id.len()) {
        return Err(format!(
            "{name:?} makes the id `{id}`, which isn't {} to {} characters long",
            ID_CHARS.start(),
            ID_CHARS.end()
        ));
    }
    if RESERVED.contains(&id.as_str()) || NOT_A_CRATE.contains(&id.as_str()) {
        return Err(format!("`{id}` can't be an app's id; give the app another name"));
    }
    Ok(id)
}

/// A project key from a name, not one of `taken` (compared ignoring case): the first four letters of a name of one
/// word, the initials of a name of several, padded from the first word to at least two, at most six; then a digit
/// added (2 to 9) until it is unique.
pub(crate) fn project_key(name: &str, taken: &[String]) -> std::result::Result<String, String> {
    let lettered: Vec<String> = words(name)
        .iter()
        .map(|w| w.chars().filter(char::is_ascii_alphabetic).collect::<String>().to_ascii_uppercase())
        .filter(|w| !w.is_empty())
        .collect();
    let Some(first) = lettered.first() else {
        return Err(format!("{name:?} has no letters to make a project key from"));
    };
    let mut base: String = if lettered.len() == 1 {
        first.chars().take(4).collect()
    } else {
        lettered.iter().take(*KEY_CHARS.end()).filter_map(|w| w.chars().next()).collect()
    };
    for c in first.chars().skip(1) {
        if base.len() >= *KEY_CHARS.start() {
            break;
        }
        base.push(c);
    }
    if base.len() < *KEY_CHARS.start() {
        return Err(format!("{name:?} is too short to make a project key from: give it at least two letters"));
    }
    let free = |key: &String| !taken.iter().any(|t| t.eq_ignore_ascii_case(key));
    std::iter::once(base.clone())
        .chain((2..=9).map(|n| format!("{base}{n}")))
        .find(free)
        .ok_or_else(|| format!("every project key made from {name:?} is taken ({base} to {base}9)"))
}

#[cfg(test)]
mod tests {
    use trailhead_core::Status;

    use super::*;

    const REPO: &str = "lane/trailhead-apps";

    struct Rig {
        store: Store,
        human: Ctx,
        _dir: tempfile::TempDir,
    }

    async fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_local(dir.path().join("t.db")).await.unwrap();
        let (ws, user) = store.bootstrap_local("lane").await.unwrap();
        Rig { store, human: Ctx::user(ws.id, user.id), _dir: dir }
    }

    impl Rig {
        async fn project(&self, key: &str, name: &str, factory: Option<serde_json::Value>) {
            let metadata = factory.map(|f| json!({ "factory": f }));
            let new = NewProject { key: key.into(), name: name.into(), metadata, ..NewProject::default() };
            self.store.create_project(&self.human, new).await.unwrap();
        }

        async fn apps(&self) {
            self.project("TICK", "Ticklist", Some(json!({"repo": REPO, "path": "apps/ticklist"}))).await;
            self.project("ATLAS", "atlas", Some(json!({"repo": "Lane/Trailhead-Apps", "path": "apps/atlas"}))).await;
            self.project("ELSE", "Elsewhere", Some(json!({"repo": "lane/other", "path": "apps/x"}))).await;
            self.project("HOME", "Home", None).await;
        }

        async fn ask(&self, ctx: &Ctx, ask: Ask) -> Result<Asked> {
            super::ask(&self.store, ctx, true, Some(REPO), ask).await
        }

        async fn agent(&self) -> Ctx {
            let agent = self.store.create_agent(&self.human, "claude").await.unwrap();
            Ctx::agent(self.human.workspace_id, self.human.user_id, agent.id)
        }
    }

    #[tokio::test]
    async fn the_targets_are_the_active_projects_pointed_at_the_apps_repo_by_name() {
        let rig = rig().await;
        rig.apps().await;
        let got = targets(&rig.store, &rig.human, true, Some(REPO.into())).await.unwrap();
        assert_eq!((got.configured, got.repo.as_deref()), (true, Some(REPO)));
        let keys: Vec<(&str, &str, Option<&str>)> =
            got.targets.iter().map(|t| (t.project.as_str(), t.name.as_str(), t.path.as_deref())).collect();
        assert_eq!(keys, [("ATLAS", "atlas", Some("apps/atlas")), ("TICK", "Ticklist", Some("apps/ticklist"))]);

        let off = targets(&rig.store, &rig.human, false, Some(REPO.into())).await.unwrap();
        assert_eq!(off, AskTargets::default(), "no apps repo, nothing to ask about");
        let unknown = targets(&rig.store, &rig.human, true, None).await.unwrap();
        assert_eq!((unknown.configured, unknown.targets.len()), (true, 0));
    }

    #[tokio::test]
    async fn an_agent_is_refused() {
        let rig = rig().await;
        rig.apps().await;
        let agent = rig.agent().await;
        let err = rig.ask(&agent, Ask::change("more", "TICK")).await.unwrap_err();
        assert!(matches!(err, Error::Forbidden(_)), "{err}");
        let err = rig.ask(&agent, Ask::new_app("notes", "Trail Notes")).await.unwrap_err();
        assert!(matches!(err, Error::Forbidden(_)), "{err}");
    }

    #[tokio::test]
    async fn a_change_is_one_graded_task_in_a_target_and_is_refused_elsewhere() {
        let rig = rig().await;
        rig.apps().await;
        let text = "  A Completed view\nthat lists what I sent this week, newest first.\n";
        let asked = rig.ask(&rig.human, Ask::change(text, "tick")).await.unwrap();
        assert_eq!(asked.project, AskedProject { key: "TICK".into(), name: "Ticklist".into(), created: false });
        let made = asked.task.unwrap();
        assert_eq!((made.key.as_str(), made.title.as_str()), ("TICK-1", "A Completed view"));
        assert!(asked.notes.is_empty());
        let task = rig.store.get_task(&rig.human, "TICK-1").await.unwrap();
        assert_eq!((task.grade.map(Grade::get), task.priority.get(), task.status), (Some(2), 2, Status::Open));
        assert!(task.description.starts_with("Asked from the window, "), "{}", task.description);
        assert!(task.description.ends_with("A Completed view\nthat lists what I sent this week, newest first."));
        assert_eq!(task.parent_id, None);

        for elsewhere in ["ELSE", "HOME", "NOPE"] {
            let err = rig.ask(&rig.human, Ask::change("more", elsewhere)).await.unwrap_err();
            assert!(matches!(err, Error::Invalid(_)), "{elsewhere}: {err}");
        }
    }

    #[tokio::test]
    async fn a_new_app_is_a_pointed_project_and_its_first_graded_task() {
        let rig = rig().await;
        rig.apps().await;
        rig.store.create_agent(&rig.human, FACTORY_AGENT).await.unwrap();
        rig.project("TN", "Taken", None).await;
        let asked = rig.ask(&rig.human, Ask::new_app("Notes on routes I want to try", " Trail Notes ")).await.unwrap();
        assert_eq!(asked.project, AskedProject { key: "TN2".into(), name: "Trail Notes".into(), created: true });
        assert!(asked.notes.is_empty(), "{:?}", asked.notes);
        let made = asked.task.unwrap();
        assert_eq!(made.key, "TN2-1");
        assert_eq!(
            made.title,
            "Create the Trail Notes app: scaffold apps/trail-notes, register it in the shell, build its first version"
        );

        let project = rig.store.get_project(&rig.human, "TN2").await.unwrap();
        let pointed = project.repo().unwrap();
        assert_eq!((pointed.repo.as_str(), pointed.path.as_deref()), (REPO, Some("apps/trail-notes")));
        assert!(project.require_review);
        let factory = rig.store.list_agents(&rig.human).await.unwrap().into_iter().find(|a| a.name == FACTORY_AGENT);
        assert_eq!(project.reviewers, [factory.unwrap().id]);
        assert!(project.brief.contains("`apps/trail-notes`") && project.brief.contains("[TN2-n]"), "{}", project.brief);
        assert!(project.brief.ends_with("Notes on routes I want to try"));
        let task = rig.store.get_task(&rig.human, "TN2-1").await.unwrap();
        assert_eq!((task.grade.map(Grade::get), task.priority.get()), (Some(2), 2));
        assert!(task.description.contains("Notes on routes I want to try\n\nDone looks like"));
        assert!(task.description.contains("`shell/src/lib.rs`") && task.description.contains("grade 3"));

        let targets = targets(&rig.store, &rig.human, true, Some(REPO.into())).await.unwrap();
        assert!(targets.targets.iter().any(|t| t.project == "TN2"), "the new app can be asked for changes");
        let again = rig.ask(&rig.human, Ask::new_app("twice", "Trail notes")).await.unwrap_err();
        assert!(matches!(&again, Error::Invalid(m) if m.contains("apps/trail-notes")), "{again}");
    }

    #[tokio::test]
    async fn without_the_factorys_agent_the_new_app_has_no_reviewer_and_says_so() {
        let rig = rig().await;
        let asked = rig.ask(&rig.human, Ask::new_app("recipes I cook", "Recipes")).await.unwrap();
        assert_eq!(asked.project.key, "RECI");
        assert_eq!(asked.task.map(|t| t.key).as_deref(), Some("RECI-1"));
        assert_eq!(asked.notes.len(), 1);
        assert!(asked.notes[0].contains("no reviewer"), "{:?}", asked.notes);
        assert!(rig.store.get_project(&rig.human, "RECI").await.unwrap().reviewers.is_empty());
    }

    #[tokio::test]
    async fn a_bad_ask_is_refused() {
        let rig = rig().await;
        rig.apps().await;
        let long = "x".repeat(MAX_ASK_CHARS + 1);
        let both =
            Ask { new_app: Some(trailhead_core::shell::NewApp { name: "A b".into() }), ..Ask::change("x", "TICK") };
        for bad in [
            Ask::change("   \n ", "TICK"),
            Ask::change(long, "TICK"),
            Ask { text: "x".into(), project: None, new_app: None },
            both,
            Ask::new_app("x", "  "),
            Ask::new_app("x", "Shell"),
            Ask::new_app("x", "Ticklist"),
            Ask::new_app("x", "2048"),
        ] {
            let err = rig.ask(&rig.human, bad.clone()).await.unwrap_err();
            assert!(matches!(err, Error::Invalid(_)), "{bad:?}: {err}");
        }
        let err = super::ask(&rig.store, &rig.human, false, None, Ask::change("x", "TICK")).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
        let err = super::ask(&rig.store, &rig.human, true, None, Ask::change("x", "TICK")).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }

    #[test]
    fn a_name_makes_an_id_and_a_key() {
        let none: &[String] = &[];
        let cases: &[(&str, Result2, Result2)] = &[
            ("Trail Notes", Ok("trail-notes"), Ok("TN")),
            ("Recipes", Ok("recipes"), Ok("RECI")),
            ("Go", Ok("go"), Ok("GO")),
            ("My  climbing-log!", Ok("my-climbing-log"), Ok("MCL")),
            ("Bob's Boulders", Ok("bobs-boulders"), Ok("BB")),
            ("Café Crème", Ok("cafe-creme"), Ok("CC")),
            ("Ångström Notes 2", Ok("angstrom-notes-2"), Ok("AN")),
            ("Notes 2", Ok("notes-2"), Ok("NOTE")),
            ("Top 10 Crags", Ok("top-10-crags"), Ok("TC")),
            ("a b c d e f g h", Ok("a-b-c-d-e-f-g-h"), Ok("ABCDEF")),
            ("日記", Err(()), Err(())),
            ("X", Err(()), Err(())),
            ("2048", Err(()), Err(())),
            ("10 Pitches", Err(()), Ok("PITC")),
            ("Shell", Err(()), Ok("SHEL")),
            ("Lookout", Err(()), Ok("LOOK")),
            ("example", Err(()), Ok("EXAM")),
            ("Type", Err(()), Ok("TYPE")),
            ("Test", Err(()), Ok("TEST")),
            ("A very long name for an app that goes on", Err(()), Ok("AVLNFA")),
        ];
        for (name, id, key) in cases {
            assert_eq!(app_id(name).as_deref().map_err(|_| ()), *id, "id of {name:?}");
            assert_eq!(project_key(name, none).as_deref().map_err(|_| ()), *key, "key of {name:?}");
        }
        let id = app_id("Trail Notes").unwrap();
        assert!(id.replace('-', "_").chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }

    type Result2 = std::result::Result<&'static str, ()>;

    #[test]
    fn a_key_that_is_taken_gets_a_digit() {
        let taken = |keys: &[&str]| keys.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(project_key("Trail Notes", &taken(&["tn"])).unwrap(), "TN2");
        assert_eq!(project_key("Trail Notes", &taken(&["TN", "TN2", "TN3"])).unwrap(), "TN4");
        let all: Vec<String> = std::iter::once("TN".to_string()).chain((2..=9).map(|n| format!("TN{n}"))).collect();
        assert!(project_key("Trail Notes", &all).is_err());
    }

    #[test]
    fn a_title_is_the_first_line_cut_at_a_word() {
        assert_eq!(title_of("\n\n  Add a Completed view  \nmore"), "Add a Completed view");
        let long = "Show every route I have sent this season on a map with the grade beside each and a filter by crag";
        let title = title_of(long);
        assert!(title.chars().count() <= TITLE_CHARS && title.ends_with('…'), "{title}");
        assert_eq!(title, "Show every route I have sent this season on a map with the grade beside each…");
        let word = "x".repeat(100);
        assert_eq!(title_of(&word).chars().count(), TITLE_CHARS);
    }
}

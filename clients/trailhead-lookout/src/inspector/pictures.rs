//! The task modal's "Pictures" section: each set's pairs as thumbnails, before beside after, with a fold over the
//! unchanged ones; a press on a thumbnail opens the kit's large view on its pair. The thumbnails' and the large view's
//! frames carry [`WantsPicture`], so their bytes are fetched only once they are shown ([`crate::pictures`]).

use super::*;
use crate::pictures::PictureCache;
use crate::widgets::{Picture, PictureViewSlot, ViewedPicture, open_picture_view, spawn_picture};

/// The section's items: per set, a line naming it and what changed, its pairs, and the fold over its unchanged ones
/// (and them, when open). Each pair's key holds what it shows, so a new picture is a new row and the rest stay.
pub(super) fn picture_items(sets: &[PictureSet]) -> Vec<(String, Block)> {
    let mut items = Vec::new();
    for set in sets {
        let key = set.key();
        items.push((format!("p/{key}/head"), text_block(set_line(set), size::SECONDARY, Tone::Dim)));
        let pair = |p: &PicturePair| (format!("p/{key}/{}", pair_key(p)), Block::Pictures(p.clone()));
        items.extend(set.pairs.iter().map(pair));
        if !set.unchanged.is_empty() {
            let fold = Block::Unchanged { set: key.clone(), count: set.unchanged.len(), open: set.open };
            items.push((format!("p/{key}/fold/{}", set.open), fold));
            if set.open {
                items.extend(set.unchanged.iter().map(pair));
            }
        }
    }
    items
}

fn pair_key(p: &PicturePair) -> String {
    let sha = |r: &Option<crate::pictures::PictureRef>| r.as_ref().map_or("", |r| r.sha256.as_str()).to_string();
    format!("{}/{:?}/{}/{}", p.name, p.change, sha(&p.before), sha(&p.after))
}

/// `run-7 · 2 changed, 1 new`: the set and how many of each kind of change it holds.
pub(super) fn set_line(set: &PictureSet) -> String {
    let mut counts: Vec<String> = Vec::new();
    for change in [PictureChange::Changed, PictureChange::New, PictureChange::Removed] {
        let n = set.pairs.iter().filter(|p| p.change == change).count();
        if n > 0 {
            counts.push(format!("{n} {}", change.label()));
        }
    }
    if !set.unchanged.is_empty() {
        counts.push(format!("{} unchanged", set.unchanged.len()));
    }
    let plain = set.pairs.iter().filter(|p| p.change == PictureChange::Plain).count();
    if plain > 0 {
        counts.push(format!("{plain} picture{}", if plain == 1 { "" } else { "s" }));
    }
    format!("{} · {}", set.title(), counts.join(", "))
}

/// The pictures a pair's thumbnails open, with their captions: Before and After, or the one alone.
fn viewed(pair: &PicturePair) -> Vec<(String, crate::pictures::PictureRef)> {
    let after = if pair.change == PictureChange::Plain { pair.name.clone() } else { "After".into() };
    [("Before".to_string(), &pair.before), (after, &pair.after)]
        .into_iter()
        .filter_map(|(caption, r)| r.clone().map(|r| (caption, r)))
        .collect()
}

/// On a thumbnail: the pair it opens, titled by its name.
#[derive(Component, Debug, Clone)]
pub(super) struct OpensPictures {
    title: String,
    pictures: Vec<(String, crate::pictures::PictureRef)>,
}

/// On the large view's root: what each of its frames shows, by slot.
#[derive(Component, Debug, Clone)]
pub(super) struct ViewWants(Vec<crate::pictures::PictureRef>);

/// On the fold's button: the set whose unchanged pictures it shows or hides.
#[derive(Component, Debug, Clone)]
pub(super) struct ToggleUnchanged(String);

/// A pair's row: its name and what changed, over its thumbnails side by side.
pub(super) fn spawn_pair(commands: &mut Commands, pair: &PicturePair) -> Entity {
    let item =
        commands.spawn(Node { flex_direction: FlexDirection::Column, row_gap: px(space::STACKED), ..default() }).id();
    let line = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
    commands.spawn_scene(text(pair.name.clone(), size::BODY)).insert(ChildOf(line));
    let tone = match pair.change {
        PictureChange::Changed => Some(token::STATUS_INFO),
        PictureChange::New => Some(token::STATUS_OK),
        PictureChange::Removed => Some(token::STATUS_DANGER),
        PictureChange::Unchanged | PictureChange::Plain => None,
    };
    if pair.change != PictureChange::Plain {
        commands.spawn_scene(badge(pair.change.label(), None, tone)).insert(ChildOf(line));
    }
    let thumbs = commands.spawn_scene(bsn! { @row() Node { flex_wrap: FlexWrap::Wrap } }).insert(ChildOf(item)).id();
    let pictures = viewed(pair);
    for (caption, picture) in &pictures {
        let parts = spawn_picture(commands, caption.clone(), Picture::Loading);
        let opens = OpensPictures { title: pair.name.clone(), pictures: pictures.clone() };
        commands.entity(parts.root).insert((opens, ChildOf(thumbs)));
        commands.entity(parts.frame).insert(WantsPicture(picture.clone()));
    }
    item
}

/// The button that shows or hides a set's unchanged pictures.
pub(super) fn spawn_fold(commands: &mut Commands, set: &str, count: usize, open: bool) -> Entity {
    let (caption, icon) = if open {
        (format!("Hide {count} unchanged"), Icon::ChevronUp)
    } else {
        (format!("Show {count} unchanged"), Icon::ChevronDown)
    };
    let fold = bsn! { @button(caption, Some(icon), ButtonKind::Plain) Node { align_self: AlignSelf::FlexStart } };
    commands.spawn_scene(fold).insert(ToggleUnchanged(set.to_string())).id()
}

/// A press on a thumbnail opens its pair large; one on a fold opens or shuts it.
pub(super) fn on_picture_press(
    activate: On<Activate>,
    opens: Query<&OpensPictures>,
    folds_pressed: Query<&ToggleUnchanged>,
    cache: Option<Res<PictureCache>>,
    mut folds: ResMut<OpenFolds>,
    mut commands: Commands,
) {
    if let Ok(ToggleUnchanged(set)) = folds_pressed.get(activate.entity) {
        if !folds.0.remove(set) {
            folds.0.insert(set.clone());
        }
        return;
    }
    let Ok(open) = opens.get(activate.entity) else { return };
    let pictures = open
        .pictures
        .iter()
        .map(|(caption, r)| {
            let picture = cache.as_ref().map_or(Picture::Loading, |c| c.picture(&r.sha256));
            ViewedPicture { caption: caption.clone(), picture }
        })
        .collect();
    let root = open_picture_view(&mut commands, open.title.clone(), pictures);
    commands.entity(root).insert(ViewWants(open.pictures.iter().map(|(_, r)| r.clone()).collect()));
}

/// Gives the large view's frames, once they are spawned, the pictures they want.
pub(super) fn want_viewed(
    slots: Query<(Entity, &PictureViewSlot), Added<PictureViewSlot>>,
    views: Query<&ViewWants>,
    parents: Query<&ChildOf>,
    mut commands: Commands,
) {
    for (frame, slot) in &slots {
        let wants = parents.iter_ancestors(frame).find_map(|e| views.get(e).ok());
        if let Some(picture) = wants.and_then(|w| w.0.get(slot.0)) {
            commands.entity(frame).insert(WantsPicture(picture.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pictures::PictureRef;

    fn pair(name: &str, change: PictureChange, before: Option<&str>, after: Option<&str>) -> PicturePair {
        let r = |s: Option<&str>| s.map(|s| PictureRef { id: Uuid::nil(), sha256: s.into() });
        PicturePair { name: name.into(), before: r(before), after: r(after), change }
    }

    #[test]
    fn a_set_says_what_changed_and_folds_its_unchanged_pictures() {
        let set = PictureSet {
            set: Some("run-7".into()),
            pairs: vec![
                pair("board", PictureChange::Changed, Some("a"), Some("b")),
                pair("inbox", PictureChange::New, None, Some("c")),
            ],
            unchanged: vec![pair("map", PictureChange::Unchanged, Some("d"), Some("d"))],
            open: false,
        };
        assert_eq!(set_line(&set), "run-7 · 1 changed, 1 new, 1 unchanged");
        let keys = |set: &PictureSet| picture_items(std::slice::from_ref(set)).into_iter().map(|(k, _)| k).collect();
        let shut: Vec<String> = keys(&set);
        assert_eq!(shut, ["p/run-7/head", "p/run-7/board/Changed/a/b", "p/run-7/inbox/New//c", "p/run-7/fold/false"]);
        let open: Vec<String> = keys(&PictureSet { open: true, ..set });
        assert_eq!(open[3..], ["p/run-7/fold/true".to_string(), "p/run-7/map/Unchanged/d/d".to_string()]);
    }

    #[test]
    fn a_pair_opens_before_and_after_and_a_new_one_alone() {
        let captions = |p: &PicturePair| viewed(p).into_iter().map(|(c, r)| (c, r.sha256)).collect::<Vec<_>>();
        assert_eq!(
            captions(&pair("board", PictureChange::Changed, Some("a"), Some("b"))),
            [("Before".to_string(), "a".to_string()), ("After".to_string(), "b".to_string())]
        );
        assert_eq!(
            captions(&pair("inbox", PictureChange::New, None, Some("c"))),
            [("After".to_string(), "c".to_string())]
        );
        assert_eq!(
            captions(&pair("logo", PictureChange::Plain, None, Some("e"))),
            [("logo".to_string(), "e".to_string())]
        );
    }

    fn attached(id: i64, name: &str, role: &str, sha: &str) -> SyncItem {
        trailhead_shell::fixture::sync_event(json!({
            "id": id, "entity": "attachment", "entity_id": trailhead_shell::fixture::sid(name), "kind": "attachment.created",
            "after": trailhead_shell::fixture::attachment_json(json!({
                "id": format!("{name}-{role}"), "task_id": "t7", "name": name, "role": role, "set": "run-1", "sha256": sha,
            })),
        }))
    }

    fn thumbnails(app: &mut App) -> Vec<(Entity, String)> {
        let world = app.world_mut();
        let mut found: Vec<(Entity, String)> =
            world.query::<(Entity, &WantsPicture)>().iter(world).map(|(e, w)| (e, w.0.sha256.clone())).collect();
        found.sort_by(|a, b| a.1.cmp(&b.1));
        found
    }

    #[test]
    fn the_section_appears_with_a_picture_and_grows_in_place() {
        let mut app = super::super::testing::app();
        assert!(!super::super::testing::texts(&mut app).iter().any(|t| t == "Pictures"));
        assert!(thumbnails(&mut app).is_empty(), "no pictures, nothing wanted");

        app.world_mut().write_message(attached(200, "board", "after", "s-after"));
        super::super::testing::settle(&mut app);
        assert!(super::super::testing::texts(&mut app).iter().any(|t| t == "Pictures"));
        assert!(super::super::testing::texts(&mut app).iter().any(|t| t == "run-1 · 1 new"));
        let first = thumbnails(&mut app);
        assert_eq!(first.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>(), ["s-after"]);
        let before = super::super::testing::body(&mut app);

        // A before for another view: a new row, and every entity already drawn stays.
        app.world_mut().write_message(attached(201, "map", "before", "s-map"));
        super::super::testing::settle(&mut app);
        let after = super::super::testing::body(&mut app);
        let kept = before.iter().filter(|e| after.contains(e)).count();
        assert!(
            before.len() - kept <= 1,
            "only the set's line may change: {} of {} gone",
            before.len() - kept,
            before.len()
        );
        let now = thumbnails(&mut app);
        assert_eq!(now.len(), 2);
        assert!(now.contains(&first[0]), "the first thumbnail is the same entity");
        assert!(super::super::testing::texts(&mut app).iter().any(|t| t == "run-1 · 1 new, 1 removed"));
    }
}

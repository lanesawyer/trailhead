//! The subscription usage card at the bottom of the sidebar: how full each of the subscription's usage windows is and
//! when it resets, as the factory's runs last reported it (`FactorySnapshot::usage`). Claude's are the five-hour session and
//! the seven-day week; a window this build has no name for shows under its own.
//!
//! The card is spawned once with the sidebar. Its meters follow the windows through [`Keyed`], and their words are
//! worked out again each minute, since "resets in" moves with the clock.

use bevy::prelude::*;
use trailhead_factory::UsageWindow;

use crate::factory::FactoryStatus;
use crate::widgets::{
    Keyed, MeterPart, Progress, ProgressTone, Token, meter, set_display, set_text, sidebar_card, size, space, text_dim,
    token,
};

/// At or past this percent a window's bar warns, before it reaches any `pause_at`.
const WARN_AT: u32 = 80;

/// The card.
#[derive(Component)]
pub struct UsageCard;

/// The column of meters inside it, one per window.
#[derive(Component)]
pub struct UsageMeters;

/// The line shown while no window has been heard of.
#[derive(Component)]
pub struct UsageEmpty;

/// One window's meter.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRow {
    pub label: String,
    pub value: String,
    pub note: String,
    pub fraction: f32,
    pub tone: Token,
}

#[derive(Resource, Default)]
pub struct UsageMeterEntities(Keyed<UsageRow>);

/// Spawns the card as the last child of `parent` so far: put it just before the sidebar's footer.
pub fn spawn_usage_card(commands: &mut Commands, parent: Entity) {
    let card = commands.queue_spawn_scene(sidebar_card()).insert((Name::new("usage"), UsageCard, ChildOf(parent))).id();
    commands.queue_spawn_scene(text_dim("Subscription", size::SECONDARY)).insert(ChildOf(card));
    commands
        .queue_spawn_scene(text_dim("No reading yet: the next factory run reports it.", size::SMALL))
        .insert((UsageEmpty, ChildOf(card)));
    commands.spawn((
        Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() },
        UsageMeters,
        ChildOf(card),
    ));
}

/// The meters for `usage` at `now` (unix seconds): the session first, then the week, then any other window by name.
pub fn usage_rows(usage: &[UsageWindow], now: i64) -> Vec<(String, UsageRow)> {
    let mut rows: Vec<(u8, String, UsageRow)> = usage
        .iter()
        .map(|w| {
            let (order, label) = window_label(&w.window);
            // A reading past its reset is from a window that has since emptied.
            let reset = w.resets_at <= now;
            let used = if reset { 0 } else { w.used_percent };
            let paused = !reset && w.pause_at.is_some_and(|p| used >= p);
            let tone = match used {
                _ if paused || used >= 100 => token::STATUS_DANGER,
                u if u >= WARN_AT => token::STATUS_WARN,
                _ => token::FILL_ACCENT,
            };
            let note = match (reset, paused) {
                (true, _) => "reset".to_string(),
                (false, true) => format!("new runs paused, resets in {}", until(w.resets_at - now)),
                (false, false) => format!("resets in {}", until(w.resets_at - now)),
            };
            let row = UsageRow { label, value: format!("{used}%"), note, fraction: used as f32 / 100.0, tone };
            (order, w.window.clone(), row)
        })
        .collect();
    rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    rows.into_iter().map(|(_, window, row)| (window, row)).collect()
}

/// What to call a window, and where it goes among the others.
fn window_label(window: &str) -> (u8, String) {
    match window {
        "five_hour" => (0, "Session".into()),
        "seven_day" => (1, "Weekly".into()),
        "seven_day_opus" => (2, "Weekly (Opus)".into()),
        "seven_day_sonnet" => (2, "Weekly (Sonnet)".into()),
        other => {
            let words = other.replace('_', " ");
            let mut chars = words.chars();
            let first = chars.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
            (3, first + chars.as_str())
        }
    }
}

/// `secs` as `45m`, `2h 10m` or `3d 4h`; under a minute reads as `1m`.
fn until(secs: i64) -> String {
    let mins = (secs.max(0) + 59) / 60;
    match mins {
        0..60 => format!("{}m", mins.max(1)),
        60..1440 => format!("{}h {}m", mins / 60, mins % 60),
        _ => format!("{}d {}h", mins / 1440, mins % 1440 / 60),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Keeps the meters in step with the factory's readings and the clock, in place.
pub fn sync_usage(
    status: Res<FactoryStatus>,
    (meters, empty): (Query<Entity, With<UsageMeters>>, Query<Entity, With<UsageEmpty>>),
    (children, parts, mut texts, mut nodes): (Query<&Children>, Query<&MeterPart>, Query<&mut Text>, Query<&mut Node>),
    (mut bars, tones): (Query<&mut Progress>, Query<&ProgressTone>),
    mut entities: ResMut<UsageMeterEntities>,
    mut commands: Commands,
    mut drawn: Local<Option<(Entity, i64)>>,
) {
    let Ok(list) = meters.single() else { return };
    let minute = unix_now() / 60;
    if *drawn == Some((list, minute)) && !status.is_changed() {
        return;
    }
    *drawn = Some((list, minute));
    let usage = status.usage();
    for e in &empty {
        set_display(&mut nodes, e, usage.is_empty());
    }
    let wanted = [(list, usage_rows(&usage, unix_now()))];
    entities.0.sync(
        &mut commands,
        &wanted,
        |commands, _, row| {
            commands
                .spawn_scene(meter(
                    row.label.clone(),
                    row.value.clone(),
                    row.note.clone(),
                    row.fraction,
                    row.tone.clone(),
                ))
                .id()
        },
        |commands, entity, row| {
            for e in children.iter_descendants(entity) {
                if let Ok(mut bar) = bars.get_mut(e) {
                    bar.set_if_neq(Progress(row.fraction));
                    if tones.get(e).is_ok_and(|t| t.0 != row.tone) {
                        commands.entity(e).insert(ProgressTone(row.tone.clone()));
                    }
                }
                let words = match parts.get(e) {
                    Ok(MeterPart::Label) => &row.label,
                    Ok(MeterPart::Value) => &row.value,
                    Ok(MeterPart::Note) => &row.note,
                    Err(_) => continue,
                };
                if let Ok(text) = texts.get_mut(e) {
                    set_text(text, words);
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(window: &str, used_percent: u32, resets_at: i64, pause_at: Option<u32>) -> UsageWindow {
        UsageWindow { window: window.into(), used_percent, resets_at, pause_at }
    }

    #[test]
    fn the_session_comes_before_the_week_and_says_when_it_resets() {
        let now = 1_000_000;
        let rows = usage_rows(
            &[
                window("seven_day", 12, now + 3 * 86400 + 4 * 3600, None),
                window("five_hour", 42, now + 2 * 3600 + 10 * 60, None),
            ],
            now,
        );
        let shown: Vec<(&str, &str, &str)> =
            rows.iter().map(|(_, r)| (r.label.as_str(), r.value.as_str(), r.note.as_str())).collect();
        assert_eq!(shown, [("Session", "42%", "resets in 2h 10m"), ("Weekly", "12%", "resets in 3d 4h")]);
        assert_eq!(rows[0].1.tone, token::FILL_ACCENT);
        assert_eq!(rows[0].1.fraction, 0.42);
    }

    #[test]
    fn a_filling_window_warns_and_one_past_its_pause_says_runs_are_paused() {
        let now = 50;
        let rows = usage_rows(
            &[window("five_hour", 85, now + 600, Some(90)), window("seven_day", 91, now + 60, Some(90))],
            now,
        );
        assert_eq!(rows[0].1.tone, token::STATUS_WARN);
        assert_eq!(rows[1].1.tone, token::STATUS_DANGER);
        assert_eq!(rows[1].1.note, "new runs paused, resets in 1m");
    }

    #[test]
    fn a_reading_past_its_reset_reads_as_empty() {
        let rows = usage_rows(&[window("five_hour", 99, 10, Some(80))], 20);
        assert_eq!((rows[0].1.value.as_str(), rows[0].1.note.as_str()), ("0%", "reset"));
        assert_eq!(rows[0].1.tone, token::FILL_ACCENT);
    }

    #[test]
    fn a_window_without_a_name_here_shows_under_its_own_last() {
        let rows = usage_rows(&[window("monthly_tokens", 5, 100, None), window("five_hour", 1, 100, None)], 0);
        assert_eq!(rows[1].1.label, "Monthly tokens");
    }
}

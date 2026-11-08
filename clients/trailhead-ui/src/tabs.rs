//! A strip of tabs: captions on a hairline, the one picked underlined in the accent, so it reads as the edge of what
//! it shows rather than a row of buttons.
//!
//! The behavior is `bevy_ui_widgets`' headless `TabList`: a click or an arrow key raises `ValueChange<Option<Entity>>`
//! on the strip, naming the tab. Nothing here writes `SelectedTab` back; whoever owns the pages does, from whatever it
//! keeps as the page shown (or adds `bevy::ui_widgets::tablist_self_update` when the strip is all there is), and the
//! derived `Selected` on each tab is what [`style_tabs`] paints from.
//!
//! A strip narrower than its captions cuts them rather than spilling past its edge: [`fit_tabs`] shares the room out,
//! leaving whole the captions short enough to fit and cutting the rest to an even share.
//!
//! Ported from bevy-data-explorer's `widgets/tabs.rs`.

use bevy::feathers::display::label;
use bevy::feathers::focus::FocusIndicator;
use bevy::feathers::theme::{ThemeBorderColor, ThemeTextColor};
use bevy::feathers::tokens;
use bevy::picking::cursor::EntityCursor;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::{InteractionDisabled, Selected};
use bevy::ui_widgets::{ControlOrientation, Tab, TabActivation, TabList};
use bevy::window::SystemCursorIcon;

use crate::patch::set_text;
use crate::spacing::space;
use crate::text::size;
use crate::theme::{BorderTone, token};
use crate::truncate::{truncate_to_width, width_of};

/// How thick the strip's rule is.
const SEAM: f32 = 1.0;
/// How thick the line under the tab picked is.
const INDICATOR: f32 = 2.0;

/// A tab's caption, painted by [`style_tabs`] and cut to fit by [`fit_tabs`], holding the whole of it.
#[derive(Component, Clone, Default)]
pub struct TabCaption(String);

/// The strip the tabs go in, ruled along the edge they share with what they show: below it across the top, beside it
/// down the side.
pub fn tab_strip(orientation: ControlOrientation) -> impl Scene {
    let (direction, rule) = match orientation {
        ControlOrientation::Horizontal => (FlexDirection::Row, UiRect::bottom(px(SEAM))),
        ControlOrientation::Vertical => (FlexDirection::Column, UiRect::right(px(SEAM))),
    };
    bsn! {
        Node {
            flex_direction: { direction },
            border: { rule },
        }
        // Automatic, since a page shows at once: an arrow key turns to the next tab rather than only focusing it.
        TabList {
            orientation: { orientation },
            activation: { TabActivation::Automatic },
        }
        BorderTone({ token::DIVIDER })
    }
}

/// One tab, for a [`tab_strip`] of the same `orientation`. Its indicator sits over the strip's rule rather than beside
/// it, so the tab picked cuts into the line.
pub fn tab(caption: impl Into<String>, orientation: ControlOrientation) -> impl Scene {
    let caption = caption.into();
    let full = caption.clone();
    let (indicator, overlap, justify) = match orientation {
        ControlOrientation::Horizontal => {
            (UiRect::bottom(px(INDICATOR)), UiRect::bottom(px(-SEAM)), JustifyContent::Center)
        }
        ControlOrientation::Vertical => (UiRect::right(px(INDICATOR)), UiRect::right(px(-SEAM)), JustifyContent::Start),
    };
    bsn! {
        Node {
            justify_content: { justify },
            align_items: { AlignItems::Center },
            padding: { UiRect::axes(px(space::PANEL_INSET), px(space::CONTROL_INSET)) },
            border: { indicator },
            margin: { overlap },
            min_width: { px(0) },
            overflow: { Overflow::clip_x() },
        }
        Tab
        Hovered
        FocusIndicator
        EntityCursor::System(SystemCursorIcon::Pointer)
        ThemeBorderColor({ tokens::BUTTON_PLAIN_BG })
        Children [
            @label(caption)
            TextLayout { linebreak: { LineBreak::NoWrap } }
            TabCaption({ full })
        ]
    }
}

/// Cut each strip's captions to the room it has. Measured against the strip rather than each tab, whose width follows
/// its caption and so would never grow back once cut.
pub fn fit_tabs(
    strips: Query<(&TabList, &ComputedNode, &Children)>,
    tabs: Query<(&Node, &Children), With<Tab>>,
    mut captions: Query<(&TabCaption, &mut Text)>,
) {
    let inset = 2.0 * space::PANEL_INSET;
    for (list, strip, children) in &strips {
        let room = strip.size().x * strip.inverse_scale_factor();
        if room <= 0.0 {
            continue;
        }
        let shown: Vec<Entity> = children
            .iter()
            .filter_map(|tab| {
                let (node, captions) = tabs.get(tab).ok()?;
                (node.display != Display::None).then(|| captions.iter())
            })
            .flatten()
            .filter(|caption| captions.contains(*caption))
            .collect();
        let wanted: Vec<f32> = shown
            .iter()
            .map(|&caption| {
                let (TabCaption(full), _) = captions.get(caption).unwrap();
                width_of(full.chars().count(), size::BODY) + inset
            })
            .collect();
        let widths = match list.orientation {
            ControlOrientation::Horizontal => share(&wanted, room),
            ControlOrientation::Vertical => wanted.iter().map(|w| w.min(room)).collect(),
        };
        for (&caption, width) in shown.iter().zip(widths) {
            let Ok((TabCaption(full), text)) = captions.get_mut(caption) else {
                continue;
            };
            set_text(text, &truncate_to_width(full, width - inset, size::BODY));
        }
    }
}

/// Share `room` among tabs wanting `wanted`: whole to every tab that wants no more than an even share of what is left,
/// and that even share to the rest.
fn share(wanted: &[f32], room: f32) -> Vec<f32> {
    let mut order: Vec<usize> = (0..wanted.len()).collect();
    order.sort_by(|&a, &b| wanted[a].total_cmp(&wanted[b]));
    let mut widths = vec![0.0; wanted.len()];
    let mut left = room;
    for (placed, &tab) in order.iter().enumerate() {
        let even = left / (order.len() - placed) as f32;
        widths[tab] = wanted[tab].min(even);
        left -= widths[tab];
    }
    widths
}

/// Underline the tab picked and brighten its caption, and the caption of one hovered; the rest are dimmed. The theme
/// components are immutable, so a change is an insert, which is what has feathers repaint them.
#[allow(clippy::type_complexity)]
pub fn style_tabs(
    tabs: Query<(Entity, &Hovered, Has<Selected>, Has<InteractionDisabled>, &ThemeBorderColor, &Children), With<Tab>>,
    captions: Query<&ThemeTextColor, With<TabCaption>>,
    mut commands: Commands,
) {
    for (tab, hovered, selected, disabled, border, children) in &tabs {
        let (line, text) = match (selected, hovered.0 && !disabled) {
            (true, _) => (tokens::BUTTON_PRIMARY_BG, tokens::TEXT_MAIN),
            (false, true) => (tokens::BUTTON_PLAIN_BG, tokens::TEXT_MAIN),
            (false, false) => (tokens::BUTTON_PLAIN_BG, tokens::TEXT_DIM),
        };
        if border.0 != line {
            commands.entity(tab).insert(ThemeBorderColor(line));
        }
        for &child in children {
            if captions.get(child).is_ok_and(|color| color.0 != text) {
                commands.entity(child).insert(ThemeTextColor(text.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::share;

    #[test]
    fn tabs_that_fit_are_left_whole() {
        assert_eq!(share(&[50.0, 80.0], 200.0), vec![50.0, 80.0]);
    }

    #[test]
    fn a_short_tab_stays_whole_while_a_long_one_is_cut() {
        assert_eq!(share(&[150.0, 40.0], 120.0), vec![80.0, 40.0]);
    }

    #[test]
    fn tabs_too_long_for_the_room_share_it_evenly() {
        assert_eq!(share(&[150.0, 100.0], 120.0), vec![60.0, 60.0]);
    }
}

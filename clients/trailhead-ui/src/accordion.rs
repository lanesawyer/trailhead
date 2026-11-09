//! Collapsible sections: a title, an open flag, and whatever children a caller hangs off it.
//!
//! The look comes from feathers' own containers (a pane for a section of a panel, a group for a section inside one)
//! and only the collapsing is ours, since feathers has no container that collapses. Anything drawn on a section names
//! a feathers token and not a color, so a theme change turns it over along with everything else.
//!
//! The caret is drawn rather than feathers' disclosure toggle: that is a checkbox with a tab stop and a press of its
//! own, and inside the header's button it would take the presses meant for the section.
//!
//! Ported from bevy-data-explorer's `widgets/accordion.rs`.

use bevy::app::Propagate;
use bevy::feathers::containers::{group_body, group_header, pane_body, pane_header};
use bevy::feathers::controls::{ButtonVariant, FeathersButton, FeathersToolButton};
use bevy::feathers::display::label;
use bevy::feathers::font_styles::InheritableFont;
use bevy::feathers::rounded_corners::RoundedCorners;
use bevy::feathers::theme::{SurfaceLevel, ThemeBorderColor, ThemeContext};
use bevy::feathers::tokens;
use bevy::prelude::*;

use crate::theme::{TextTone, token};
use bevy::ui_widgets::Activate;

use crate::icons::{Icon, button_icon, icon_text};
use crate::patch::{display, patch_node, set_text};
use crate::spacing::{radius, space, stroke};
use crate::text::{Size, size};
use crate::truncate::truncate_to_width;

/// Space between a body's sides and what is in it, the same on both so its contents sit centered in the box rather
/// than against its right edge.
pub const ACCORDION_INDENT: f32 = space::CONTROL_INSET;
/// Size the accordion titles are drawn at, which sets how many characters fit.
const TITLE_FONT: Size = size::BODY;
/// How deep a section sits, which decides what it is drawn as.
///
/// Both are feathers containers, and the difference is only which set of tokens they take: a pane reads as one of a
/// panel's own divisions, a group as something inside one. Nesting a pane in a pane gives two identical headers one
/// indent apart, with nothing to say which owned which.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SectionLevel {
    /// A division of a panel itself.
    Pane,
    /// A section within a pane.
    Group,
}

/// A collapsible section. Set `open` to open or close it; a press on its header toggles it.
#[derive(Component, Clone, Default)]
pub struct Accordion {
    pub open: bool,
}

/// The entities an accordion keeps in step with whether it is open.
#[derive(Component)]
pub(crate) struct AccordionPieces {
    /// The header's container, which also holds any controls added to it.
    header: Entity,
    /// The button filling the header that opens and closes the section.
    toggle: Entity,
    outline: Entity,
    body: Entity,
}

/// An accordion's header row, which pads its end once controls are added.
#[derive(Component, Clone, Default)]
pub(crate) struct AccordionHeaderRow;

/// The caret drawn in a header, which turns as the section opens.
#[derive(Component, Clone, Default)]
pub struct AccordionCaret;

/// The pieces of a spawned accordion that callers need.
pub struct AccordionParts {
    pub section: Entity,
    /// Fill this with the section's contents.
    pub body: Entity,
    /// Add controls beside the title here, with [`spawn_header_button`] or an [`icon_menu`](crate::icon_menu).
    pub header: Entity,
}

/// Spawn an accordion, drawn as the feathers container `level` names.
///
/// The container supplies the look (a headed box with its own background, border and rounded corners) and this
/// supplies the behavior feathers has none of: a header that collapses the body under it. What the header holds
/// beyond its title is the caller's business, so the header is returned along with the body.
pub fn spawn_accordion(commands: &mut Commands, title: &str, open: bool, level: SectionLevel) -> AccordionParts {
    let accordion = commands
        .spawn_scene(bsn! {
            Accordion { open: { open } }
            Node {
                flex_direction: { FlexDirection::Column },
                width: { percent(100) },
            }
        })
        .id();

    // No border or padding, so the toggle fills the header and its hover reaches every edge.
    // `pad_accordion_headers` makes room at the end once a control is added there. The border is drawn over the
    // toggle instead, by an outline spawned after it: a toggle inside a border was placed in whole physical pixels
    // while the border was not, and at a fractional scale factor the hover stopped a pixel short of it on the right
    // and bottom. A clip, so a long title cannot push those controls out.
    let header = commands
        .spawn_scene(bsn! {
            @{header_container(level)}
            AccordionHeaderRow
            Node {
                width: { percent(100) },
                padding: { UiRect::ZERO },
                column_gap: { px(space::CONTROLS) },
                border: { UiRect::ZERO },
                border_radius: { header_corners(open).to_border_radius(radius::CONTROL) },
                overflow: { Overflow::clip() },
            }
            InheritableFont { font_size: { FontSize::from(TITLE_FONT) } }
        })
        .id();

    let toggle = commands
        .spawn_scene(bsn! {
            // The plain variant, so a full-width section header picks up hover and press feedback without taking on
            // button chrome.
            @FeathersButton {
                @variant: { ButtonVariant::Plain }
            }
            on(move |_: On<Activate>, mut accordions: Query<&mut Accordion>| {
                if let Ok(mut section) = accordions.get_mut(accordion) {
                    section.open = !section.open;
                }
            })
            Node {
                flex_grow: { 1.0_f32 },
                // A long title must not push the header's buttons out of a header that clips.
                flex_shrink: { 1.0_f32 },
                min_width: { px(0) },
                overflow: { Overflow::clip() },
                // Stretched to the header's height rather than feathers' row height, so the hover fills it top to
                // bottom.
                height: { Val::Auto },
                align_self: { AlignSelf::Stretch },
                border_radius: { header_corners(open).to_border_radius(radius::CONTROL) },
                align_items: { AlignItems::Center },
                justify_content: { JustifyContent::Start },
                column_gap: { px(space::ICON_LABEL) },
                padding: { UiRect::horizontal(px(space::CONTROL_INSET)) },
            }
            Children [
                AccordionCaret
                @icon_text(caret(open))
                TextTone({ token::TEXT })
                --
                AccordionTitle { full: { title.to_string() } }
                Node {
                    flex_grow: { 1.0_f32 },
                    flex_shrink: { 1.0_f32 },
                    min_width: { px(0) },
                }
                @label(title.to_string())
                // Long titles are cut to fit rather than wrapped onto a second line, which would break the header's
                // height.
                TextLayout { linebreak: { LineBreak::NoWrap } }
            ]
        })
        .id();
    // All round, where feathers leaves the bottom open for the body to close: a closed section has no body showing.
    // It stays when the section opens, as a rule under the header, since one that came and went changed the header's
    // height and shifted everything under it on every toggle.
    let border = match level {
        SectionLevel::Pane => tokens::PANE_HEADER_BORDER,
        SectionLevel::Group => tokens::GROUP_BORDER,
    };
    let outline = commands
        .spawn_scene(bsn! {
            Node {
                position_type: { PositionType::Absolute },
                left: { px(0) },
                right: { px(0) },
                top: { px(0) },
                bottom: { px(0) },
                border: { UiRect::all(px(stroke::HAIRLINE)) },
                border_radius: { header_corners(open).to_border_radius(radius::CONTROL) },
            }
            ThemeBorderColor({ border })
            Pickable::IGNORE
        })
        .id();
    commands.entity(header).add_children(&[toggle, outline]);

    let body = commands
        .spawn_scene(bsn! {
            @{body_container(level)}
            @{body_patch(open)}
        })
        .id();

    commands.entity(accordion).add_children(&[header, body]).insert(AccordionPieces { header, toggle, outline, body });
    AccordionParts { section: accordion, body, header }
}

/// Add a button to the right of an accordion's header, returning it so the caller can attach its own marker and act
/// on it.
pub fn spawn_header_button(commands: &mut Commands, header: Entity, icon: Icon) -> Entity {
    let button = commands
        .spawn_scene(bsn! {
            @FeathersToolButton {
                @caption: { bsn_list! {@button_icon(icon)} }
            }
            // Header buttons hold their size; the title beside them gives way instead. Without this the title's
            // growth squeezed them out of a header that clips, and they simply were not there to click.
            Node { flex_shrink: { 0.0_f32 } }
        })
        .id();
    commands.entity(header).add_child(button);
    button
}

/// An accordion's untruncated title, kept because the text it is shown through is rewritten to fit.
#[derive(Component, Clone, Default)]
pub struct AccordionTitle {
    pub full: String,
}

/// Cut each accordion title to whatever room its header leaves it.
pub fn truncate_accordion_titles(titles: Query<(Entity, &AccordionTitle, &ComputedNode)>, mut texts: Query<&mut Text>) {
    for (entity, title, node) in &titles {
        let width = node.size().x * node.inverse_scale_factor();
        if width <= 0.0 {
            continue;
        }
        let wanted = truncate_to_width(&title.full, width, TITLE_FONT);
        if let Ok(text) = texts.get_mut(entity) {
            set_text(text, &wanted);
        }
    }
}

/// What every accordion body patches over its container, whichever it is: the indent its contents are read against,
/// and the state to start in.
fn body_patch(open: bool) -> impl Scene {
    bsn! {
        Node {
            // Set here rather than left for `sync_accordions` to correct: that runs a frame later, and a section
            // spawned closed would draw its contents once before being hidden.
            display: { display(open) },
            width: { percent(100) },
            row_gap: { px(space::ROWS) },
            border: { UiRect::new(px(stroke::HAIRLINE), px(stroke::HAIRLINE), px(0), px(stroke::HAIRLINE)) },
            padding: { UiRect::new(
                px(ACCORDION_INDENT),
                px(ACCORDION_INDENT),
                px(space::CONTROL_INSET),
                px(space::CONTROL_INSET),
            ) },
        }
    }
}

/// The feathers container a header is drawn as at `level`.
///
/// The two differ only in their tokens, but each is a scene type of its own, so the choice is boxed for the one spawn
/// that patches over either.
fn header_container(level: SectionLevel) -> Box<dyn Scene> {
    match level {
        SectionLevel::Pane => Box::new(bsn! { @pane_header() }),
        SectionLevel::Group => Box::new(bsn! { @group_header() }),
    }
}

/// The feathers container a body is drawn as at `level`, and the surface the controls inside it are drawn against,
/// so feathers can lift them off it.
fn body_container(level: SectionLevel) -> Box<dyn Scene> {
    match level {
        SectionLevel::Pane => Box::new(bsn! {
            @pane_body()
            // Feathers' pane body has no border of its own, which left an open pane's box without sides or a bottom
            // under its header.
            ThemeBorderColor({ tokens::PANE_HEADER_BORDER })
            Propagate::<ThemeContext>(ThemeContext(SurfaceLevel::Higher))
        }),
        SectionLevel::Group => Box::new(bsn! {
            @group_body()
            Propagate::<ThemeContext>(ThemeContext(SurfaceLevel::Highest))
        }),
    }
}

/// Which corners a header rounds. A closed section is a box on its own, so it rounds all four; an open one is the top
/// of the box its body finishes.
fn header_corners(open: bool) -> RoundedCorners {
    if open { RoundedCorners::Top } else { RoundedCorners::All }
}

/// Space kept at the end of a header, only once a control besides the toggle is in it. With none there the toggle
/// runs to the edge, so its hover is even on both sides.
fn header_padding(controls: usize) -> UiRect {
    if controls > 0 { UiRect::right(px(space::CONTROLS)) } else { UiRect::ZERO }
}

fn caret(open: bool) -> Icon {
    if open { Icon::ChevronDown } else { Icon::ChevronRight }
}

/// Show or hide a section's body, round its header, and turn its caret, as it opens or closes.
pub(crate) fn sync_accordions(
    accordions: Query<(&Accordion, &AccordionPieces), Changed<Accordion>>,
    children: Query<&Children>,
    carets: Query<(), With<AccordionCaret>>,
    mut nodes: Query<&mut Node>,
    mut texts: Query<&mut Text>,
) {
    for (accordion, pieces) in &accordions {
        let open = accordion.open;
        if let Ok(node) = nodes.get_mut(pieces.body) {
            patch_node(node, |node| node.display = display(open));
        }
        // The container, the toggle filling it and the outline over both all round the same corners; a closed
        // section has no body under it to finish the box.
        let corners = header_corners(open).to_border_radius(radius::CONTROL);
        for entity in [pieces.header, pieces.toggle, pieces.outline] {
            if let Ok(node) = nodes.get_mut(entity) {
                patch_node(node, |node| node.border_radius = corners);
            }
        }
        for child in children.get(pieces.toggle).into_iter().flatten() {
            if carets.contains(*child)
                && let Ok(text) = texts.get_mut(*child)
            {
                set_text(text, caret(open).glyph());
            }
        }
    }
}

/// Make room at the end of a header once a control is added beside its title.
#[allow(clippy::type_complexity)]
pub(crate) fn pad_accordion_headers(
    mut headers: Query<(&mut Node, &Children), (With<AccordionHeaderRow>, Changed<Children>)>,
) {
    for (node, children) in &mut headers {
        // Everything in the header but the toggle and its outline is a control.
        let wanted = header_padding(children.len().saturating_sub(2));
        patch_node(node, |node| node.padding = wanted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caret_shows_whether_a_section_is_open() {
        assert_ne!(caret(true), caret(false));
    }

    #[test]
    fn a_closed_header_rounds_the_corners_its_body_would_have_finished() {
        let open = header_corners(true).to_border_radius(radius::CONTROL);
        let closed = header_corners(false).to_border_radius(radius::CONTROL);
        // Open, the body under it carries the bottom of the box.
        assert_eq!(open.bottom_left, CornerRadius::from(px(0)));
        assert_eq!(open.top_left, CornerRadius::from(px(radius::CONTROL)));
        assert_eq!(closed.bottom_left, CornerRadius::from(px(radius::CONTROL)));
        assert_eq!(closed.top_left, CornerRadius::from(px(radius::CONTROL)));
    }

    #[test]
    fn a_header_with_only_its_toggle_leaves_no_gap_at_the_end() {
        assert_eq!(header_padding(0), UiRect::ZERO);
        assert_eq!(header_padding(1).right, px(space::CONTROLS));
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_systems(Update, (sync_accordions, pad_accordion_headers));
        app
    }

    #[test]
    fn opening_a_section_shows_its_body_and_turns_its_caret_without_respawning_anything() {
        let mut app = app();
        let parts = spawn_accordion(&mut app.world_mut().commands(), "Section", false, SectionLevel::Pane);
        app.world_mut().flush();
        app.update();
        let shown = |app: &App| app.world().get::<Node>(parts.body).unwrap().display;
        let caret_glyph = |app: &mut App| {
            let mut carets = app.world_mut().query_filtered::<&Text, With<AccordionCaret>>();
            carets.single(app.world()).unwrap().0.clone()
        };
        assert_eq!(shown(&app), Display::None);
        assert_eq!(caret_glyph(&mut app), caret(false).glyph());
        let before: Vec<Entity> = app.world_mut().query::<Entity>().iter(app.world()).collect();

        app.world_mut().get_mut::<Accordion>(parts.section).unwrap().open = true;
        app.update();
        assert_eq!(shown(&app), Display::Flex);
        assert_eq!(caret_glyph(&mut app), caret(true).glyph());
        let after: Vec<Entity> = app.world_mut().query::<Entity>().iter(app.world()).collect();
        assert_eq!(before, after, "toggling a section changes it in place");
    }

    #[test]
    fn a_closed_section_is_not_laid_out() {
        // A section spawned closed must be hidden from the start. Leaving it to the update showed its contents for a
        // frame first, which read as a flash of its contents whenever the panel was rebuilt.
        assert_eq!(display(false), Display::None);
        assert_eq!(display(true), Display::Flex);
    }
}

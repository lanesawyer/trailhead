//! A pressable surface that isn't a feathers button (a tab, a card, a hand-built row) answering the pointer as a
//! button does: its background turns to a hover tone while the pointer is over it or anything on it.

use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::InteractionDisabled;

use crate::theme::{BackgroundTone, Token};

/// The node's background at `rest`, and `hover` under the pointer. The hover tone has to show against the surface
/// the node sits on, as a plain button's does.
#[derive(Component, Clone, Default, PartialEq, Debug)]
#[require(Hovered)]
pub struct HoverTone {
    pub rest: Token,
    pub hover: Token,
}

/// Paint each surface whose hover changed in its tone. A disabled one stays at rest.
#[allow(clippy::type_complexity)]
pub(crate) fn paint_hover_tones(
    surfaces: Query<
        (Entity, &HoverTone, &Hovered, Has<InteractionDisabled>, Option<&BackgroundTone>),
        Or<(Changed<Hovered>, Changed<HoverTone>)>,
    >,
    mut commands: Commands,
) {
    for (surface, tone, hovered, disabled, shown) in &surfaces {
        let wanted = if hovered.0 && !disabled { &tone.hover } else { &tone.rest };
        if shown.is_none_or(|shown| shown.0 != *wanted) {
            commands.entity(surface).insert(BackgroundTone(wanted.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token;

    #[test]
    fn a_surface_takes_its_hover_tone_under_the_pointer_and_its_rest_tone_after() {
        let mut app = App::new();
        app.add_systems(Update, paint_hover_tones);
        let tone = HoverTone { rest: token::SURFACE_PANEL, hover: token::FILL_ITEM_HOVER };
        let surface = app.world_mut().spawn(tone).id();
        let shown = |app: &mut App, hovered: bool| {
            app.world_mut().entity_mut(surface).insert(Hovered(hovered));
            app.update();
            app.world().get::<BackgroundTone>(surface).unwrap().0.clone()
        };
        assert_eq!(shown(&mut app, false), token::SURFACE_PANEL);
        assert_eq!(shown(&mut app, true), token::FILL_ITEM_HOVER);
        assert_eq!(shown(&mut app, false), token::SURFACE_PANEL);
        app.world_mut().entity_mut(surface).insert(InteractionDisabled);
        assert_eq!(shown(&mut app, true), token::SURFACE_PANEL, "a disabled surface doesn't answer");
    }
}

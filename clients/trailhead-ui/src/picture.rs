//! A picture (a screenshot, say) shown fitted within a box with its caption under it, and a modal that shows one, or a
//! pair side by side, as large as the window lets it.
//!
//! - **The thumbnail.** [`spawn_picture`] spawns a box `extent::PICTURE_WIDTH` by `PICTURE_HEIGHT` and the caption
//!   under it, pressable as a whole: a press, Enter or Space sends `Activate` from the root, for the caller to open the
//!   large view. What the box shows is the [`Picture`] on its frame: [`Picture::Loading`] while the bytes are on their
//!   way, [`Picture::Ready`] with an image, or [`Picture::Failed`] with why. Set it with `set_if_neq`; the kit shows
//!   it and fits the image to the box, keeping its aspect ratio.
//! - **The large view.** [`open_picture_view`] opens a modal as wide and tall as a modal may be, holding one picture or
//!   two. Two are a pair (before and after): they stand side by side under their captions, and a group in the header
//!   shows either alone in the same place, so flipping between the two shows a small difference. Fit shrinks (or
//!   grows) each to its room; Actual size shows it pixel for pixel, scrolling both ways. Escape closes it, as every
//!   modal. Each picture's frame carries a [`PictureViewSlot`] with its index, so a picture whose bytes arrive after the
//!   view opened is set the same way as a thumbnail's.
//! - **From bytes.** [`png_image`] decodes a PNG into an [`Image`] (Bevy is built without its own image formats, so the
//!   kit uses the `png` crate); add it to `Assets<Image>` and hand its handle to [`Picture::Ready`] with its size.

use bevy::asset::RenderAssetUsages;
use bevy::input_focus::tab_navigation::TabIndex;
use bevy::picking::cursor::EntityCursor;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::ui_widgets::Button;
use bevy::window::SystemCursorIcon;

use crate::hover::HoverTone;
use crate::modal::{Modal, ModalSlots, open_modal};
use crate::scroll::ScrollBoth;
use crate::spacing::{extent, radius, space, stroke};
use crate::text::{size, text, text_dim};
use crate::theme::{BackgroundTone, BorderTone, TextTone, token};
use crate::{Picked, button_group, patch_node, set_display, set_text};

/// What a picture's frame shows. Set it with `set_if_neq`.
#[derive(Component, Clone, Debug, PartialEq, Default)]
pub enum Picture {
    /// The bytes are on their way.
    #[default]
    Loading,
    /// An image, and its size in pixels, which the frame fits it to.
    Ready { image: Handle<Image>, width: u32, height: u32 },
    /// It could not be shown, and why, in a few words.
    Failed(String),
}

/// How a frame sizes its image: fitted to the frame (the default), or pixel for pixel, scrolling when it is larger.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PictureFit {
    #[default]
    Fit,
    Actual,
}

/// The image inside a frame, which the kit sizes.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PictureImage;

/// The words a frame shows while it has no image: loading, or why it failed.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PictureStatus;

/// A thumbnail's caption.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PictureCaption;

/// A spawned thumbnail: put `root` where it goes and set the [`Picture`] on `frame`.
#[derive(Clone, Copy, Debug)]
pub struct PictureParts {
    pub root: Entity,
    pub frame: Entity,
}

/// Which of a large view's pictures a frame holds, from 0 in the order they were given.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PictureViewSlot(pub usize);

const LOADING: &str = "Loading…";

/// A frame holding `picture`: its image (hidden until there is one) and its status words.
fn spawn_frame(commands: &mut Commands, picture: Picture, node: Node) -> Entity {
    let frame = commands
        .spawn((node, picture, PictureFit::Fit, BackgroundTone(token::FILL_SOLID), BorderTone(token::BORDER)))
        .id();
    commands.spawn((
        Node { display: Display::None, margin: UiRect::all(Val::Auto), flex_shrink: 0.0, ..default() },
        ImageNode::default(),
        PictureImage,
        ChildOf(frame),
    ));
    commands.spawn_scene(text_dim(LOADING, size::SMALL)).insert((
        Node { margin: UiRect::all(Val::Auto), ..default() },
        PictureStatus,
        ChildOf(frame),
    ));
    frame
}

/// A thumbnail captioned `caption`, showing `picture`, spawned unparented.
pub fn spawn_picture(commands: &mut Commands, caption: impl Into<String>, picture: Picture) -> PictureParts {
    let root = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: px(space::STACKED),
                width: px(extent::PICTURE_WIDTH),
                flex_shrink: 0.0,
                border_radius: BorderRadius::all(px(radius::CONTROL)),
                ..default()
            },
            Button,
            HoverTone { rest: token::FILL_ITEM, hover: token::FILL_ITEM_HOVER },
            TabIndex(0),
            EntityCursor::System(SystemCursorIcon::Pointer),
        ))
        .id();
    let frame_node = Node {
        width: px(extent::PICTURE_WIDTH),
        height: px(extent::PICTURE_HEIGHT),
        border: UiRect::all(px(stroke::HAIRLINE)),
        border_radius: BorderRadius::all(px(radius::CONTROL)),
        overflow: Overflow::clip(),
        ..default()
    };
    let frame = spawn_frame(commands, picture, frame_node);
    commands.entity(frame).insert(ChildOf(root));
    commands.spawn_scene(text_dim(caption, size::SMALL)).insert((PictureCaption, ChildOf(root)));
    PictureParts { root, frame }
}

/// One picture of a large view.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewedPicture {
    pub caption: String,
    pub picture: Picture,
}

/// The large view's modal, holding the pictures it was opened with until it is filled.
#[derive(Component)]
pub struct PictureView {
    title: String,
    pictures: Vec<ViewedPicture>,
}

impl Modal for PictureView {
    fn title(&self) -> String {
        self.title.clone()
    }
    fn tall(&self) -> bool {
        true
    }
    fn wide(&self) -> bool {
        true
    }
}

/// A large view's group choosing which of a pair shows: both, or one alone.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct ShownGroup;

/// A large view's group choosing Fit or Actual size.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct FitGroup;

/// A large view's column for one picture: its caption over its frame.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct ViewPane(usize);

/// Opens the large view titled `title` on `pictures`: one, or two as a pair. Returns the modal's root.
pub fn open_picture_view(commands: &mut Commands, title: impl Into<String>, pictures: Vec<ViewedPicture>) -> Entity {
    open_modal(commands, PictureView { title: title.into(), pictures })
}

/// Which of `count` panes show for a group's choice: 0 is all of them, `n` the `n`th alone.
fn panes_shown(count: usize, chosen: usize) -> Vec<bool> {
    (0..count).map(|i| chosen == 0 || chosen == i + 1).collect()
}

/// The size an image `w` by `h` takes in `room`, keeping its aspect ratio.
pub(crate) fn fitted(image: Vec2, room: Vec2) -> Vec2 {
    if image.x <= 0.0 || image.y <= 0.0 || room.x <= 0.0 || room.y <= 0.0 {
        return Vec2::ZERO;
    }
    image * (room.x / image.x).min(room.y / image.y)
}

/// Fills a large view once its modal has spawned: the panes in the body, and the groups in the header.
pub(crate) fn fill_picture_views(
    views: Query<(Entity, &PictureView, &ModalSlots), Added<ModalSlots>>,
    mut commands: Commands,
) {
    for (root, view, slots) in &views {
        let row = commands
            .spawn((
                Node { flex_grow: 1.0, min_height: px(0), column_gap: px(space::GROUPS), ..default() },
                ChildOf(slots.body),
            ))
            .id();
        for (i, viewed) in view.pictures.iter().enumerate() {
            let pane = commands
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        flex_grow: 1.0,
                        flex_basis: px(0),
                        min_width: px(0),
                        min_height: px(0),
                        row_gap: px(space::STACKED),
                        ..default()
                    },
                    ViewPane(i),
                    ChildOf(row),
                ))
                .id();
            commands.spawn_scene(text(viewed.caption.clone(), size::BODY)).insert(ChildOf(pane));
            let node = Node {
                flex_grow: 1.0,
                flex_basis: px(0),
                min_width: px(0),
                min_height: px(0),
                border: UiRect::all(px(stroke::HAIRLINE)),
                border_radius: BorderRadius::all(px(radius::CONTROL)),
                overflow: Overflow::scroll(),
                ..default()
            };
            let frame = spawn_frame(&mut commands, viewed.picture.clone(), node);
            commands.entity(frame).insert((PictureViewSlot(i), ScrollBoth, ChildOf(pane)));
        }
        let fit = vec![("Fit".to_string(), None), ("Actual size".to_string(), None)];
        let fit = commands.spawn_scene(button_group(fit, Some(0))).insert(FitGroup).id();
        let mut tools = vec![fit];
        if view.pictures.len() == 2 {
            let mut options = vec![("Both".to_string(), None)];
            options.extend(view.pictures.iter().map(|p| (p.caption.clone(), None)));
            tools.insert(0, commands.spawn_scene(button_group(options, Some(0))).insert(ShownGroup).id());
        }
        commands.entity(slots.tools).insert_children(0, &tools);
        commands.entity(root).remove::<PictureView>().insert(PictureViewFilled);
    }
}

/// A large view that has been filled, so it isn't filled again.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct PictureViewFilled;

/// A pick in a large view's groups: which of the pair shows, or how they are sized.
pub(crate) fn on_view_pick(
    picked: On<Picked>,
    groups: Query<(Has<ShownGroup>, Has<FitGroup>)>,
    parents: Query<&ChildOf>,
    views: Query<(), With<PictureViewFilled>>,
    panes: Query<(Entity, &ViewPane)>,
    mut frames: Query<(Entity, &mut PictureFit), With<PictureViewSlot>>,
    mut nodes: Query<&mut Node>,
) {
    let group = picked.entity;
    let Some(view) = parents.iter_ancestors(group).find(|e| views.contains(*e)) else { return };
    let inside = |e: Entity| parents.iter_ancestors(e).any(|a| a == view);
    let Ok((shown, fit)) = groups.get(group) else { return };
    if shown {
        let ours: Vec<(Entity, usize)> = panes.iter().filter(|(e, _)| inside(*e)).map(|(e, p)| (e, p.0)).collect();
        let wanted = panes_shown(ours.len(), picked.index);
        for (pane, i) in ours {
            set_display(&mut nodes, pane, wanted.get(i).copied().unwrap_or(true));
        }
    } else if fit {
        let want = if picked.index == 0 { PictureFit::Fit } else { PictureFit::Actual };
        for (_, mut fit) in frames.iter_mut().filter(|(e, _)| inside(*e)) {
            fit.set_if_neq(want);
        }
    }
}

/// Shows each changed frame's [`Picture`]: its image, or its status words.
pub(crate) fn show_pictures(
    frames: Query<(&Picture, &Children), Changed<Picture>>,
    mut images: Query<&mut ImageNode, With<PictureImage>>,
    mut texts: Query<&mut Text, With<PictureStatus>>,
    status: Query<(), With<PictureStatus>>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    for (picture, children) in &frames {
        for &child in children {
            if let Ok(mut image) = images.get_mut(child) {
                if let Picture::Ready { image: handle, .. } = picture
                    && image.image != *handle
                {
                    image.image = handle.clone();
                }
                set_display(&mut nodes, child, matches!(picture, Picture::Ready { .. }));
            } else if status.contains(child) {
                let (words, tone) = match picture {
                    Picture::Loading => (LOADING, token::TEXT_DIM),
                    Picture::Failed(why) => (why.as_str(), token::STATUS_DANGER),
                    Picture::Ready { .. } => ("", token::TEXT_DIM),
                };
                if let Ok(text) = texts.get_mut(child) {
                    set_text(text, words);
                }
                commands.entity(child).insert(TextTone(tone));
                set_display(&mut nodes, child, !matches!(picture, Picture::Ready { .. }));
            }
        }
    }
}

/// Sizes each frame's image to the frame (keeping its aspect ratio) or pixel for pixel, from the frame's laid-out room.
pub(crate) fn fit_pictures(
    frames: Query<(&Picture, &PictureFit, &ComputedNode, &Children)>,
    mut nodes: Query<&mut Node, With<PictureImage>>,
) {
    for (picture, fit, computed, children) in &frames {
        let Picture::Ready { width, height, .. } = picture else { continue };
        let scale = computed.inverse_scale_factor();
        let content = computed.content_box().size() * scale;
        let natural = Vec2::new(*width as f32, *height as f32);
        let size = match fit {
            PictureFit::Fit => fitted(natural, content),
            PictureFit::Actual => natural * scale,
        };
        for &child in children {
            if let Ok(node) = nodes.get_mut(child) {
                patch_node(node, |n| {
                    n.width = px(size.x.floor());
                    n.height = px(size.y.floor());
                });
            }
        }
    }
}

/// A PNG's pixels as an image to add to `Assets<Image>`, kept only where it is drawn (not in the app's memory too), or
/// why it could not be read.
pub fn png_image(bytes: &[u8]) -> Result<Image, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| format!("not a PNG: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("too large to show")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("a broken PNG: {e}"))?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("an indexed PNG the decoder didn't expand".into()),
    };
    Ok(Image::new(
        Extent3d { width: info.width, height: info.height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().and_then(|mut w| w.write_image_data(data)).unwrap();
        bytes
    }

    #[test]
    fn a_png_decodes_to_rgba_whatever_its_color_type() {
        let rgb = png_image(&encode(2, 1, png::ColorType::Rgb, &[255, 0, 0, 0, 0, 255])).unwrap();
        assert_eq!(rgb.size(), UVec2::new(2, 1));
        assert_eq!(rgb.data.unwrap(), [255, 0, 0, 255, 0, 0, 255, 255]);
        let gray = png_image(&encode(1, 1, png::ColorType::Grayscale, &[9])).unwrap();
        assert_eq!(gray.data.unwrap(), [9, 9, 9, 255]);
        assert!(png_image(b"not a png").unwrap_err().starts_with("not a PNG"));
    }

    #[test]
    fn an_image_is_fitted_keeping_its_aspect_ratio() {
        assert_eq!(fitted(Vec2::new(1600.0, 1000.0), Vec2::new(160.0, 100.0)), Vec2::new(160.0, 100.0));
        assert_eq!(fitted(Vec2::new(1000.0, 1000.0), Vec2::new(160.0, 100.0)), Vec2::new(100.0, 100.0));
        assert_eq!(fitted(Vec2::new(40.0, 10.0), Vec2::new(160.0, 100.0)), Vec2::new(160.0, 40.0));
        assert_eq!(fitted(Vec2::new(40.0, 10.0), Vec2::ZERO), Vec2::ZERO);
    }

    #[test]
    fn a_pairs_choice_shows_both_or_one_alone() {
        assert_eq!(panes_shown(2, 0), [true, true]);
        assert_eq!(panes_shown(2, 1), [true, false]);
        assert_eq!(panes_shown(2, 2), [false, true]);
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_systems(Update, show_pictures);
        app
    }

    fn parts(app: &App, frame: Entity) -> (Entity, Entity) {
        let world = app.world();
        let children = world.get::<Children>(frame).unwrap();
        let image = children.iter().find(|c| world.get::<PictureImage>(*c).is_some()).unwrap();
        let status = children.iter().find(|c| world.get::<PictureStatus>(*c).is_some()).unwrap();
        (image, status)
    }

    fn shown(app: &App, e: Entity) -> bool {
        app.world().get::<Node>(e).unwrap().display != Display::None
    }

    #[test]
    fn a_frame_shows_loading_then_its_image_or_why_it_failed_in_place() {
        let mut app = app();
        let thumb = spawn_picture(&mut app.world_mut().commands(), "board-dark", Picture::Loading);
        app.world_mut().flush();
        app.update();
        let (image, status) = parts(&app, thumb.frame);
        assert_eq!(app.world().get::<Text>(status).unwrap().0, LOADING);
        assert!(shown(&app, status) && !shown(&app, image));

        let handle = app.world_mut().resource_mut::<Assets<Image>>().add(Image::default());
        let ready = Picture::Ready { image: handle.clone(), width: 320, height: 200 };
        *app.world_mut().get_mut::<Picture>(thumb.frame).unwrap() = ready;
        app.update();
        assert!(shown(&app, image) && !shown(&app, status));
        assert_eq!(app.world().get::<ImageNode>(image).unwrap().image, handle);

        *app.world_mut().get_mut::<Picture>(thumb.frame).unwrap() = Picture::Failed("not a PNG".into());
        app.update();
        assert_eq!(parts(&app, thumb.frame), (image, status), "the frame keeps its parts");
        assert_eq!(app.world().get::<Text>(status).unwrap().0, "not a PNG");
        assert_eq!(app.world().get::<TextTone>(status).unwrap().0, token::STATUS_DANGER);
        assert!(shown(&app, status) && !shown(&app, image));
    }

    fn view_app() -> App {
        let mut app = app();
        app.add_plugins(crate::ModalPlugin)
            .add_systems(Update, (fill_picture_views, crate::button_group::paint_groups))
            .add_observer(on_view_pick)
            .add_observer(crate::button_group::on_option);
        app.world_mut().spawn(Window::default());
        app
    }

    #[test]
    fn a_pair_opens_side_by_side_and_either_shows_alone_in_its_place() {
        let mut app = view_app();
        let pair = vec![
            ViewedPicture { caption: "Before".into(), picture: Picture::Loading },
            ViewedPicture { caption: "After".into(), picture: Picture::Failed("gone".into()) },
        ];
        let modal = open_picture_view(&mut app.world_mut().commands(), "board-dark", pair);
        app.world_mut().flush();
        for _ in 0..4 {
            app.update();
        }
        let mut slots: Vec<(usize, Picture)> = app
            .world_mut()
            .query::<(&PictureViewSlot, &Picture)>()
            .iter(app.world())
            .map(|(s, p)| (s.0, p.clone()))
            .collect();
        slots.sort_by_key(|(i, _)| *i);
        assert_eq!(slots, [(0, Picture::Loading), (1, Picture::Failed("gone".into()))]);
        let panes: Vec<(Entity, usize)> =
            app.world_mut().query::<(Entity, &ViewPane)>().iter(app.world()).map(|(e, p)| (e, p.0)).collect();
        let shown_panes = |app: &App| {
            let mut on: Vec<usize> = panes.iter().filter(|(e, _)| shown(app, *e)).map(|(_, i)| *i).collect();
            on.sort();
            on
        };
        assert_eq!(shown_panes(&app), [0, 1]);

        let group = app.world_mut().query_filtered::<Entity, With<ShownGroup>>().single(app.world()).unwrap();
        for (index, want) in [(2, vec![1]), (1, vec![0]), (0, vec![0, 1])] {
            app.world_mut().trigger(Picked { entity: group, index });
            app.update();
            assert_eq!(shown_panes(&app), want, "option {index}");
        }

        let fit = app.world_mut().query_filtered::<Entity, With<FitGroup>>().single(app.world()).unwrap();
        app.world_mut().trigger(Picked { entity: fit, index: 1 });
        app.update();
        let fits: Vec<PictureFit> = app.world_mut().query::<&PictureFit>().iter(app.world()).copied().collect();
        assert_eq!(fits, [PictureFit::Actual, PictureFit::Actual]);
        assert!(app.world().get::<PictureViewFilled>(modal).is_some());
    }

    #[test]
    fn one_picture_has_no_choice_of_which_shows() {
        let mut app = view_app();
        let one = vec![ViewedPicture { caption: "New".into(), picture: Picture::Loading }];
        open_picture_view(&mut app.world_mut().commands(), "board-dark", one);
        app.world_mut().flush();
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(app.world_mut().query::<&ShownGroup>().iter(app.world()).count(), 0);
        assert_eq!(app.world_mut().query::<&FitGroup>().iter(app.world()).count(), 1);
    }
}

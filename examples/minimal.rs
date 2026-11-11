//! The output-API demo: streaming generation around a `GenerationAnchor`,
//! with everything a consumer reads back — `RoomSpawned` to attach gameplay,
//! `DoorAbandoned` and `GenerationFailed` surfaced, `Layout`'s room count and
//! the point → room query on a HUD.
//!
//! Run: `cargo run --example minimal` (needs the devShell on NixOS — see
//! AGENTS.md).
//!
//! Controls: **WASD / arrows** move the camera (which is the anchor, so the
//! dungeon streams as you go), **R** regenerates the dungeon around you (**Shift+R**: same seed).
//!
//! The rest of the set: `testing` is the barest plugin wiring plus a debug
//! overlay, `halls` adds a player, wall collision and line of
//! sight in a hallways-only dungeon, `meta` adds meta-generation weights.

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::*;

mod common;

fn main() {
    // Fixed seed so the demo is the same dungeon every run — reproducible
    // layouts are the crate's main debugging affordance. R rerolls from a
    // random seed; pass a different `with_seed` to pin a new one.
    info!("minimal: seed 1234 — press R for a fresh dungeon");
    // No `insert_resource(LdtkSettings { .. })` here: GeneratorPlugin enforces
    // `UseWorldTranslation` and `load_level_neighbors: false` itself. See
    // ARCHITECTURE.md §2.
    App::new()
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .add_plugins(LdtkPlugin)
        .add_plugins(GeneratorPlugin::new(common::map_path(), 800.0).with_seed(1234))
        .add_systems(Startup, (spawn_camera, spawn_hud))
        .add_systems(
            Update,
            (
                common::move_with_wasd::<Camera>,
                common::regenerate_on_key,
                tint_intersection_rooms,
                warn_on_abandoned_doors,
                common::show_generation_failure,
                update_hud,
            ),
        )
        .run();
}

fn spawn_camera(mut commands: Commands) {
    commands.spawn((Camera2d, GenerationAnchor));
}

/// Rooms with at least this many doors get tinted: the junctions a minimap
/// would draw as intersections. Four of the bundled 44 rooms qualify (7 levels
/// have one door, 27 two, 6 three, 3 four, 1 six).
const INTERSECTION_DOORS: usize = 4;

/// `RoomSpawned` (`DESIGN.md` §4) is the seam for attaching gameplay to a
/// generated room: the entity exists, its tilemap is spawned, and the catalog
/// is readable.
///
/// Selection is by **door count**, not by tag. Tag-driven selection is the
/// same shape (`def.bool_tag(tag_id)`, with `tag_id` cached in a `Local` so
/// `Catalog::tag_id` resolves the name once instead of per event, per
/// `DESIGN.md` §2) — but the bundled catalog declares no `RoomTags` at all, so
/// a lookup on it could only ever miss and this example would silently attach
/// nothing.
///
/// The child sprite is positioned from the event's `at` (the placement origin,
/// in tiles), not as `size / 2` from the room entity: a room entity's own
/// translation cancels the level's LDtk world offset, so it is *not* the
/// room's min corner — `size / 2` from it lands the tint somewhere off in the
/// background. Tiles → pixels happens here, at the plugin boundary, never
/// inside `generator/` (`ARCHITECTURE.md` §2).
#[allow(clippy::cast_precision_loss)] // room sizes are tiny; exact in f32
fn tint_intersection_rooms(
    mut spawned: MessageReader<RoomSpawned>,
    rooms: Query<&Transform>,
    catalog: Res<CatalogRes>,
    mut commands: Commands,
) {
    let grid_size = catalog.grid_size() as f32;

    for event in spawned.read() {
        let Some(def) = catalog.get(event.room) else {
            continue;
        };
        if def.doors.len() < INTERSECTION_DOORS {
            continue;
        }
        let Ok(room_transform) = rooms.get(event.entity) else {
            continue;
        };
        let size = Vec2::new(def.size.width as f32, def.size.height as f32) * grid_size;
        let min = Vec2::new(event.at.x as f32, event.at.y as f32) * grid_size;
        let local = min + size / 2.0 - room_transform.translation.truncate();
        commands.entity(event.entity).with_children(|parent| {
            parent.spawn((
                Sprite::from_color(Color::srgba(0.25, 0.55, 1.0, 0.15), size),
                Transform::from_xyz(local.x, local.y, 1.0),
            ));
        });
    }
}

/// A dead door is a permanent visible gap (`DESIGN.md` §9, "Dead doors") —
/// a real game walls it off; this example just surfaces that it happened.
fn warn_on_abandoned_doors(mut abandoned: MessageReader<DoorAbandoned>) {
    for event in abandoned.read() {
        warn!(
            "door at {:?} facing {:?} could not be filled",
            event.at, event.dir
        );
    }
}

#[derive(Component)]
struct HudText;

fn spawn_hud(mut commands: Commands) {
    commands.spawn((
        Text::new("state: -\nrooms: -\nhere: -"),
        TextFont {
            font_size: FontSize::Px(18.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            left: Val::Px(10.0),
            ..default()
        },
        HudText,
    ));
}

/// Exercises the two outputs `DESIGN.md` §4 calls out as needed "constantly":
/// `Layout`'s room count, and the point -> room query, here against the
/// anchor's own position rather than the player's.
///
/// `Layout::room_at` takes a `TilePos` (`generator/` is tiles-only), so the
/// anchor's pixel `Transform` is converted via `Catalog::grid_size` first —
/// that conversion belongs here, at the plugin boundary, not inside `generator/`.
fn update_hud(
    state: Res<State<GenerationState>>,
    catalog: Res<CatalogRes>,
    layout: Res<LayoutRes>,
    anchor: Query<&GlobalTransform, With<GenerationAnchor>>,
    mut hud: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };

    let grid_size = catalog.grid_size() as f32;
    let here = anchor
        .single()
        .ok()
        .map(|t| t.translation().truncate())
        .map(|px| TilePos {
            x: (px.x / grid_size).floor() as i32,
            y: (px.y / grid_size).floor() as i32,
        })
        .and_then(|tile| layout.room_at(&catalog, tile))
        .and_then(|id| catalog.get(id))
        .map_or_else(|| "-".to_string(), |def| format!("{:?}", def.iid));

    text.0 = format!(
        "state: {:?}\nrooms: {}\nhere: {here}",
        state.get(),
        layout.placed_count(),
    );
}

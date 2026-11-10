//! The playable demo: a player that collides with the walls of the rooms
//! around it, a follow camera, and visibility limited by both range and line
//! of sight — the two blending into a single fade rather than a hard cut.
//!
//! Run: `cargo run --example full` (needs the devShell on NixOS — see
//! AGENTS.md).
//!
//! Controls:
//! - **WASD / arrows** — move the player, who is a `GenerationAnchor`, so the
//!   dungeon streams in ahead of you and is culled behind you
//! - **R** — throw the dungeon away and generate a new one around you
//! - **C** — toggle wall collision (physics is a *consumer* concern — the
//!   library never reads the wall `IntGrid`; this example is the consumer,
//!   see DESIGN.md §10). With collision off every wall cell's debug box turns
//!   red, so the noclip is visible as well as felt.
//!
//! Collision is deliberately physics-free: walls are `IntGrid` value 1 cells
//! registered through `bevy_ecs_ldtk`, resolved as simple AABB pushes against
//! the player. Swap that system for `bevy_xpbd`/`avian` colliders in a real
//! game — the wall registration half stays identical.
//!
//! ## Visibility
//!
//! Each spawned room gets a black overlay sized to its footprint. Every frame
//! the overlay's alpha is `1 - visible`, where
//!
//! ```text
//! visible = range(distance) * sight(line of sight)
//! ```
//!
//! `range` is 1 while the room is within `FOG_RADIUS` and ramps to 0 over the
//! next `FOG_FADE` pixels, so the edge of the lit area falls off instead of
//! cutting. `sight` is 1 when a ray from the player reaches the room, and
//! `FOG_BLOCKED` (not 0) when a wall is in the way. Multiplying them is what
//! makes the two limits *one* gradient: a near room behind a wall dims rather
//! than vanishing, and a far room in the open fades rather than popping.
//!
//! Thin gaps matter: ray-marching a single centre line through a two-tile
//! doorway misses more often than it hits, so three rays are cast, fanned
//! across a third of a tile. Line of sight is per **room**, not per tile —
//! fine for a demo, and the overlay is per room precisely so it stays cheap.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::*;

mod common;

/// Inside this radius (px) a room is fully visible.
const FOG_RADIUS: f32 = 480.0;

/// Beyond `FOG_RADIUS`, visibility ramps to nothing over this many px.
const FOG_FADE: f32 = 400.0;

/// What a room keeps when range would show it but a wall is in the way. Not
/// zero: a solid black wall of fog reads as "the world ended", where a dim
/// room reads as "you remember it".
const FOG_BLOCKED: f32 = 0.10;

const FOG_Z: f32 = 10.0;
const PLAYER_HALF_EXTENT: f32 = 6.0;
const PLAYER_Z: f32 = 30.0;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .add_plugins(LdtkPlugin)
        .add_plugins(
            GeneratorPlugin::new(
                "ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk",
                700.0,
            )
            .with_seed(424242),
        )
        // Walls: IntGrid value 1 on the "Walls" layer becomes a `Wall`
        // entity per cell. This registration is the entire collision setup
        // the library requires — everything else below is example-side.
        .register_ldtk_int_cell_for_layer::<Wall>("Walls", 1)
        .init_resource::<CollisionOn>()
        .add_systems(Startup, spawn)
        // Chained, not just grouped: all three write the player's `Transform`
        // or read it, and Bevy is free to order conflicting systems either
        // way round. Moving → colliding → following → fading is the order the
        // frame is meant to happen in.
        .add_systems(
            Update,
            (
                common::move_with_wasd::<Player>,
                toggleable_collision,
                common::camera_follow::<Player>,
                update_fog,
            )
                .chain(),
        )
        .add_systems(
            Update,
            (
                common::regenerate_on_key,
                spawn_fog,
                update_hud,
                warn_on_abandoned_doors,
            ),
        )
        .run();
}

/// Marker for one wall cell (`IntGrid` value 1 on the "Walls" layer), via
/// `register_ldtk_int_cell_for_layer`.
#[derive(Default, Component)]
struct Wall;

impl LdtkIntCell for Wall {
    fn bundle_int_cell(_int_grid_cell: IntGridCell, _layer_instance: &LayerInstance) -> Self {
        Wall
    }
}

#[derive(Component)]
struct Player;

fn spawn(mut commands: Commands) {
    // The player IS the anchor: generation follows it, so walking toward the
    // frontier fills doors ahead of you and culling despawns behind. The
    // camera is anchored too, so the culling radius stays centred on the view.
    commands.spawn((
        Sprite {
            color: Color::srgb(0.9, 0.3, 0.3),
            custom_size: Some(Vec2::splat(PLAYER_HALF_EXTENT * 2.0)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, PLAYER_Z),
        Player,
        GenerationAnchor,
    ));
    commands.spawn((Camera2d, GenerationAnchor));
    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: 16.0,
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

/// Whether collision is currently applied. A resource so the HUD and the
/// debug view of the walls can reflect the same state.
#[derive(Resource)]
struct CollisionOn(bool);

impl Default for CollisionOn {
    fn default() -> Self {
        Self(true)
    }
}

/// Axis-separated AABB resolution against wall cells: move on X, push out,
/// move on Y, push out. Removing this system (or `C` in-game) disables
/// collision entirely — walls remain visible, the player just passes through.
///
/// Walls are the registered `IntGrid` tile entities; `GlobalTransform`
/// propagation through the level → layer → tile hierarchy gives each one's
/// world position, so this system needs no hierarchy math at all.
fn toggleable_collision(
    mut collision_on: ResMut<CollisionOn>,
    keys: Res<ButtonInput<KeyCode>>,
    catalog: Res<CatalogRes>,
    mut player: Query<&mut Transform, (With<Player>, Without<Wall>)>,
    walls: Query<&GlobalTransform, (With<Wall>, Without<Player>)>,
    mut gizmos: Gizmos,
) {
    if keys.just_pressed(KeyCode::KeyC) {
        collision_on.0 = !collision_on.0;
    }

    let cell = catalog.grid_size() as f32;
    let cell_half = cell / 2.0;

    // Player centre before this frame's collision resolution — the debug
    // colour should say where the player actually is, not where it is about
    // to be pushed to.
    let player_pos = player
        .single()
        .map(|t| t.translation.xy())
        .unwrap_or_default();

    // Debug view either way: outlined boxes when colliding, red when the
    // player is currently clipping through them.
    for wall in walls.iter() {
        let center = wall.translation().xy();
        let clipping = (center - player_pos)
            .abs()
            .cmplt(Vec2::splat(PLAYER_HALF_EXTENT + cell_half))
            .all();
        gizmos.rect_2d(
            center,
            Vec2::splat(cell),
            if !collision_on.0 || clipping {
                Color::srgb(1.0, 0.2, 0.2)
            } else {
                Color::srgba(0.3, 0.3, 0.3, 0.6)
            },
        );
    }

    if !collision_on.0 {
        return;
    }

    let Ok(mut transform) = player.single_mut() else {
        return;
    };

    // Resolve the axis just moved, not both at once: walking into a wall
    // must still let you slide along it.
    let resolve = |translation: &mut Vec3, axis: usize| {
        let half = Vec2::splat(PLAYER_HALF_EXTENT);
        for wall in walls.iter() {
            let center = wall.translation().xy();
            let overlap = (center - translation.xy()).abs() - (half + cell_half);
            if overlap.x < 0.0 && overlap.y < 0.0 {
                let push = if axis == 0 { overlap.x } else { overlap.y };
                if axis == 0 {
                    translation.x -= push * f32::signum(translation.x - center.x);
                } else {
                    translation.y -= push * f32::signum(translation.y - center.y);
                }
            }
        }
    };

    resolve(&mut transform.translation, 0);
    resolve(&mut transform.translation, 1);
}

/// The fog overlay for one room: a black sprite sized to the room's
/// footprint, alpha driven by [`update_fog`].
#[derive(Component)]
struct Fog {
    center: Vec2,
    size: Vec2,
}

/// One overlay per room, spawned as a child of the room entity — so culling a
/// room away takes its fog with it and nothing needs a cleanup pass.
///
/// The child's local offset is not simply `size / 2`: a room root's
/// translation is chosen so the *level content's* min corner lands on the
/// placement origin, which is offset from the root itself. Using the event's
/// `at` (the placement origin in tiles) and the root's own translation keeps
/// that arithmetic in one place instead of duplicating the level-offset
/// formula.
#[allow(clippy::cast_precision_loss)] // room sizes are tiny, exact in f32
fn spawn_fog(
    mut spawned: MessageReader<RoomSpawned>,
    rooms: Query<&Transform>,
    catalog: Res<CatalogRes>,
    mut commands: Commands,
) {
    let grid = catalog.grid_size() as f32;

    for event in spawned.read() {
        let Some(def) = catalog.get(event.room) else {
            continue;
        };
        let Ok(room_transform) = rooms.get(event.entity) else {
            continue;
        };

        let size = Vec2::new(def.size.width as f32, def.size.height as f32) * grid;
        let min = Vec2::new(event.at.x as f32, event.at.y as f32) * grid;
        let center = min + size / 2.0;
        let local = center - room_transform.translation.truncate();

        commands.entity(event.entity).with_children(|parent| {
            parent.spawn((
                Sprite::from_color(Color::BLACK, size),
                Transform::from_xyz(local.x, local.y, FOG_Z),
                Fog { center, size },
            ));
        });
    }
}

/// Recomputes every overlay's alpha from the player's position — the blend of
/// the two limits described in this file's header.
#[allow(clippy::cast_precision_loss)] // positions are px; f32 is the point
fn update_fog(
    player: Query<&Transform, With<Player>>,
    walls: Query<&GlobalTransform, With<Wall>>,
    mut fog: Query<(&Fog, &mut Sprite, &mut Visibility)>,
    catalog: Res<CatalogRes>,
) {
    let Ok(player) = player.single() else {
        return;
    };
    let from = player.translation.xy();
    let grid = catalog.grid_size().max(1) as f32;
    let blocked = wall_cells(&walls, grid);

    for (fog, mut sprite, mut visibility) in fog.iter_mut() {
        // Distance to the room's *nearest point*, not its centre: standing at
        // a doorway should light both rooms, not neither.
        let min = fog.center - fog.size / 2.0;
        let max = fog.center + fog.size / 2.0;
        let distance = (from.clamp(min, max) - from).length();

        let range = if distance <= FOG_RADIUS {
            1.0
        } else {
            1.0 - (distance - FOG_RADIUS) / FOG_FADE
        };
        let sight = if clear_line(from, fog.center, &blocked, grid) {
            1.0
        } else {
            FOG_BLOCKED
        };
        let visible = (range.clamp(0.0, 1.0) * sight).clamp(0.0, 1.0);

        sprite.color = Color::srgba(0.0, 0.0, 0.0, 1.0 - visible);
        *visibility = if visible <= 0.01 {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
    }
}

/// Wall cell coordinates, rebuilt each frame. Cheap at the scale this example
/// keeps live (a few hundred cells inside the culling radius), and rebuilding
/// avoids a stale cell surviving a room being culled.
#[allow(clippy::cast_possible_truncation)] // world px / grid is small
fn wall_cells(walls: &Query<&GlobalTransform, With<Wall>>, grid: f32) -> HashSet<(i32, i32)> {
    walls
        .iter()
        .map(|wall| cell_of(wall.translation().xy(), grid))
        .collect()
}

/// Three rays, fanned across a third of a tile: a doorway is two tiles wide,
/// and a single centre line squeezes through it far too rarely to look right.
fn clear_line(from: Vec2, to: Vec2, blocked: &HashSet<(i32, i32)>, grid: f32) -> bool {
    let along = to - from;
    let across = Vec2::new(-along.y, along.x).normalize_or_zero() * grid * 0.35;
    [Vec2::ZERO, across, -across]
        .iter()
        .any(|offset| ray_clear(from + *offset, to + *offset, blocked, grid))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // small tile counts
fn ray_clear(from: Vec2, to: Vec2, blocked: &HashSet<(i32, i32)>, grid: f32) -> bool {
    let along = to - from;
    let steps = (along.length() / (grid * 0.5)).ceil().max(1.0) as usize;
    (1..steps).all(|step| {
        let t = step as f32 / steps as f32;
        !blocked.contains(&cell_of(from + along * t, grid))
    })
}

#[allow(clippy::cast_possible_truncation)] // world px / grid is small
fn cell_of(point: Vec2, grid: f32) -> (i32, i32) {
    (
        (point.x / grid).floor() as i32,
        (point.y / grid).floor() as i32,
    )
}

#[derive(Component)]
struct HudText;

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn update_hud(
    collision_on: Res<CollisionOn>,
    state: Res<State<GenerationState>>,
    layout: Res<LayoutRes>,
    player: Query<&Transform, With<Player>>,
    mut hud: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };
    let pos = player
        .single()
        .map(|t| t.translation.xy().round().as_ivec2())
        .unwrap_or_default();

    text.0 = format!(
        "state: {:?}  rooms: {}  pos: {}:{}\n[C] collision: {}   WASD move · R regenerate",
        state.get(),
        layout.placed_count(),
        pos.x,
        pos.y,
        if collision_on.0 { "ON" } else { "OFF" },
    );
}

/// A dead door is a permanent visible gap (DESIGN.md §9) — a real game walls
/// it off; this example just surfaces that it happened.
fn warn_on_abandoned_doors(mut abandoned: MessageReader<DoorAbandoned>) {
    for event in abandoned.read() {
        warn!(
            "door at {:?} facing {:?} could not be filled",
            event.at, event.dir
        );
    }
}

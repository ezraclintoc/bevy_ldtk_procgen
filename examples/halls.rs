//! A dungeon made only of hallways: the catalog is filtered down to its
//! corridor-sized rooms by a consumer weight table, so the player walks a
//! branching maze of corridors, colliding with the walls.
//!
//! Run: `cargo run --example halls` (needs the devShell on NixOS — see
//! AGENTS.md). Pass a project path to try another map:
//! `cargo run --example halls -- other.ldtk`.
//!
//! ## Halls only
//!
//! Selection is a weight table (`DESIGN.md` §9) — `common::hall_weights`,
//! shared with `testing`'s "halls only" toggle: corridor-sized rooms only,
//! junctions favoured and dead ends least likely, so the maze branches and
//! keeps growing. The engine gives no room any meaning; this filter is
//! entirely the consumer's.
//!
//! The clear colour is the first pixel of the project's first level (its
//! top-left tile, read from the tileset image), so the space between and
//! around rooms matches the rooms' own outer edge.

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::*;

mod common;

/// With collision off, wall cells within this many px of the player are
/// outlined red.
const NOCLIP_OUTLINE_RADIUS: f32 = 200.0;

const PLAYER_HALF_EXTENT: f32 = 6.0;
const PLAYER_Z: f32 = 30.0;

fn main() {
    // Fixed seed so the room you start in, and the first few doors, are the
    // same on every run — useful when checking collision or visibility against
    // known geometry. R rerolls from a random seed.
    info!("halls: seed 424242 — press R for a fresh dungeon");
    App::new()
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .add_plugins(LdtkPlugin)
        .add_plugins(GeneratorPlugin::new(common::map_path(), 700.0).with_seed(424242))
        // Walls: IntGrid value 1 becomes a `Wall` entity per cell. Any layer,
        // not a named one: the bundled projects call their wall layer
        // `WallGrid` and `IntGrid`, and a name that matches neither silently
        // registers nothing — and so no collision.
        // This registration is the entire collision setup the library
        // requires — everything else below is example-side.
        .register_ldtk_int_cell::<Wall>(1)
        .init_resource::<CollisionOn>()
        .init_resource::<Background>()
        .add_systems(
            Update,
            halls_only
                .pipe(apply_weights)
                .in_set(GenerationSet::Weights),
        )
        .add_systems(Startup, spawn)
        // Chained: the camera follows where collision left the player.
        .add_systems(
            Update,
            (move_and_collide, common::camera_follow::<Player>).chain(),
        )
        .add_systems(
            Update,
            (
                common::regenerate_on_key,
                common::show_generation_failure,
                match_background,
                update_hud,
                warn_on_abandoned_doors,
            ),
        )
        .run();
}

/// Every frame's weight table — the catalog never changes, but the system
/// is the seam `DESIGN.md` §9 defines, so it is used as-is.
fn halls_only(catalog: Res<CatalogRes>) -> Vec<f32> {
    common::hall_weights(&catalog)
}

/// Marker for one wall cell (`IntGrid` value 1 on any layer), via
/// `register_ldtk_int_cell`.
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
    // The player IS the anchor — the only one: generation and culling both
    // follow it. (The camera trails behind on a lerp; anchoring it too would
    // centre culling on a point that lags the player.)
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
    commands.spawn(Camera2d);
    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(16.0),
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

/// The colour of the first pixel of the first level. `None` until the
/// project and its tileset image have loaded.
#[derive(Resource, Default)]
struct Background(Option<Color>);

/// Resolves [`Background`] once, and sets the clear colour to match.
///
/// "First pixel of a level" is the top-left pixel as drawn: the top-most
/// layer with a tile at the level's origin, sampled from that layer's
/// tileset at the tile's source (flips honoured). A project with no tiles
/// there — e.g. the `_notileset` fixture — falls back to the level's own
/// background colour.
#[allow(clippy::cast_sign_loss)] // LDtk tile sources are non-negative px
fn match_background(
    projects: Res<Assets<LdtkProject>>,
    images: Res<Assets<Image>>,
    mut background: ResMut<Background>,
    mut clear: ResMut<ClearColor>,
) {
    if background.0.is_some() {
        return;
    }
    let Some((_, project)) = projects.iter().next() else {
        return;
    };
    let json = project.json_data();
    let Some(level) = json.levels.first() else {
        return;
    };
    let grid = json.default_grid_size.max(1);

    let first_tile = level.layer_instances.iter().flatten().find_map(|layer| {
        let tile = layer
            .grid_tiles
            .iter()
            .chain(&layer.auto_layer_tiles)
            .find(|t| t.px == IVec2::ZERO)?;
        Some((layer.tileset_def_uid?, tile))
    });
    let color = match first_tile {
        Some((uid, tile)) => {
            let Some(image) = project
                .tileset_map()
                .get(&uid)
                .and_then(|handle| images.get(handle))
            else {
                return; // tileset still loading — try next frame
            };
            // Bit 0 of `f` flips X, bit 1 flips Y: the drawn top-left pixel
            // is then the source's opposite edge.
            let x = tile.src.x + if tile.f & 1 != 0 { grid - 1 } else { 0 };
            let y = tile.src.y + if tile.f & 2 != 0 { grid - 1 } else { 0 };
            image
                .get_color_at(x as u32, y as u32)
                .unwrap_or(level.bg_color)
        }
        None => level.bg_color,
    };
    let color = color.with_alpha(1.0);
    background.0 = Some(color);
    clear.0 = color;
}

/// Whether collision is currently applied. A resource so the HUD can
/// reflect the same state.
#[derive(Resource)]
struct CollisionOn(bool);

impl Default for CollisionOn {
    fn default() -> Self {
        Self(true)
    }
}

/// Movement with axis-separated AABB collision against wall cells: move on
/// X and push out, then move on Y and push out. Each pass only ever pushes
/// back against the direction just moved, to the face of the wall — never
/// sideways — so walking into a wall stops you and lets you slide along it,
/// and a wall that only overlaps because of the *other* axis's motion can't
/// shove you through a room. `C` disables the resolution: walls stay visible,
/// the player passes through.
///
/// Walls are the registered `IntGrid` tile entities; `GlobalTransform`
/// propagation through the level → layer → tile hierarchy gives each one's
/// world position, so this system needs no hierarchy math at all.
#[allow(clippy::too_many_arguments)] // a Bevy system's parameters are its inputs
fn move_and_collide(
    mut collision_on: ResMut<CollisionOn>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    catalog: Res<CatalogRes>,
    mut player: Query<&mut Transform, (With<Player>, Without<Wall>)>,
    walls: Query<&GlobalTransform, (With<Wall>, Without<Player>)>,
    mut gizmos: Gizmos,
) {
    if keys.just_pressed(KeyCode::KeyC) {
        collision_on.0 = !collision_on.0;
    }
    let Ok(mut transform) = player.single_mut() else {
        return;
    };

    let cell = catalog.grid_size() as f32;
    let reach = Vec2::splat(PLAYER_HALF_EXTENT + cell / 2.0);
    let delta = common::wasd_direction(&keys) * common::WALK_SPEED * time.delta_secs();
    let mut pos = transform.translation.xy();

    for axis in [0, 1] {
        let moved = delta[axis];
        pos[axis] += moved;
        if !collision_on.0 || moved.abs() < f32::EPSILON {
            continue;
        }
        for wall in walls.iter() {
            let center = wall.translation().xy();
            // A hair of slack: after a push the player sits exactly on a
            // wall's face, and rounding must not count that as overlapping
            // when the other axis is resolved next.
            let overlapping = ((center - pos).abs() - reach)
                .cmplt(Vec2::splat(-0.01))
                .all();
            if overlapping {
                pos[axis] = if moved > 0.0 {
                    pos[axis].min(center[axis] - reach[axis])
                } else {
                    pos[axis].max(center[axis] + reach[axis])
                };
            }
        }
    }
    transform.translation = pos.extend(transform.translation.z);

    // Noclip made visible: with collision off, outline the wall cells near
    // the player in red.
    if !collision_on.0 {
        for wall in walls.iter() {
            let center = wall.translation().xy();
            if center.distance(pos) < NOCLIP_OUTLINE_RADIUS {
                gizmos.rect_2d(center, Vec2::splat(cell), Color::srgb(1.0, 0.2, 0.2));
            }
        }
    }
}

#[derive(Component)]
struct HudText;

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn update_hud(
    collision_on: Res<CollisionOn>,
    state: Res<State<GenerationState>>,
    layout: Res<LayoutRes>,
    player: Query<&Transform, With<Player>>,
    rooms: Query<(), With<LevelIid>>,
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
        "state: {:?}  rooms: {} placed, {} loaded  pos: {}:{}\n\
         [C] collision: {}   WASD move · R regenerate · Shift+R same seed",
        state.get(),
        layout.placed_count(),
        rooms.iter().count(),
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

//! The full demo: streaming generation with a player you actually move,
//! toggleable wall collision, a following camera, and the generation
//! events surfaced on screen.
//!
//! Run: `cargo run --example full` (needs the devShell on NixOS — see
//! AGENTS.md).
//!
//! Controls:
//! - **WASD / arrows** — move the player (a `GenerationAnchor`, so the
//!   dungeon streams in around you)
//! - **C** — toggle wall collision (physics is a *consumer* concern —
//!   the library never reads the wall IntGrid; this example is the
//!   consumer, see DESIGN.md §10)
//! - **G** — toggle noclipping through the HUD's eyes (same toggle,
//!   different view: collision OFF turns wall colliders' debug color red)
//!
//! Collision is deliberately physics-free: walls are `IntGrid` value 1
//! cells registered through `bevy_ecs_ldtk`, resolved as simple AABB pushes
//! against the player. Swap this system for bevy_xpbd/avian colliders in a
//! real game — the wall registration half stays identical.

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::*;

const PLAYER_SPEED: f32 = 220.0;
const PLAYER_HALF_EXTENT: f32 = 6.0;

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
        .add_systems(Startup, (spawn_player, spawn_hud))
        .add_systems(
            Update,
            (
                player_movement,
                toggleable_collision,
                camera_follow,
                update_hud,
                warn_on_abandoned_doors,
            ),
        )
        .run();
}

/// Marker for one wall cell (IntGrid value 1 on the "Walls" layer), via
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

fn spawn_player(mut commands: Commands) {
    // The player IS the anchor: generation follows it, so walking toward
    // the frontier fills doors ahead of you and culling despawns behind.
    commands.spawn((
        Sprite {
            color: Color::srgb(0.9, 0.3, 0.3),
            custom_size: Some(Vec2::splat(PLAYER_HALF_EXTENT * 2.0)),
            ..default()
        },
        Transform::default(),
        Player,
        GenerationAnchor,
    ));
    commands.spawn((Camera2d, GenerationAnchor));
}

fn player_movement(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut player: Query<&mut Transform, With<Player>>,
) {
    let Ok(mut transform) = player.single_mut() else {
        return;
    };

    let mut direction = Vec2::ZERO;
    if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
        direction.y += 1.0;
    }
    if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
        direction.y -= 1.0;
    }
    if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
        direction.x -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
        direction.x += 1.0;
    }
    if direction == Vec2::ZERO {
        return;
    }

    transform.translation += direction.normalize().extend(0.0) * PLAYER_SPEED * time.delta_secs();
}

/// Whether collision is currently applied. Lives in a resource so the HUD
/// and the debug view of the walls can reflect the same state.
#[derive(Resource)]
struct CollisionOn(bool);

/// Axis-separated AABB resolution against wall cells: move on X, push out,
/// move on Y, push out. Removing this system (or `C` in-game) disables
/// collision entirely — walls remain visible, the player just passes
/// through.
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

    // Debug view either way: outlined boxes when colliding, red when the
    // player is currently clipping through them.
    for wall in walls.iter() {
        let center = wall.translation().xy();
        let clipping = (center - player_start(&player))
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
    let mut resolve = |translation: &mut Vec3, axis: usize| {
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

fn player_start(player: &Query<&mut Transform, (With<Player>, Without<Wall>)>) -> Vec2 {
    player
        .single()
        .map(|t| t.translation.xy())
        .unwrap_or_default()
}

fn camera_follow(
    player: Query<&Transform, With<Player>>,
    mut camera: Query<&mut Transform, (With<Camera>, Without<Player>)>,
) {
    let Ok(player) = player.single() else {
        return;
    };
    for mut transform in camera.iter_mut() {
        transform.translation = transform.translation.lerp(
            player.translation,
            // Frame-rate independent-ish smoothing.
            0.12,
        );
    }
}

#[derive(Component)]
struct HudText;

fn spawn_hud(mut commands: Commands) {
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
    commands.insert_resource(CollisionOn(true));
}

fn update_hud(
    mut collision_on: ResMut<CollisionOn>,
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
        "state: {:?}  rooms: {}  pos: {}:{}  [C] collision: {}",
        state.get(),
        layout.placed_count(),
        pos.x,
        pos.y,
        if collision_on.0 { "ON" } else { "OFF" },
    );
}

/// A dead door is a permanent visible gap (DESIGN.md §9) — a real game
/// walls it off; this example just surfaces that it happened.
fn warn_on_abandoned_doors(mut abandoned: MessageReader<DoorAbandoned>) {
    for event in abandoned.read() {
        warn!(
            "door at {:?} facing {:?} could not be filled",
            event.at, event.dir
        );
    }
}

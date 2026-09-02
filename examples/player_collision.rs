//! A sprite that walks the generated dungeon and collides with its walls.
//!
//! Walls come from the room set's IntGrid layer: every cell of value 1 is registered as
//! a `Wall` entity, and movement is resolved one axis at a time against their bounds.
//!
//! Controls: WASD or arrow keys.

use bevy::image::ImagePlugin;
use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::{WorldPlugin, WorldState};

mod common;
use common::{RoomSet, TINY_DUNGEON};

const ROOM_SET: RoomSet = TINY_DUNGEON;

const PLAYER_SPEED: f32 = 120.0;
const PLAYER_HALF_EXTENT: f32 = 5.0;
const CAMERA_LERP: f32 = 8.0;

/// Small enough to keep the per-frame wall scan cheap; the pan_camera example
/// shows the generator running without this cap.
const MAX_ROOMS: usize = 60;

#[derive(Component)]
struct Player;

#[derive(Default, Component)]
struct Wall;

#[derive(Default, Bundle, LdtkIntCell)]
struct WallBundle {
    wall: Wall,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .add_plugins(LdtkPlugin)
        .add_plugins(WorldPlugin {
            ldtk_path: ROOM_SET.ldtk_path.into(),
            max_rooms: MAX_ROOMS,
            ..default()
        })
        .register_ldtk_int_cell_for_layer::<WallBundle>(ROOM_SET.wall_layer, 1)
        .insert_resource(LdtkSettings {
            level_spawn_behavior: LevelSpawnBehavior::UseWorldTranslation {
                load_level_neighbors: false,
            },
            ..default()
        })
        .add_systems(Startup, setup)
        .add_systems(Update, (spawn_player, move_player, follow_player).chain())
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn((Camera2d, Projection::from(OrthographicProjection {
        scale: 0.35,
        ..OrthographicProjection::default_2d()
    })));
}

/// Waits for the generator to place the spawn room, then drops the player in its centre.
/// `world_pos` is the room's top-left with y increasing upwards.
fn spawn_player(
    mut commands: Commands,
    world_state: Res<WorldState>,
    existing: Query<(), With<Player>>,
) {
    if !existing.is_empty() {
        return;
    }
    let Some(spawn_room) = world_state.rooms.first() else {
        return;
    };

    let size = spawn_room.room.size.as_vec2();
    let centre = spawn_room.world_pos + Vec2::new(size.x / 2.0, -size.y / 2.0);

    commands.spawn((
        Sprite::from_color(
            Color::srgb(0.95, 0.85, 0.3),
            Vec2::splat(PLAYER_HALF_EXTENT * 2.0),
        ),
        Transform::from_xyz(centre.x, centre.y, 100.0),
        Player,
    ));
}

fn move_player(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    walls: Query<&GlobalTransform, With<Wall>>,
    mut player: Query<&mut Transform, With<Player>>,
) {
    let Ok(mut transform) = player.single_mut() else {
        return;
    };

    let mut direction = Vec2::ZERO;
    if keys.any_pressed([KeyCode::KeyW, KeyCode::ArrowUp]) {
        direction.y += 1.0;
    }
    if keys.any_pressed([KeyCode::KeyS, KeyCode::ArrowDown]) {
        direction.y -= 1.0;
    }
    if keys.any_pressed([KeyCode::KeyA, KeyCode::ArrowLeft]) {
        direction.x -= 1.0;
    }
    if keys.any_pressed([KeyCode::KeyD, KeyCode::ArrowRight]) {
        direction.x += 1.0;
    }

    let Some(direction) = direction.try_normalize() else {
        return;
    };
    let delta = direction * PLAYER_SPEED * time.delta_secs();

    let mut position = transform.translation.truncate();
    position.x += delta.x;
    resolve(&mut position, true, &walls);
    position.y += delta.y;
    resolve(&mut position, false, &walls);

    transform.translation.x = position.x;
    transform.translation.y = position.y;
}

/// Pushes `position` back out of any wall it overlaps, along one axis only. Resolving
/// axes separately is what lets the player slide along a wall instead of sticking to it.
fn resolve(position: &mut Vec2, horizontal: bool, walls: &Query<&GlobalTransform, With<Wall>>) {
    let separation = PLAYER_HALF_EXTENT + ROOM_SET.grid_size / 2.0;

    for wall in walls {
        let wall_pos = wall.translation().truncate();
        let offset = *position - wall_pos;

        if offset.x.abs() >= separation || offset.y.abs() >= separation {
            continue;
        }

        if horizontal {
            position.x = wall_pos.x + separation * if offset.x < 0.0 { -1.0 } else { 1.0 };
        } else {
            position.y = wall_pos.y + separation * if offset.y < 0.0 { -1.0 } else { 1.0 };
        }
    }
}

fn follow_player(
    time: Res<Time>,
    player: Query<&Transform, With<Player>>,
    mut camera: Query<&mut Transform, (With<Camera2d>, Without<Player>)>,
) {
    let (Ok(player), Ok(mut camera)) = (player.single(), camera.single_mut()) else {
        return;
    };

    let target = player.translation.truncate();
    let current = camera.translation.truncate();
    let eased = current.lerp(target, (CAMERA_LERP * time.delta_secs()).min(1.0));

    camera.translation.x = eased.x;
    camera.translation.y = eased.y;
}

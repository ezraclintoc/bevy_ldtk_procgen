//! Free-flying pan camera over a generated dungeon.
//!
//! Swap `ROOM_SET` to generate from a different LDtk catalog; this is a compile-time
//! choice, so rebuild after changing it.

use bevy::camera_controller::pan_camera::{PanCamera, PanCameraPlugin};
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::image::ImagePlugin;
use bevy::prelude::*;
use bevy::window::{MonitorSelection, WindowMode};
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::{WorldPlugin, WorldState};

mod common;
use common::{RoomSet, TILEMAP_PACKED};

const ROOM_SET: RoomSet = TILEMAP_PACKED;

#[derive(Component)]
struct HudText;

fn main() {
    let debug = std::env::args().any(|arg| arg == "--debug" || arg == "-d");

    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        mode: WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                        ..default()
                    }),
                    ..default()
                })
                .set(ImagePlugin::default_nearest()),
        )
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
        .add_plugins(PanCameraPlugin)
        .add_plugins(LdtkPlugin)
        .add_plugins(WorldPlugin {
            ldtk_path: ROOM_SET.ldtk_path.into(),
            debug,
            ..default()
        })
        .add_systems(Startup, (setup, setup_hud))
        .add_systems(Update, update_hud)
        .insert_resource(LdtkSettings {
            level_spawn_behavior: LevelSpawnBehavior::UseWorldTranslation {
                load_level_neighbors: false,
            },
            ..default()
        })
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn((Camera2d, PanCamera::default()));
}

fn setup_hud(mut commands: Commands) {
    commands.spawn((
        Text::new("FPS: -\nRooms (data): 0\nRooms (loaded): 0"),
        TextFont {
            font_size: 18.0,
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

fn update_hud(
    diagnostics: Res<DiagnosticsStore>,
    world_state: Res<WorldState>,
    loaded_rooms: Query<Entity, With<LevelSet>>,
    mut hud: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };

    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);

    text.0 = format!(
        "{}\nFPS: {:.0}\nRooms (data): {}\nRooms (loaded): {}",
        ROOM_SET.ldtk_path,
        fps,
        world_state.rooms.len(),
        loaded_rooms.iter().count(),
    );
}

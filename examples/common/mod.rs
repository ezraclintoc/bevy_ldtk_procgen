//! Shared example plumbing: WASD movement, R to regenerate, follow camera.
//!
//! A subdirectory of `examples/` is not auto-discovered as an example target,
//! so this is a module the examples `mod`-include, not a binary of its own.
//!
//! `#![allow(dead_code)]`: every example uses a different subset (`testing`
//! moves a camera, `full` moves a player and follows it), and warnings about
//! the unused half would be noise in all four.

#![allow(dead_code)]

use bevy::prelude::*;

use bevy_ldtk_procgen::prelude::*;

/// The project every example loads, relative to `assets/`. Override with the
/// first command-line argument to try another map:
///
/// ```text
/// cargo run --example full -- my_project_18px.ldtk
/// ```
pub const DEFAULT_MAP: &str = "ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk";

/// [`DEFAULT_MAP`], or the first command-line argument when one is given. A
/// project that fails validation is reported through `GenerationFailed`, so
/// a bad path or an invalid map shows up in the log rather than as a panic.
pub fn map_path() -> String {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_MAP.to_string());
    info!("map: {path}");
    path
}

/// The library must not panic on a bad catalog (`AGENTS.md`, lint policy) —
/// `GenerationFailed` is how that surfaces instead. Every example logs it, so
/// a map passed on the command line that fails validation says why rather
/// than leaving an empty screen.
pub fn show_generation_failure(mut failed: MessageReader<GenerationFailed>) {
    for GenerationFailed(errors) in failed.read() {
        for error in errors.iter() {
            error!("catalog invalid: {error:?}");
        }
    }
}

/// Longest side, in tiles, a room may have and still count as a hall.
pub const MAX_HALL_TILES: u32 = 7;

/// The "halls only" weight table (`DESIGN.md` §9): a room is a hall when no
/// side is longer than [`MAX_HALL_TILES`] — by footprint, not by name,
/// because the bundled project's junctions (`Level_39`–`Level_43`, 4x5 tees
/// and a crossing) aren't named `Hallway_*`, and without them the maze can't
/// branch. Every hall weighs its door count: dead ends (1) are the least
/// likely pick, straights and corners (2) next, junctions (3–4) the most —
/// which keeps the frontier growing instead of capping itself off. Everything
/// bigger weighs 0. A map with no hall-sized rooms gets every room, weighted
/// the same way, rather than generating nothing.
///
/// Index `i` is `RoomId(i)`, which is catalog order (`Catalog::iter`).
#[allow(clippy::cast_precision_loss)] // door counts are tiny
pub fn hall_weights(catalog: &Catalog) -> Vec<f32> {
    let is_hall = |def: &RoomDef| def.size.width.max(def.size.height) <= MAX_HALL_TILES;
    let any_halls = catalog.iter().any(is_hall);
    catalog
        .iter()
        .map(|def| {
            if !any_halls || is_hall(def) {
                def.doors.len() as f32
            } else {
                0.0
            }
        })
        .collect()
}

/// Pixels per second of WASD movement. A constant speed with no acceleration:
/// the examples are about generation, not character control.
pub const WALK_SPEED: f32 = 260.0;

/// WASD / arrow keys → direction. Diagonals are normalized, so holding two
/// keys isn't 1.41× faster than one.
pub fn wasd_direction(keys: &ButtonInput<KeyCode>) -> Vec2 {
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
    direction.normalize_or_zero()
}

/// Moves every `M` at [`WALK_SPEED`]. Generic over the marker so the camera
/// examples (`M = Camera`, where the camera *is* the anchor) and `full`
/// (`M = Player`, with a follow cam behind it) share one implementation —
/// what moves is an anchor, and generation follows anchors.
pub fn move_with_wasd<M: Component>(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut movers: Query<&mut Transform, With<M>>,
) {
    let direction = wasd_direction(&keys);
    if direction == Vec2::ZERO {
        return;
    }
    for mut transform in movers.iter_mut() {
        transform.translation += direction.extend(0.0) * WALK_SPEED * time.delta_secs();
    }
}

/// R → regenerate with a fresh random seed; Shift+R → regenerate the
/// current seed again (`GenerationSeed`), around the anchor's current
/// position. Either way the layout and every spawned room are discarded.
///
/// The library takes a **message**, never a keybinding (`DESIGN.md` §10) —
/// binding the keys is the example's job.
pub fn regenerate_on_key(
    keys: Res<ButtonInput<KeyCode>>,
    seed: Res<GenerationSeed>,
    mut regenerate: MessageWriter<Regenerate>,
) {
    if !keys.just_pressed(KeyCode::KeyR) {
        return;
    }
    let same = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    regenerate.write(Regenerate {
        seed: same.then_some(seed.0),
    });
}

/// Smoothed follow cam, for the examples whose anchor is *not* the camera.
/// Lerping rather than snapping keeps the streaming readable: rooms appear
/// ahead of the player instead of jolting into frame.
pub fn camera_follow<M: Component>(
    target: Query<&Transform, (With<M>, Without<Camera>)>,
    mut camera: Query<&mut Transform, (With<Camera>, Without<M>)>,
) {
    let Ok(target) = target.single() else {
        return;
    };
    for mut transform in camera.iter_mut() {
        transform.translation = transform.translation.lerp(target.translation, 0.15);
    }
}

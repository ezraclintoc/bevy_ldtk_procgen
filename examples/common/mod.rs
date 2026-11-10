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

/// R → regenerate: discard the layout and every spawned room, then generate
/// again around the anchor's current position.
///
/// The library takes a **message**, never a keybinding (`DESIGN.md` §10) —
/// binding `R` is the example's job. The seed is `None`, i.e. a fresh random
/// one, because reproducing the example's hard-coded seed would rebuild the
/// identical dungeon and look like the key did nothing.
pub fn regenerate_on_key(
    keys: Res<ButtonInput<KeyCode>>,
    mut regenerate: MessageWriter<Regenerate>,
) {
    if keys.just_pressed(KeyCode::KeyR) {
        regenerate.write(Regenerate::default());
    }
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

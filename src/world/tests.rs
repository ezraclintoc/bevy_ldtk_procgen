//! Exercises the generation algorithm directly against `assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk`, without a
//! Bevy `App`/`AssetServer`: `LdtkJson` parses straight from the file via `serde_json`
//! and implements `RawLevelAccessor` on its own.

use super::pipeline::{build_room_index, generate_batch};
use super::types::{rects_collide, rects_collide_tl, Door, GenerationConfig, RoomIndex, WorldState};
use bevy::prelude::Vec2;
use bevy_ecs_ldtk::ldtk::LdtkJson;
use bevy_ecs_ldtk::prelude::RawLevelAccessor;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use std::time::Instant;

const MAX_BATCHES: usize = 200;
// Deliberately low: generation currently stalls after a handful of rooms once every
// open door's candidates fail placement (see the find_bridging_room "first candidate
// only, no fallback" issue in docs/report.md). This floor exists to catch generation
// being completely broken (e.g. the spawn room itself failing to place), not to assert
// full dungeon coverage.
const MIN_EXPECTED_ROOMS: usize = 3;

fn load_room_index() -> RoomIndex {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk");
    let raw = std::fs::read_to_string(path).expect("failed to read assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk");
    let json: LdtkJson = serde_json::from_str(&raw).expect("failed to parse assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk");
    build_room_index(json.iter_raw_levels())
}

/// Commits a batch's placed rooms; door bookkeeping happens inside add_room.
fn commit_batch(state: &mut WorldState, placed: &[super::types::Room]) {
    for room in placed {
        state.add_room(room.clone());
    }
}

/// Drives generation batch by batch, mirroring the real two-stage pipeline:
/// `generate_batch` (mimicking `spawn_if_idle`) runs against a throwaway clone of the
/// persistent state so within-batch placements stay isolated, then `commit_batch`
/// (mimicking `poll_task`) folds the result into the real state. Walks a virtual
/// camera to the last room placed each batch so the search frontier keeps advancing
/// instead of stalling inside one `CAMERA_SPAWN_DIST` circle. Capped at `MAX_BATCHES`
/// rather than a target room count, since some batches legitimately place zero rooms
/// (known bridging-fallback gap) without that meaning generation is broken.
fn drive_generation(room_idx: &RoomIndex) -> WorldState {
    let mut state = WorldState::default();
    let mut rng = SmallRng::seed_from_u64(42);
    let mut cam_pos = Vec2::ZERO;
    let config = GenerationConfig::default();

    for _ in 0..MAX_BATCHES {
        let mut batch_state = state.clone();
        let placed = generate_batch(&mut batch_state, room_idx, cam_pos, config.camera_spawn_dist, config.max_rooms, &mut rng);
        if placed.is_empty() {
            break;
        }
        if let Some(last) = placed.last() {
            cam_pos = last.world_pos;
        }
        commit_batch(&mut state, &placed);
    }
    state
}

#[test]
fn no_overlapping_rooms_after_generation() {
    let room_idx = load_room_index();
    let state = drive_generation(&room_idx);

    assert!(
        state.rooms.len() >= MIN_EXPECTED_ROOMS,
        "generation stalled almost immediately - only {} rooms placed",
        state.rooms.len()
    );

    for (i, a) in state.rooms.iter().enumerate() {
        for b in &state.rooms[i + 1..] {
            assert!(
                !rects_collide_tl(
                    a.world_pos,
                    a.room.size.as_vec2(),
                    b.world_pos,
                    b.room.size.as_vec2(),
                ),
                "rooms {:?} and {:?} overlap at {:?} / {:?}",
                a.room.iid,
                b.room.iid,
                a.world_pos,
                b.world_pos,
            );
        }
    }
}

/// Benchmarks room-placement speed at several target room counts. For each target, tries
/// up to `MAX_SEED_ATTEMPTS` different seeds and keeps only the runs that actually reach
/// that target - a run that stalls early (the known bridging-fallback gap, a correctness
/// issue, not a speed one; see docs/report.md) is excluded rather than averaged in, so an
/// unrelated correctness bug can't masquerade as a speed regression. Reports the average
/// time-per-room across the successful runs at each target - a flat value across targets
/// would mean roughly linear cost; a value that keeps climbing with the target (as it
/// does today) is the signature of `try_place_room`'s flat linear collision scan (no
/// spatial hashing yet - see readme.md's "Status" note). If zero seeds reach a target,
/// that's reported rather than silently skipped: it's real information (this target isn't
/// reachable with today's algorithm) that belongs in the comparison across versions.
#[test]
#[ignore = "slow (up to 50 seeds x 4 targets, ~10s) - run explicitly with \
            `cargo test generation_speed_by_target -- --ignored`"]
fn generation_speed_by_target() {
    const TARGETS: [usize; 4] = [100, 250, 500, 1000];
    const MAX_SEED_ATTEMPTS: u64 = 50;

    let room_idx = load_room_index();
    let mut any_target_reached = false;
    let config = GenerationConfig::default();

    for &target in &TARGETS {
        let mut time_per_room_samples: Vec<f64> = Vec::new();

        for seed in 0..MAX_SEED_ATTEMPTS {
            let mut state = WorldState::default();
            let mut rng = SmallRng::seed_from_u64(seed);
            let mut cam_pos = Vec2::ZERO;

            let start = Instant::now();
            for _ in 0..MAX_BATCHES {
                if state.rooms.len() >= target {
                    break;
                }
                let mut batch_state = state.clone();
                let placed = generate_batch(&mut batch_state, &room_idx, cam_pos, config.camera_spawn_dist, config.max_rooms, &mut rng);
                if placed.is_empty() {
                    break;
                }
                if let Some(last) = placed.last() {
                    cam_pos = last.world_pos;
                }
                commit_batch(&mut state, &placed);
            }
            let elapsed = start.elapsed();

            if state.rooms.len() >= target {
                time_per_room_samples.push(elapsed.as_secs_f64() / state.rooms.len() as f64);
            }
        }

        if time_per_room_samples.is_empty() {
            println!(
                "{} rooms: 0/{} seeds reached target - not reached this run. This may be the \
                 known bridging-fallback stall (see docs/report.md), not a speed regression; \
                 try again (e.g. widen the seed range) before concluding this target regressed.",
                target, MAX_SEED_ATTEMPTS,
            );
            continue;
        }

        any_target_reached = true;
        let avg_us_per_room = time_per_room_samples.iter().sum::<f64>()
            / time_per_room_samples.len() as f64
            * 1e6;
        println!(
            "{} rooms: {}/{} seeds reached target, avg {:.2} us/room",
            target,
            time_per_room_samples.len(),
            MAX_SEED_ATTEMPTS,
            avg_us_per_room,
        );
    }

    assert!(
        any_target_reached,
        "no target room count was reached by any of {} seeds - generation appears completely broken",
        MAX_SEED_ATTEMPTS,
    );
}

/// Directly exercises the "doors overlap other rooms" symptom that motivated this
/// review: `no_overlapping_rooms_after_generation` only checks room-vs-room rects
/// (try_place_room's step 1) - that's the actual visual-safety guarantee (no room's
/// tiles are ever drawn over another's). A door's bounding box is a different, softer
/// concept: reserved breathing room for a door that's still unconnected, so something
/// can eventually attach there. Once a door is resolved (matched to a specific
/// neighbor, no longer in `open_doors`), that reservation has already served its
/// purpose - nothing new is ever drawn in that zone, so checking it against an
/// unrelated room afterward doesn't correspond to a real visual overlap. This test
/// therefore only checks the clearance zone for doors that are STILL open at the end
/// of generation (genuinely unconnected, so the reservation still matters); a
/// resolved door's clearance overlapping another room is expected and fine.
#[test]
fn no_door_overlaps_with_room_bodies_after_generation() {
    let room_idx = load_room_index();
    let state = drive_generation(&room_idx);

    assert!(
        state.rooms.len() >= MIN_EXPECTED_ROOMS,
        "generation stalled almost immediately - only {} rooms placed",
        state.rooms.len()
    );

    for (i, room) in state.rooms.iter().enumerate() {
        for doordef in &room.room.doors {
            let door = Door::new(room, doordef);

            let is_still_open = state.open_doors.iter().any(|d| d.world_pos == door.world_pos);
            if !is_still_open {
                continue;
            }

            let (bbox_pos, bbox_size) = door.get_bounding_box();

            for (j, other) in state.rooms.iter().enumerate() {
                if i == j {
                    continue;
                }
                if !rects_collide(bbox_pos, bbox_size, other.world_pos, other.room.size.as_vec2()) {
                    continue;
                }
                let is_matching_connection = other.room.doors.iter().any(|d| {
                    d.dir == doordef.dir.opposite() && Door::new(other, d).world_pos == door.world_pos
                });
                assert!(
                    is_matching_connection,
                    "open door of room {:?} (dir {:?}) at {:?} overlaps room {:?}'s body at {:?}/{:?} without a matching connection",
                    room.room.iid,
                    doordef.dir,
                    door.world_pos,
                    other.room.iid,
                    other.world_pos,
                    other.room.size,
                );
            }
        }
    }
}

/// Regression test for the "some open doors within CAMERA_SPAWN_DIST never get filled"
/// known issue (see docs/report.md). Keeps the camera fixed (mirrors a player standing
/// still) and runs only a handful of batches: since `spawn_if_idle` reseeds the same
/// SmallRng seed every batch in production, a door that fails to fill once at a fixed
/// camera position tends to keep failing identically batch after batch, so a "few"
/// batches is enough to reveal a permanent stall, not just one that needs more time.
///
#[test]
fn nearby_open_doors_get_filled_within_a_few_batches() {
    const FEW_BATCHES: usize = 100;

    let room_idx = load_room_index();
    let mut state = WorldState::default();
    let mut rng = SmallRng::seed_from_u64(42);
    let cam_pos = Vec2::ZERO;
    let config = GenerationConfig::default();

    for _ in 0..FEW_BATCHES {
        let mut batch_state = state.clone();
        let placed = generate_batch(&mut batch_state, &room_idx, cam_pos, config.camera_spawn_dist, config.max_rooms, &mut rng);
        if placed.is_empty() {
            break;
        }
        commit_batch(&mut state, &placed);
    }

    let stuck_doors: Vec<Vec2> = state
        .open_doors
        .iter()
        .filter(|d| d.world_pos.distance(cam_pos) <= config.camera_spawn_dist)
        .map(|d| d.world_pos)
        .collect();

    assert!(
        stuck_doors.is_empty(),
        "{} open door(s) within CAMERA_SPAWN_DIST ({}) of the camera are still unfilled after {} batches: {:?}",
        stuck_doors.len(),
        config.camera_spawn_dist,
        FEW_BATCHES,
        stuck_doors,
    );
}

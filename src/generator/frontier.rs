//! `OriginFrontier`: the open doors ordered by distance from the dungeon's
//! origin, for deterministic generation.
//!
//! Filling doors nearest-to-the-anchor makes the dungeon depend on the path
//! the anchor walked: the same seed, explored differently, places different
//! rooms. Filling the door nearest the *origin* instead — always the global
//! minimum, never a local one — makes the sequence of attempts a fixed
//! function of the seed: `next` with a limit returns exactly the global
//! minimum while it is inside the limit and stops otherwise, so any schedule
//! of growing limits replays a prefix of the same one infinite sequence.
//! How far out generation has reached, and how many rooms each frame got
//! through, decide only how long that prefix is, never what is in it.
//!
//! What still breaks it: anything that changes RNG draws or candidate
//! weights between runs — a weight table that reads live state (DESIGN.md
//! §7), or a failed attempt that is retried later instead of abandoned
//! (`next`'s caller must close a door whose attempt failed, or it is
//! returned again forever).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::catalog::Catalog;
use super::geom::{OpenDoorId, PlacementId};
use super::layout::{Layout, OpenDoor};

/// Heap key: squared distance from the origin, then position, then the
/// door's identity — total, and made only of values the placement sequence
/// itself determines (no slot numbers, which depend on close order).
type Key = (i64, i32, i32, u32, u8);

/// Open doors keyed by distance from tile (0, 0) — where deterministic
/// generation puts its first room — nearest first. Lazy: newly opened doors are ingested from the Layout on demand,
/// closed ones are discarded when they surface.
#[derive(Debug, Clone, Default)]
pub(crate) struct OriginFrontier {
    heap: BinaryHeap<Reverse<Key>>,
    /// Placements whose doors are already in `heap`. Doors only ever open
    /// when their own placement commits, so new placements are the only
    /// source of new open doors.
    ingested: usize,
}

impl OriginFrontier {
    /// The open door nearest the origin, if it lies within `max_distance`
    /// tiles; `None` otherwise (or with no open doors). Peeks, not pops: the
    /// door stays the answer until it is closed, by a successful placement
    /// or by abandoning it.
    pub(crate) fn next(
        &mut self,
        layout: &Layout,
        catalog: &Catalog,
        max_distance: f32,
    ) -> Option<OpenDoorId> {
        self.ingest(layout, catalog);
        while let Some(&Reverse((dist_sq, _, _, placement, door))) = self.heap.peek() {
            // i64 -> f64 is exact up to 2^53, far beyond any tile distance.
            #[allow(clippy::cast_precision_loss)]
            let distance = (dist_sq as f64).sqrt();
            if distance > f64::from(max_distance) {
                return None;
            }
            let open = OpenDoor {
                placement: PlacementId(placement),
                door,
            };
            if let Some(id) = layout.open_door_id(open) {
                return Some(id);
            }
            self.heap.pop();
        }
        None
    }

    fn ingest(&mut self, layout: &Layout, catalog: &Catalog) {
        // Indexed, not `iter().skip(..)`: skipping walks every earlier
        // placement, which made each call O(rooms placed).
        for slot in self.ingested..layout.placed_count() {
            let id = PlacementId(u32::try_from(slot).unwrap_or(u32::MAX));
            let Some(placement) = layout.get(id) else {
                continue;
            };
            let Some(def) = catalog.get(placement.room) else {
                continue;
            };
            for (i, door) in def.doors.iter().enumerate() {
                let Ok(index) = u8::try_from(i) else {
                    continue;
                };
                let open = OpenDoor {
                    placement: id,
                    door: index,
                };
                if layout.open_door_id(open).is_none() {
                    continue; // connected on commit
                }
                let pos = placement.origin + door.local_pos;
                let (dx, dy) = (i64::from(pos.x), i64::from(pos.y));
                self.heap
                    .push(Reverse((dx * dx + dy * dy, pos.y, pos.x, id.0, index)));
            }
        }
        self.ingested = layout.placed_count();
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::SmallRng;

    use super::*;
    use crate::generator::catalog::build_catalog;
    use crate::generator::geom::TilePos;
    use crate::generator::place::{StartRoom, WeightParams, attempt_door, place_start_room};

    fn load_catalog() -> Catalog {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk"
        );
        let raw = std::fs::read_to_string(path).unwrap();
        build_catalog(&serde_json::from_str(&raw).unwrap()).unwrap()
    }

    /// Grows a dungeon from the origin, raising the reach through
    /// `schedule` and placing at most `per_step` rooms per step — the shape
    /// of the plugin's per-frame loop, with the frame budget and the
    /// anchor's travel as the two free variables.
    fn grow(seed: u64, schedule: &[f32], per_step: usize) -> Vec<(u32, TilePos)> {
        let catalog = load_catalog();
        let mut layout = Layout::default();
        let mut rng = SmallRng::seed_from_u64(seed);
        let origin = TilePos { x: 0, y: 0 };
        place_start_room(
            &mut layout,
            &catalog,
            &StartRoom::Any,
            origin,
            &mut rng,
            WeightParams::default(),
        )
        .unwrap();
        let mut frontier = OriginFrontier::default();
        for &reach in schedule {
            // Keep stepping at this reach until it is exhausted, `per_step`
            // at a time — frames passing while the anchor stands still.
            loop {
                let mut placed = 0;
                while placed < per_step {
                    let Some(door) = frontier.next(&layout, &catalog, reach) else {
                        break;
                    };
                    if attempt_door(
                        &mut layout,
                        &catalog,
                        door,
                        &mut rng,
                        WeightParams::default(),
                    )
                    .is_none()
                    {
                        layout.close_door(door);
                    }
                    placed += 1;
                }
                if placed < per_step {
                    break;
                }
            }
        }
        layout.iter().map(|(_, p)| (p.room.0, p.origin)).collect()
    }

    /// The point of the frontier: one jump to the final reach, or a slow
    /// walk out to it at a different per-frame budget, place the same rooms
    /// in the same order.
    #[test]
    fn layout_is_independent_of_reach_schedule_and_frame_budget() {
        for seed in 0..8 {
            let jump = grow(seed, &[60.0], 1000);
            let walk: Vec<f32> = (1..=30).map(|i| i as f32 * 2.0).collect();
            let crawl = grow(seed, &walk, 3);
            assert!(jump.len() > 50, "seed {seed}: only {} rooms", jump.len());
            assert_eq!(jump, crawl, "seed {seed}: schedule changed the dungeon");
        }
    }

    /// A smaller reach yields a prefix of a larger one's placements — so
    /// walking further only ever adds rooms after the ones already there.
    #[test]
    fn smaller_reach_is_a_prefix() {
        let near = grow(3, &[25.0], 1000);
        let far = grow(3, &[50.0], 1000);
        assert!(near.len() < far.len());
        assert_eq!(near[..], far[..near.len()]);
    }
}

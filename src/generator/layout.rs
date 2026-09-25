//! `Layout`, `Placement`, `OpenDoor`. See ARCHITECTURE.md §3.
//!
//! `Layout::commit` does no geometry of its own — matching a candidate
//! against open doors and checking overlap is `place.rs`'s job (not written
//! yet); this file only keeps `placements`, `open_doors`, and the spatial
//! index from drifting out of sync with each other, the way the old
//! `WorldState::add_room` did for its two structures (ARCHITECTURE.md §5).

use super::catalog::Catalog;
use super::geom::{OpenDoorId, PlacementId, RoomId, TilePos, TileRect, TileSize};
use super::spatial::SpatialHash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Placement {
    pub room: RoomId,
    pub origin: TilePos,
}

/// No position stored — see `open_door_pos` — so there's no float to
/// compare and the old float-equality door bug can't recur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenDoor {
    pub placement: PlacementId,
    pub door: u8,
}

pub fn open_door_pos(layout: &Layout, catalog: &Catalog, d: OpenDoor) -> Option<TilePos> {
    let placement = layout.get(d.placement)?;
    let def = catalog.get(placement.room)?;
    let door = def.doors.get(d.door as usize)?;
    Some(placement.origin + door.local_pos)
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    placements: Vec<Placement>,
    /// Tombstoned slots, not `swap_remove` — see `OpenDoorId`'s doc comment.
    /// Each slot's generation increments every time it's freed, so a stale
    /// `OpenDoorId` from before that point no longer matches.
    open_doors: Vec<(u32, Option<OpenDoor>)>,
    free_door_slots: Vec<u32>,
    /// `None` until the first `commit`: the cell size comes from
    /// `Catalog::spatial_cell_size`, unknown when `Layout::default()` runs
    /// (e.g. `init_resource` at plugin build time, before any catalog is
    /// loaded).
    placements_index: Option<SpatialHash<PlacementId>>,
}

impl Layout {
    pub fn get(&self, id: PlacementId) -> Option<&Placement> {
        self.placements.get(id.0 as usize)
    }

    pub fn placed_count(&self) -> usize {
        self.placements.len()
    }

    pub fn open_doors(&self) -> impl Iterator<Item = (OpenDoorId, &OpenDoor)> {
        self.open_doors
            .iter()
            .enumerate()
            .filter_map(|(i, (generation, slot))| {
                slot.as_ref().map(|door| {
                    let id = OpenDoorId {
                        slot: i as u32,
                        generation: *generation,
                    };
                    (id, door)
                })
            })
    }

    pub fn iter(&self) -> impl Iterator<Item = (PlacementId, &Placement)> {
        self.placements
            .iter()
            .enumerate()
            .map(|(i, p)| (PlacementId(i as u32), p))
    }

    /// Every placement whose spatial-hash cell overlaps `rect` — a
    /// broad-phase result, not an exact-overlap one; the caller still needs
    /// `geom::overlaps` against each candidate's real bounds. Empty before
    /// the first `commit` (no index yet, nothing placed yet either).
    pub fn nearby(&self, rect: TileRect) -> Vec<PlacementId> {
        self.placements_index
            .as_ref()
            .map(|index| index.query(rect))
            .unwrap_or_default()
    }

    /// Point -> room lookup (DESIGN.md §4, "needed constantly"), via the
    /// spatial index rather than a full scan.
    pub fn room_at(&self, catalog: &Catalog, pos: TilePos) -> Option<RoomId> {
        let point = TileRect {
            min: pos,
            size: TileSize {
                width: 1,
                height: 1,
            },
        };
        self.nearby(point).into_iter().find_map(|id| {
            let placement = self.get(id)?;
            let def = catalog.get(placement.room)?;
            let rect = TileRect {
                min: placement.origin,
                size: def.size,
            };
            rect.contains(pos).then_some(placement.room)
        })
    }

    /// Commits one placement: assigns it a `PlacementId`, indexes it
    /// spatially, closes whichever open doors `closes` names, and opens its
    /// own doors in turn. `closes` are `OpenDoorId`s the caller has already
    /// matched against this placement's door positions — this method does
    /// no position math itself, only bookkeeping.
    ///
    /// `None` only if `room` isn't in `catalog` — meaning the caller passed
    /// a `RoomId` from a different `Catalog`, which shouldn't happen given
    /// `RoomId` is mint-only-by-`Catalog`, but is still checked rather than
    /// indexed into blindly (`indexing_slicing` is denied).
    pub fn commit(
        &mut self,
        catalog: &Catalog,
        room: RoomId,
        origin: TilePos,
        closes: &[OpenDoorId],
    ) -> Option<PlacementId> {
        let def = catalog.get(room)?;

        let index = self
            .placements_index
            .get_or_insert_with(|| SpatialHash::new(catalog.spatial_cell_size()));

        let id = PlacementId(u32::try_from(self.placements.len()).unwrap_or(u32::MAX));
        self.placements.push(Placement { room, origin });

        let rect = TileRect {
            min: origin,
            size: def.size,
        };
        index.insert(rect, id);

        for &close in closes {
            self.close_door(close);
        }

        for (i, _door) in def.doors.iter().enumerate() {
            let door_index = u8::try_from(i).unwrap_or(u8::MAX);
            self.open_door(OpenDoor {
                placement: id,
                door: door_index,
            });
        }

        Some(id)
    }

    fn open_door(&mut self, door: OpenDoor) -> OpenDoorId {
        if let Some(slot_index) = self.free_door_slots.pop() {
            // The generation was already bumped when this slot was freed
            // (see `close_door`) — reuse it as-is so this new door's id
            // compares unequal to whatever id pointed at the old occupant.
            let generation = self
                .open_doors
                .get(slot_index as usize)
                .map_or(0, |(g, _)| *g);
            if let Some(slot) = self.open_doors.get_mut(slot_index as usize) {
                slot.1 = Some(door);
            }
            OpenDoorId {
                slot: slot_index,
                generation,
            }
        } else {
            self.open_doors.push((0, Some(door)));
            OpenDoorId {
                slot: u32::try_from(self.open_doors.len() - 1).unwrap_or(u32::MAX),
                generation: 0,
            }
        }
    }

    /// `None` if `id` was already closed, stale (its slot was freed and
    /// reused since), or never existed — every one of those is "nothing to
    /// close", reported as such rather than a panic.
    pub fn close_door(&mut self, id: OpenDoorId) -> Option<OpenDoor> {
        let (generation, slot) = self.open_doors.get_mut(id.slot as usize)?;
        if *generation != id.generation {
            return None;
        }
        let taken = slot.take();
        if taken.is_some() {
            *generation = generation.wrapping_add(1);
            self.free_door_slots.push(id.slot);
        }
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::catalog::build_catalog;
    use crate::generator::geom::TileSize;

    /// `commit` does no geometry validation of its own — matching doors by
    /// position is `place.rs`'s job, not written yet — so this exercises the
    /// bookkeeping contract directly: whatever `OpenDoorId`s are named in
    /// `closes` get closed, regardless of whether they're geometrically
    /// sensible for the placement. That's a deliberate simplification, not
    /// an oversight: real door-matching needs `place.rs` to test honestly.
    fn load_catalog() -> Catalog {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk"
        );
        let raw = std::fs::read_to_string(path).expect("fixture must exist");
        let project: bevy_ecs_ldtk::ldtk::LdtkJson =
            serde_json::from_str(&raw).expect("fixture must be valid LDtk JSON");
        build_catalog(&project).expect("bundled catalog should validate")
    }

    #[test]
    fn commit_close_and_spatial_queries_stay_consistent() {
        let catalog = load_catalog();
        let room = RoomId(0);
        let def = catalog.get(room).expect("room 0 must exist");
        let door_count = def.doors.len();

        let mut layout = Layout::default();

        let first = layout
            .commit(&catalog, room, TilePos { x: 0, y: 0 }, &[])
            .expect("commit against a real catalog room must succeed");
        assert_eq!(layout.placed_count(), 1);
        assert_eq!(layout.open_doors().count(), door_count);

        let closing = layout
            .open_doors()
            .next()
            .map(|(id, _door)| id)
            .expect("a room with at least one door has at least one open door");

        let second = layout
            .commit(&catalog, room, TilePos { x: 1000, y: 1000 }, &[closing])
            .expect("second commit must succeed");
        assert_ne!(first, second);
        assert_eq!(layout.placed_count(), 2);
        // One door closed by `closes`, `door_count` more opened by the
        // second placement — net change is `door_count - 1`.
        assert_eq!(layout.open_doors().count(), door_count * 2 - 1);
        assert!(
            layout.open_doors().all(|(id, _)| id != closing),
            "the door named in `closes` must no longer be open"
        );

        // Closing it again is a no-op, not a panic or a double-decrement.
        assert!(layout.close_door(closing).is_none());
        assert_eq!(layout.open_doors().count(), door_count * 2 - 1);

        assert_eq!(layout.room_at(&catalog, TilePos { x: 0, y: 0 }), Some(room));
        assert_eq!(
            layout.room_at(&catalog, TilePos { x: 1000, y: 1000 }),
            Some(room)
        );
        assert_eq!(
            layout.room_at(&catalog, TilePos { x: -5000, y: -5000 }),
            None
        );

        let region = TileRect {
            min: TilePos { x: -1, y: -1 },
            size: TileSize {
                width: 3,
                height: 3,
            },
        };
        let nearby = layout.nearby(region);
        assert!(nearby.contains(&first));
        assert!(!nearby.contains(&second));
    }

    #[test]
    fn open_door_pos_matches_placement_origin_plus_local_pos() {
        let catalog = load_catalog();
        let room = RoomId(0);
        let def = catalog.get(room).expect("room 0 must exist");
        let Some(first_door) = def.doors.first() else {
            return; // nothing to check if this catalog room has no doors
        };
        let origin = TilePos { x: 5, y: 7 };
        let expected = origin + first_door.local_pos;

        let mut layout = Layout::default();
        let placement = layout
            .commit(&catalog, room, origin, &[])
            .expect("commit must succeed");

        let door = OpenDoor { placement, door: 0 };
        assert_eq!(open_door_pos(&layout, &catalog, door), Some(expected));
    }
}

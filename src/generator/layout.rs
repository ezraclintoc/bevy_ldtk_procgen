//! `Layout`, `Placement`, `OpenDoor`. See ARCHITECTURE.md §3.
//!
//! Mutation (`commit`, opening/closing doors) belongs to `place.rs` and
//! isn't written yet.

use super::catalog::Catalog;
use super::geom::{PlacementId, RoomId, TilePos, TileRect};

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
    // TODO(ARCHITECTURE.md §3): Vec + swap_remove invalidates indices, the
    // trap that kept doors out of the old spatial hash. Needs a stable-index
    // structure before wiring into SpatialHash<OpenDoor>.
    open_doors: Vec<OpenDoor>,
}

impl Layout {
    pub fn get(&self, id: PlacementId) -> Option<&Placement> {
        self.placements.get(id.0 as usize)
    }

    pub fn placed_count(&self) -> usize {
        self.placements.len()
    }

    pub fn open_doors(&self) -> &[OpenDoor] {
        &self.open_doors
    }

    pub fn iter(&self) -> impl Iterator<Item = (PlacementId, &Placement)> {
        self.placements
            .iter()
            .enumerate()
            .map(|(i, p)| (PlacementId(i as u32), p))
    }

    /// Linear scan for now, not the spatial index — correct but not final.
    /// Takes `TilePos`, tiles only; `examples/minimal.rs` currently passes a
    /// raw pixel `Vec2` here and will need a plugin-side conversion wrapper.
    pub fn room_at(&self, catalog: &Catalog, pos: TilePos) -> Option<RoomId> {
        self.placements.iter().find_map(|p| {
            let def = catalog.get(p.room)?;
            let rect = TileRect {
                min: p.origin,
                size: def.size,
            };
            rect.contains(pos).then_some(p.room)
        })
    }
}

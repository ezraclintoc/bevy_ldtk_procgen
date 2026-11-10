//! `RoomPlaced`, `RoomSpawned`, `RoomDespawned`, `DoorAbandoned`,
//! `GenerationFailed`, `GenerationSettled`, and the consumer-written
//! `Regenerate`. See DESIGN.md §4.

use bevy::prelude::*;

use crate::generator::error::CatalogErrors;
use crate::generator::geom::{Dir, RoomId, TilePos};

#[derive(Message, Debug, Clone, Copy)]
pub struct RoomPlaced {
    pub room: RoomId,
    pub at: TilePos,
}

#[derive(Message, Debug, Clone, Copy)]
pub struct RoomSpawned {
    pub room: RoomId,
    /// The placement's origin in tiles — the room's min corner. Carried so a
    /// consumer can position things against the room's real bounds without
    /// re-deriving them from `Layout` by entity.
    pub at: TilePos,
    pub entity: Entity,
}

#[derive(Message, Debug, Clone, Copy)]
pub struct RoomDespawned {
    pub room: RoomId,
}

#[derive(Message, Debug, Clone)]
pub struct GenerationFailed(pub CatalogErrors);

#[derive(Message, Debug, Clone, Copy)]
pub struct DoorAbandoned {
    pub at: TilePos,
    pub dir: Dir,
}

#[derive(Message, Debug, Clone, Copy)]
pub struct GenerationSettled;

/// Consumer-written, engine-consumed: throw the current dungeon away and
/// generate again.
///
/// A message, not a keybinding — the library never binds keys (`AGENTS.md`,
/// invariants), so a demo's `R` handler is the consumer's own system writing
/// this. `seed: None` re-rolls from entropy; `seed: Some(s)` reproduces
/// exactly what `s` builds, which is what lets a player press R until the
/// dungeon looks good and then hard-code the seed that produced it
/// (`DESIGN.md` §3).
#[derive(Message, Debug, Clone, Copy, Default)]
pub struct Regenerate {
    pub seed: Option<u64>,
}

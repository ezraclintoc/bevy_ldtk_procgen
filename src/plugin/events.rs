//! `RoomPlaced`, `RoomSpawned`, `RoomDespawned`, `DoorAbandoned`,
//! `GenerationFailed`, `GenerationSettled`. See DESIGN.md §4.

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

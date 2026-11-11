//! The output messages — `RoomPlaced`, `RoomSpawned`, `RoomDespawned`,
//! `DoorAbandoned`, `GenerationFailed`, `GenerationSettled` — and the one
//! input message, `Regenerate`. See DESIGN.md §4.
//!
//! They are Bevy messages (`add_message`), not `Event`s: a consumer that reads
//! them a frame late still sees everything, which is what makes it safe to
//! attach gameplay in a system added after the plugin.

use bevy::prelude::*;

use crate::generator::error::CatalogErrors;
use crate::generator::geom::{Dir, RoomId, TilePos};

/// The Layout grew: one room was committed at a position.
///
/// Fires before the room's entities exist — that is [`RoomSpawned`]'s job,
/// and the two are deliberately separate so a consumer can react to "the
/// dungeon now includes this room" without waiting for tilemap spawning.
#[derive(Message, Debug, Clone, Copy)]
pub struct RoomPlaced {
    /// Which catalog entry was placed (instances of the same room repeat).
    pub room: RoomId,
    /// The placement's min corner, in tiles. With the room's `RoomDef::size`
    /// this is its exact footprint.
    pub at: TilePos,
}

/// The room's entities now exist — `bevy_ecs_ldtk` has spawned its level
/// content, so gameplay can be attached to `entity`.
///
/// Fires once per spawn: a room that is culled and later revisited spawns
/// again (from the unchanged Layout) and fires again.
#[derive(Message, Debug, Clone, Copy)]
pub struct RoomSpawned {
    /// Which catalog entry this instance is.
    pub room: RoomId,
    /// The placement's origin in tiles — the room's min corner. Carried so a
    /// consumer can position things against the room's real bounds without
    /// re-deriving them from `Layout` by entity.
    pub at: TilePos,
    /// The room's root entity — the thing to parent gameplay entities to.
    pub entity: Entity,
}

/// A room's entities were removed, by culling or by a [`Regenerate`].
///
/// The Layout is unchanged by culling, so a room reported
/// here can come back with [`RoomSpawned`]; only `Regenerate` makes a
/// placement disappear for good.
#[derive(Message, Debug, Clone, Copy)]
pub struct RoomDespawned {
    /// Which catalog entry was removed.
    pub room: RoomId,
}

/// The catalog could not be built, or `StartRoom` matched nothing. Generation
/// is stopped in `GenerationState::Failed` when this fires.
///
/// Carries *every* problem found in one pass, not just the first, so a broken
/// project takes one run to fix rather than one run per error.
#[derive(Message, Debug, Clone)]
pub struct GenerationFailed(pub CatalogErrors);

/// A door was given up on: it is no longer open, and nothing will ever be
/// placed through it. It is now a permanent gap in the dungeon, for the
/// consumer to wall off however its art allows.
#[derive(Message, Debug, Clone, Copy)]
pub struct DoorAbandoned {
    /// The door footprint's min corner, in tiles.
    pub at: TilePos,
    /// The direction the abandoned door faces — the side now open to nothing.
    pub dir: Dir,
}

/// Generation has nothing left to do right now: no eligible open door around
/// any anchor. Not a terminal state — moving an anchor can reopen work.
#[derive(Message, Debug, Clone, Copy)]
pub struct GenerationSettled;

/// Consumer-written, engine-consumed: throw the current dungeon away and
/// generate again.
///
/// A message, not a keybinding — the library never binds keys (`AGENTS.md`,
/// invariants), so a demo's `R` handler is the consumer's own system writing
/// this. `seed: None` re-rolls from entropy; `seed: Some(s)` reproduces
/// exactly what `s` builds, which is what lets a player press R until the
/// dungeon looks good and then hard-code the seed that produced it.
#[derive(Message, Debug, Clone, Copy, Default)]
pub struct Regenerate {
    /// `None` re-rolls from entropy; `Some(seed)` rebuilds exactly what that
    /// seed produces.
    pub seed: Option<u64>,
}

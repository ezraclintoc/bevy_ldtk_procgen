//! Procedural dungeon generation for Bevy from an [LDtk] room catalog.
//!
//! Author each room as an LDtk level and mark its doors. At runtime the plugin
//! connects rooms door to door into one dungeon that streams in around
//! whatever carries a [`GenerationAnchor`], endlessly or up to a size you
//! choose. No layout is hand-authored and no room is defined in Rust.
//!
//! [LDtk]: https://ldtk.io
//!
//! # Quick start
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_ecs_ldtk::prelude::*;
//! use bevy_ldtk_procgen::prelude::*;
//!
//! fn main() {
//!     App::new()
//!         .add_plugins((DefaultPlugins, LdtkPlugin))
//!         // Rooms load within 800 px of an anchor, are placed (as data) out
//!         // to 1600 px, and unload beyond 1000 px.
//!         .add_plugins(GeneratorPlugin::new("my_rooms.ldtk", 800.0).with_seed(1234))
//!         .add_systems(Startup, |mut commands: Commands| {
//!             // Generation follows whatever carries the anchor.
//!             commands.spawn((Camera2d, GenerationAnchor));
//!         })
//!         .run();
//! }
//! ```
//!
//! # Authoring rooms
//!
//! - **One LDtk level per room.** Its size must be a whole number of tiles;
//!   the tile size is read from the project, never assumed.
//! - **Doors**, from one of two sources ([`DoorSource`]):
//!   - `Door` entities (the default) on any Entities layer, flush against the
//!     level edge they open through. Direction comes from the edge, width from
//!     the entity's extent along it.
//!   - Gaps in an IntGrid wall layer
//!     ([`DoorSource::IntGridGaps`], via [`GeneratorPlugin::with_door_source`]):
//!     every run of empty cells along an edge is a door that wide.
//! - **At least one single-door room per direction.** These caps are how a
//!   branch ends, and every open door reserves the space for one, so a project
//!   without them fails to load ([`CatalogError::NoCapRoomForDirection`]).
//! - **Tags** (optional): bool tags from a project-wide `RoomTags` enum field,
//!   and float fields per level. The `weight` float field is a room's base
//!   selection weight (1.0 when absent). Tag names have no built-in meaning.
//! - Problems are reported, not panicked on: a bad project fails with
//!   [`GenerationFailed`], each error naming its level. A room whose doors are
//!   too close together to ever be placed is logged as a warning at load.
//!
//! # How it works
//!
//! The [`Layout`] (resource [`LayoutRes`]) is the dungeon: which room is
//! placed where. It only grows, and it is the single source of truth. Spawned
//! level entities are a cache over it: rooms are spawned near an anchor and
//! despawned far from it, and the Layout never changes when they are, so an
//! area you return to is the same area.
//!
//! Three rings around each anchor ([`StreamingRadii`]) drive this: open doors
//! within the **generation** radius are filled (as data, cheap), placed rooms
//! within the **spawn** radius get entities, and spawned rooms beyond the
//! **despawn** radius are removed. The despawn ring is always kept clear of
//! the spawn ring, so no setting can load and unload a room on alternate
//! frames.
//!
//! Every open door reserves room for the smallest room that could close it, so
//! no door is ever sealed by a neighbour and every branch can end cleanly.
//!
//! # Configuration
//!
//! [`GeneratorPlugin::new`]`(path, view_radius)` sets the spawn radius to
//! `view_radius`, generation to twice that and despawn to 1.25×. Builder
//! methods:
//!
//! | Method | Default | |
//! |---|---|---|
//! | [`with_seed`](GeneratorPlugin::with_seed) | random | Fix the dungeon. |
//! | [`with_generation_radius`](GeneratorPlugin::with_generation_radius) | 2 × view | How far ahead rooms are placed. |
//! | [`with_culling`](GeneratorPlugin::with_culling) | radius(view) | Spawn/despawn rings, or [`CullingPolicy::Disabled`]. |
//! | [`with_order`](GeneratorPlugin::with_order) | `NearestAnchor` | Which door is filled next; `FromOrigin` is fully deterministic. |
//! | [`with_start`](GeneratorPlugin::with_start) | `StartRoom::Any` | The first room: any, one with a tag, or a named level. |
//! | [`with_max_rooms`](GeneratorPlugin::with_max_rooms) | endless | A bounded dungeon that closes off gracefully. |
//! | [`with_weight_config`](GeneratorPlugin::with_weight_config) | none | Quotas, depth gates, multi-door preference ([`WeightConfig`]). |
//! | [`with_door_source`](GeneratorPlugin::with_door_source) | `Door` entities | Or gaps in an IntGrid layer. |
//! | [`with_placement_budget`](GeneratorPlugin::with_placement_budget) | 64 rooms / 2 ms | Placement work per frame. |
//! | [`with_spawn_budget`](GeneratorPlugin::with_spawn_budget) | adaptive | Tilemap spawning per frame; adapts to the machine. |
//!
//! [`GeneratorPlugin::from_handle`] takes an already-loading
//! `Handle<LdtkProject>` instead of a path.
//!
//! # Resources
//!
//! | Resource | | |
//! |---|---|---|
//! | [`LayoutRes`] | read | Placed rooms, open doors, point → room queries. |
//! | [`CatalogRes`] | read | Every room definition: size, doors, tags, grid size. |
//! | [`GenerationSeed`] | read | The seed of the current dungeon, random ones included. |
//! | [`OriginReach`] | read | How far `FromOrigin` generation reaches. |
//! | [`SpawnRate`] | read | The spawn budget in effect this frame. |
//! | [`StreamingRadii`] | read/write | The three rings; change them at runtime. |
//! | [`GenerationOrder`] | read/write | Send [`Regenerate`] after switching. |
//! | [`PlacementBudget`] | read/write | Placement work per frame. |
//! | [`SpawnBudget`] | read/write | `Adaptive` or `Fixed` tiles per frame. |
//! | [`RoomWeights`] | write via [`apply_weights`] | Your per-frame weight table. |
//!
//! [`GenerationState`] is `Loading → Building → Ready`, or `Failed`.
//!
//! # Messages
//!
//! | Message | Direction | When |
//! |---|---|---|
//! | [`RoomPlaced`] | out | A room joined the Layout. |
//! | [`RoomSpawned`] | out | Its entities exist; carries the root `Entity`. |
//! | [`RoomDespawned`] | out | Culled; the Layout is unchanged. |
//! | [`DoorAbandoned`] | out | A door can't be filled (weights excluded every room that fits); wall it off. |
//! | [`GenerationSettled`] | out | Nothing left to place around the anchors, until they move. |
//! | [`GenerationFailed`] | out | The project couldn't be loaded; every error names its level. |
//! | [`Regenerate`] | in | Throw the dungeon away: a new random seed, or `Some(seed)`. |
//!
//! There are no built-in keybindings; regenerating on a key is your system
//! writing [`Regenerate`].
//!
//! # Querying the dungeon
//!
//! Positions in the Layout are in tiles ([`TilePos`]); divide world pixels by
//! [`Catalog::grid_size`] to convert.
//!
//! ```
//! use bevy::prelude::*;
//! use bevy_ldtk_procgen::prelude::*;
//!
//! #[derive(Component)]
//! struct Player;
//!
//! /// Which room is the player in?
//! fn current_room(
//!     player: Single<&GlobalTransform, With<Player>>,
//!     layout: Res<LayoutRes>,
//!     catalog: Res<CatalogRes>,
//! ) {
//!     let grid = catalog.grid_size() as f32;
//!     let px = player.translation();
//!     let tile = TilePos {
//!         x: (px.x / grid).floor() as i32,
//!         y: (px.y / grid).floor() as i32,
//!     };
//!     if let Some(def) = layout.room_at(&catalog, tile).and_then(|id| catalog.get(id)) {
//!         info!("in {}", def.identifier);
//!     }
//! }
//! # bevy::ecs::system::assert_is_system(current_room);
//! ```
//!
//! [`Layout::iter`] lists every placement, [`Layout::nearby`] the ones in a
//! rectangle, and [`Layout::open_doors`] the unconnected doors, which is
//! enough for minimaps, pathfinding and placing content.
//!
//! # Weights and meta-generation
//!
//! Each candidate room's chance is its `weight` tag, times your per-frame
//! table, times the engine's [`WeightConfig`] rules. The table is a system
//! returning one weight per room ([`RoomId`] order, i.e. catalog order), piped
//! into [`apply_weights`] in [`GenerationSet::Weights`]:
//!
//! ```
//! use bevy::prelude::*;
//! use bevy_ldtk_procgen::prelude::*;
//!
//! /// Corridors only: weight rooms by door count, so the dungeon keeps
//! /// branching instead of closing off.
//! fn corridor_weights(catalog: Res<CatalogRes>) -> Vec<f32> {
//!     catalog.iter().map(|room| room.doors.len() as f32).collect()
//! }
//!
//! fn setup(app: &mut App) {
//!     app.add_systems(
//!         Update,
//!         corridor_weights
//!             .pipe(apply_weights)
//!             .in_set(GenerationSet::Weights),
//!     );
//! }
//! # let _ = setup;
//! ```
//!
//! For a bounded dungeon, [`GeneratorPlugin::with_max_rooms`] sets a ceiling;
//! as it nears, candidates are steered toward caps so every branch closes
//! rather than stopping mid-corridor.
//!
//! # Seeds and determinism
//!
//! Every dungeon has a seed ([`GenerationSeed`]), so any dungeon can be
//! rebuilt with `Regenerate { seed: Some(seed) }`. Under
//! [`GenerationOrder::FromOrigin`] the result is exact however the dungeon
//! was explored and however fast the machine is; under the default
//! `NearestAnchor`, the anchor's route is an input too.
//!
//! # Performance
//!
//! Placing a room costs ~10 µs, flat to a million rooms (~27 bytes of memory
//! each). Spawning one is the expensive part (~2.6 µs per tile), which is why
//! rooms are placed well ahead of what is loaded and why the default
//! [`SpawnBudget`] learns, per machine, how much it can spawn per frame
//! without a stutter.
//!
//! # Examples
//!
//! `cargo run --release --example <name>`: `minimal` (the output API),
//! `halls` (a player with wall collision in an endless maze), `meta` (a
//! bounded dungeon) and `testing` (a debug harness with live ring and budget
//! controls).

mod generator;
pub mod plugin;

// The crate docs above link to prelude items by bare name.
#[allow(unused_imports)]
use prelude::*;

/// Everything a consumer normally needs, in one `use`.
pub mod prelude {
    pub use crate::generator::catalog::{
        Catalog, CatalogError, DoorSource, FloatTagId, RoomDef, TagId,
    };
    pub use crate::generator::error::CatalogErrors;
    pub use crate::generator::geom::{
        Dir, PlacementId, RoomId, TilePos, TileRect, TileSize, overlaps,
    };
    pub use crate::generator::layout::{Layout, open_door_pos};
    pub use crate::generator::place::StartRoom;
    pub use crate::generator::weight::{CountBy, WeightConfig};
    pub use crate::plugin::{
        CatalogRes, CullingPolicy, DoorAbandoned, GenerationAnchor, GenerationFailed,
        GenerationOrder, GenerationSeed, GenerationSet, GenerationSettled, GenerationState,
        GeneratorPlugin, LayoutRes, OriginReach, PlacementBudget, Regenerate, RoomDespawned,
        RoomPlaced, RoomSpawned, RoomWeights, SpawnBudget, SpawnRate, StreamingRadii,
        apply_weights,
    };
}

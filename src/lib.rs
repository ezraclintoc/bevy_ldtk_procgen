//! Procedural dungeon generation from an LDtk room catalog.
//!
//! Author rooms as LDtk levels, put `Door` entities on their edges, and the
//! plugin connects them at runtime into one dungeon that streams in around
//! whatever carries [`prelude::GenerationAnchor`]. No hand-authored layout and
//! no room definitions in Rust.
//!
//! # What a consumer touches
//!
//! | | |
//! |---|---|
//! | Setup | `add_plugins((DefaultPlugins, LdtkPlugin, GeneratorPlugin::new(path, radius)))` |
//! | Input | `GenerationAnchor` on whatever generation should follow; `Regenerate` to reroll |
//! | Output | `CatalogRes`, `LayoutRes`, and the `RoomPlaced`/`RoomSpawned`/... messages |
//! | Weights | a system returning `Vec<f32>` piped into `apply_weights`, in `GenerationSet::Weights` |
//!
//! # Guarantees
//!
//! - The **Layout is truth**; spawned entities are a cache over it. Culling
//!   never mutates the Layout, so revisited areas come back identical.
//! - A fixed seed reproduces a dungeon exactly, including after culling.
//! - Tile size is read from the project (`Catalog::grid_size`), never assumed.
//! - Nothing in this crate panics on malformed input: catalog problems arrive
//!   as `GenerationFailed`, naming the offending level.
//!
//! `DESIGN.md` is the contract this implements and `ARCHITECTURE.md` the map of
//! how; the examples are the executable version of this doc.

mod generator;
pub mod plugin;

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
    pub use crate::generator::weight::CountBy;
    pub use crate::plugin::{
        CatalogRes, CullingPolicy, DoorAbandoned, GenerationAnchor, GenerationFailed,
        GenerationOrder, GenerationSeed, GenerationSet, GenerationSettled, GenerationState,
        GeneratorPlugin, LayoutRes, OriginReach, Regenerate, RoomDespawned, RoomPlaced,
        RoomSpawned, RoomWeights, StreamingRadii, apply_weights,
    };
}

//! `bevy_ldtk_procgen` — see `DESIGN.md` and `ARCHITECTURE.md` at the crate
//! root; nothing under this crate is implemented yet.

mod generator;
pub mod plugin;

pub mod prelude {
    pub use crate::generator::catalog::{Catalog, CatalogError, FloatTagId, RoomDef, TagId};
    pub use crate::generator::error::CatalogErrors;
    pub use crate::generator::geom::{
        Dir, PlacementId, RoomId, TilePos, TileRect, TileSize, overlaps,
    };
    pub use crate::generator::layout::{Layout, open_door_pos};
    pub use crate::generator::place::StartRoom;
    pub use crate::generator::weight::CountBy;
    pub use crate::plugin::{
        CatalogRes, CullingPolicy, DoorAbandoned, GenerationAnchor, GenerationFailed,
        GenerationSet, GenerationSettled, GenerationState, GeneratorPlugin, LayoutRes,
        RoomDespawned, RoomPlaced, RoomSpawned, RoomWeights, apply_weights,
    };
}

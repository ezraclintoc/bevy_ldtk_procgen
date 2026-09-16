//! `bevy_ldtk_procgen` — see `DESIGN.md` and `ARCHITECTURE.md` at the crate
//! root; nothing under this crate is implemented yet.

mod generator;
pub mod plugin;

pub mod prelude {
    pub use crate::generator::catalog::{Catalog, FloatTagId, RoomDef, TagId};
    pub use crate::generator::geom::{Dir, PlacementId, RoomId, TilePos, TileRect, TileSize};
    pub use crate::generator::layout::Layout;
    pub use crate::generator::weight::CountBy;
    pub use crate::plugin::{
        CatalogRes, DoorAbandoned, GenerationAnchor, GenerationFailed, GenerationSet,
        GenerationSettled, GenerationState, GeneratorPlugin, LayoutRes, RoomDespawned, RoomPlaced,
        RoomSpawned,
    };
}

//! `Catalog`, `RoomDef`, `DoorDef`, `RoomId`, `TagId`, `FloatTagId`, and
//! parsing plus validation against the authoring contract. See DESIGN.md §2
//! and §6, ARCHITECTURE.md §1 and §3.
//!
//! `build_catalog` (parsing and validation) is not written yet.

use std::collections::HashMap;

use super::geom::{Dir, PixelPos, RoomId, TilePos, TileSize};

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum CatalogError {
    EmptyCatalog,
    NoStartRoomMatched {
        constraint: String,
    },
    MixedGridSize {
        level: String,
        expected: i32,
        found: i32,
    },
    LevelNotTileAligned {
        level: String,
    },
    DoorLayerMissing {
        level: String,
    },
    DoorNotOnEdge {
        level: String,
        at: TilePos,
    },
    DuplicateLevelIid {
        iid: String,
    },
    /// Warning under `Lenient`, not necessarily fatal under `Strict`.
    NoCapRoomForDirection {
        dir: Dir,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DoorDef {
    pub local_pos: TilePos,
    pub dir: Dir,
    pub width: u8,
}

/// `pub(crate)` field, same reasoning as `RoomId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TagId(pub(crate) u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FloatTagId(pub(crate) u16);

/// Tag values are stored densely, indexed by ordinal — not a `HashMap`.
#[derive(Debug, Clone)]
pub struct RoomDef {
    pub iid: String,
    pub size: TileSize,
    pub doors: Vec<DoorDef>,
    /// Integration-only: read by `plugin/systems.rs`, never by placement code.
    pub world_offset_px: PixelPos,
    bool_tags: Vec<bool>,
    float_tags: Vec<f32>,
}

impl RoomDef {
    pub fn bool_tag(&self, id: TagId) -> bool {
        self.bool_tags.get(id.0 as usize).copied().unwrap_or(false)
    }

    pub fn float_tag(&self, id: FloatTagId) -> f32 {
        self.float_tags.get(id.0 as usize).copied().unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    rooms: Vec<RoomDef>,
    bool_tag_ids: HashMap<String, TagId>,
    float_tag_ids: HashMap<String, FloatTagId>,
    grid_size: u32,
    /// Built once; only ever point-looked-up, never iterated during
    /// placement, so the `HashMap` here doesn't reintroduce §7's hazard.
    min_clearance: HashMap<(Dir, u8), TileSize>,
}

impl Catalog {
    pub fn get(&self, id: RoomId) -> Option<&RoomDef> {
        self.rooms.get(id.0 as usize)
    }

    pub fn tag_id(&self, name: &str) -> Option<TagId> {
        self.bool_tag_ids.get(name).copied()
    }

    pub fn float_tag_id(&self, name: &str) -> Option<FloatTagId> {
        self.float_tag_ids.get(name).copied()
    }

    pub fn min_clearance(&self, dir: Dir, width: u8) -> Option<TileSize> {
        self.min_clearance.get(&(dir, width)).copied()
    }

    pub fn grid_size(&self) -> u32 {
        self.grid_size
    }

    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &RoomDef> {
        self.rooms.iter()
    }
}

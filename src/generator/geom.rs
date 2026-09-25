//! `TilePos`, `TileRect`, `TileSize`, `PixelPos`, `Dir`. See ARCHITECTURE.md §2-3.

/// `pub(crate)` field: mintable only from within this crate, so an external
/// consumer can never forge one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomId(pub(crate) u32);

/// Same `pub(crate)` reasoning as `RoomId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlacementId(pub(crate) u32);

/// Handle for one entry in `Layout`'s open-door set. Stable across
/// insert/remove of *other* doors — a tombstoned-slot design (not
/// `swap_remove`), which is what makes it safe to hold onto across a frame
/// instead of only until the next door closes. See ARCHITECTURE.md §3.
///
/// Carries a generation, not just a slot index: a freed slot can be reused
/// by a brand-new door in the very next `Layout::commit` call (nothing
/// prevents it — that's the whole point of freeing a slot), so a slot index
/// alone can't tell "this door is still open" from "a different door now
/// occupies where that one used to be". The generation makes a stale id
/// compare unequal to the slot's current occupant instead of silently
/// matching it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenDoorId {
    pub(crate) slot: u32,
    pub(crate) generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TilePos {
    pub x: i32,
    pub y: i32,
}

impl std::ops::Add for TilePos {
    type Output = TilePos;

    fn add(self, rhs: TilePos) -> TilePos {
        TilePos {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileSize {
    pub width: u32,
    pub height: u32,
}

/// Pixels — only used for `RoomDef::world_offset_px`. Everything else under
/// `generator/` is tiles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelPos {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileRect {
    pub min: TilePos,
    pub size: TileSize,
}

impl TileRect {
    pub fn max(self) -> TilePos {
        TilePos {
            x: self.min.x + self.size.width as i32,
            y: self.min.y + self.size.height as i32,
        }
    }

    pub fn contains(self, pos: TilePos) -> bool {
        let max = self.max();
        pos.x >= self.min.x && pos.x < max.x && pos.y >= self.min.y && pos.y < max.y
    }
}

/// Strict `<`: two rects sharing only an edge do not overlap.
pub fn overlaps(a: TileRect, b: TileRect) -> bool {
    let (a_max, b_max) = (a.max(), b.max());
    a.min.x < b_max.x && b.min.x < a_max.x && a.min.y < b_max.y && b.min.y < a_max.y
}

/// Clockwise (not alphabetical) so `opposite()` and array indexing fall out cleanly.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    N = 0,
    E = 1,
    S = 2,
    W = 3,
}

impl Dir {
    /// Explicit match, not `(self as u8 + 2) & 3`: converting an integer
    /// back into a `#[repr(u8)]` enum needs `unsafe`, which is denied
    /// crate-wide.
    pub const fn opposite(self) -> Dir {
        match self {
            Dir::N => Dir::S,
            Dir::E => Dir::W,
            Dir::S => Dir::N,
            Dir::W => Dir::E,
        }
    }
}

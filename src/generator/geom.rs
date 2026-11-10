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

/// A tile coordinate in `generator/`'s y-up frame. Integer everywhere on
/// purpose: two integer adds replace the old float comparisons, so no epsilon
/// and no `float_cmp` (`ARCHITECTURE.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TilePos {
    /// Column, increasing to the right.
    pub x: i32,
    /// Row, increasing upward — LDtk's y-down frame is flipped once, at parse.
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

impl std::ops::Sub for TilePos {
    type Output = TilePos;

    fn sub(self, rhs: TilePos) -> TilePos {
        TilePos {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

/// A size in TILES — a room's footprint, or the extent of a clearance strip.
/// Pixels only enter the picture at the plugin boundary (`ARCHITECTURE.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileSize {
    /// Extent along x, in tiles.
    pub width: u32,
    /// Extent along y, in tiles.
    pub height: u32,
}

/// Pixels — only used for `RoomDef::world_offset_px`. Everything else under
/// `generator/` is tiles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelPos {
    pub x: f32,
    pub y: f32,
}

/// An axis-aligned rectangle in tiles: min corner plus size. One type for
/// every geometric question in the crate, replacing the old implementation's
/// three collision helpers with three different conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileRect {
    /// The corner closest to (-inf, -inf) — what a placement stores.
    pub min: TilePos,
    /// Extent from `min`, in tiles.
    pub size: TileSize,
}

impl TileRect {
    /// The exclusive far corner: the first tile *past* the rectangle. With
    /// [`overlaps`]'s strict `<`, two rooms sharing a wall this way do not
    /// overlap.
    pub fn max(self) -> TilePos {
        TilePos {
            x: self.min.x + self.size.width as i32,
            y: self.min.y + self.size.height as i32,
        }
    }

    /// Whether the tile at `pos` is inside — half-open on the far side, same
    /// convention as [`TileRect::max`].
    pub fn contains(self, pos: TilePos) -> bool {
        let max = self.max();
        pos.x >= self.min.x && pos.x < max.x && pos.y >= self.min.y && pos.y < max.y
    }

    /// Distance from `pos` to the nearest point of this rect, in tiles —
    /// `0.0` when inside. The measure spawn/cull radii are checked against
    /// (DESIGN.md §8): a room is "near" an anchor by its closest corner,
    /// not its center.
    pub fn distance_to(self, pos: TilePos) -> f32 {
        let max = self.max();
        let dx = (self.min.x - pos.x).max(pos.x - max.x).max(0) as f32;
        let dy = (self.min.y - pos.y).max(pos.y - max.y).max(0) as f32;
        (dx * dx + dy * dy).sqrt()
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
    /// Up, +y.
    N = 0,
    /// Right, +x.
    E = 1,
    /// Down, -y.
    S = 2,
    /// Left, -x.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> TileRect {
        TileRect {
            min: TilePos { x: 0, y: 0 },
            size: TileSize {
                width: 4,
                height: 3,
            },
        }
    }

    #[test]
    fn distance_to_is_zero_inside_and_on_the_edge() {
        assert_eq!(rect().distance_to(TilePos { x: 2, y: 1 }), 0.0);
        // Strict `<` overlap semantics: the max edge is outside, but the
        // continuous measure treats the boundary as touching (distance 0).
        assert_eq!(rect().distance_to(TilePos { x: 4, y: 3 }), 0.0);
    }

    #[test]
    fn distance_to_measures_axis_and_diagonal_gaps() {
        assert_eq!(rect().distance_to(TilePos { x: 7, y: 1 }), 3.0);
        assert_eq!(rect().distance_to(TilePos { x: 0, y: -4 }), 4.0);
        // 3-4-5: x is 3 past max.x, y is 4 below min.y.
        assert_eq!(rect().distance_to(TilePos { x: 7, y: -4 }), 5.0);
    }
}

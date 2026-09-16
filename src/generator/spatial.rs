//! `SpatialHash`: a uniform grid indexing both rooms and doors — two
//! instances of one generic type. See ARCHITECTURE.md §5.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use super::geom::TileRect;

#[derive(Debug, Clone)]
pub struct SpatialHash<T> {
    cell_size: u32,
    cells: HashMap<(i32, i32), Vec<T>>,
}

impl<T: Copy> SpatialHash<T> {
    pub fn new(cell_size: u32) -> Self {
        Self {
            cell_size,
            cells: HashMap::new(),
        }
    }

    pub fn insert(&mut self, rect: TileRect, value: T) {
        for cell in cells_touching(self.cell_size, rect) {
            self.cells.entry(cell).or_default().push(value);
        }
    }
}

/// `div_euclid`, not `/`: tile coordinates go negative, and plain division
/// truncates toward zero instead of flooring.
///
/// Free function, not a `&self` method: Rust 2024's `impl Trait` capture
/// rules would make the returned iterator borrow all of `self`, blocking the
/// `&mut self.cells` access `insert` needs right after.
fn cells_touching(cell_size: u32, rect: TileRect) -> impl Iterator<Item = (i32, i32)> {
    let size = cell_size as i32;
    let max = rect.max();
    let min_cx = rect.min.x.div_euclid(size);
    let min_cy = rect.min.y.div_euclid(size);
    let max_cx = (max.x - 1).div_euclid(size);
    let max_cy = (max.y - 1).div_euclid(size);
    (min_cy..=max_cy).flat_map(move |cy| (min_cx..=max_cx).map(move |cx| (cx, cy)))
}

impl<T: Copy + Eq + Hash> SpatialHash<T> {
    pub fn query(&self, rect: TileRect) -> Vec<T> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cell in cells_touching(self.cell_size, rect) {
            let Some(values) = self.cells.get(&cell) else {
                continue;
            };
            for &value in values {
                if seen.insert(value) {
                    out.push(value);
                }
            }
        }
        out
    }
}

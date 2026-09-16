//! `WeightConfig`, `PlacementCounters`, `CountBy`, and weighted candidate
//! sampling. See DESIGN.md §9, ARCHITECTURE.md §4.
//!
//! Sampling itself belongs to `place.rs` and isn't written yet.

use std::collections::HashMap;

use super::catalog::TagId;
use super::geom::RoomId;

/// A hallway is a placement too — `All` and `Tag("room")` give very
/// different answers against a hallway-heavy catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CountBy {
    All,
    Tag(String),
}

/// **Not pinned down by DESIGN.md** — §9 names `tag_transition` and its
/// effect but never gives this type's actual shape. First guess, not a
/// settled decision.
#[derive(Debug, Clone, Default)]
pub struct TransitionTable {
    multipliers: HashMap<(TagId, TagId), f32>,
}

impl TransitionTable {
    pub fn set(&mut self, from: TagId, to: TagId, multiplier: f32) {
        self.multipliers.insert((from, to), multiplier);
    }

    pub fn multiplier(&self, from: TagId, to: TagId) -> f32 {
        self.multipliers.get(&(from, to)).copied().unwrap_or(1.0)
    }
}

/// Configured with tag names; resolved to `TagId`/`FloatTagId` once against
/// the loaded Catalog, not looked up by string during placement.
#[derive(Debug, Clone, Default)]
pub struct WeightConfig {
    pub ceiling: Option<(usize, CountBy)>,
    pub tag_max: Vec<(String, usize)>,
    pub tag_min_depth: Vec<(String, u32)>,
    pub tag_transition: Option<TransitionTable>,
    pub prefer_multi_door: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PlacementCounters {
    pub placed_total: usize,
    /// One slot per `TagId` — not a `HashMap` — the entire reason bool tags
    /// are interned.
    pub tag_counts: Vec<usize>,
    pub rooms_since_tag: Vec<usize>,
    pub template_uses: Vec<u16>,
    pub recent_templates: [Option<RoomId>; 4],
    pub distance_from_start: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub room: RoomId,
    pub base_weight: f32,
    pub doors: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct PlacementContext<'a> {
    pub counters: &'a PlacementCounters,
    pub config: &'a WeightConfig,
    pub depth: u32,
    pub previous: Option<RoomId>,
}

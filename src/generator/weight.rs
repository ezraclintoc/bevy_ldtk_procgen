//! `WeightConfig`, `PlacementCounters`, `CountBy`, and weighted candidate
//! selection. See DESIGN.md §9, ARCHITECTURE.md §4.
//!
//! The sampling math itself (`sample_weighted`) lives in `place.rs`, which
//! is the only caller.
//!
//! Two weight sources combine, split by how often they change (§9):
//! - the **consumer table** (`RoomWeights`, one entry per `RoomId`), computed
//!   once per frame by a consumer system — external factors like player level
//!   belong there, never here;
//! - the **engine adjustments** in this file, evaluated fresh per placement:
//!   closure bias, multi-door bonus, `tag_max`/`tag_min_depth` hard gates.
//!   Per-placement factors cannot live in the once-per-frame table: a frame
//!   places several rooms and a table has no memory between them.

use std::collections::HashMap;

use super::catalog::{Catalog, RoomDef, TagId};
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

/// Meta-generation configuration, stated with tag *names* — resolved to
/// `TagId`s once against the loaded Catalog (`WeightConfig::resolve`), never
/// looked up by string inside the placement loop (DESIGN.md §2).
#[derive(Debug, Clone, Default)]
pub struct WeightConfig {
    pub ceiling: Option<(usize, CountBy)>,
    pub tag_max: Vec<(String, usize)>,
    pub tag_min_depth: Vec<(String, u32)>,
    pub tag_transition: Option<TransitionTable>,
    pub prefer_multi_door: bool,
}

impl WeightConfig {
    /// Resolve tag names to interned ids against `catalog`, once, when the
    /// catalog becomes available. Names matching nothing resolve to `None`
    /// and their rule is silently inert — a config naming a tag the project
    /// never declared must not fail a load (the tag set is project data).
    pub fn resolve(&self, catalog: &Catalog) -> ResolvedWeights {
        let tag_max = self
            .tag_max
            .iter()
            .filter_map(|(name, max)| catalog.tag_id(name).map(|id| (id, *max)))
            .collect();
        let tag_min_depth = self
            .tag_min_depth
            .iter()
            .filter_map(|(name, depth)| catalog.tag_id(name).map(|id| (id, *depth)))
            .collect();
        ResolvedWeights {
            tag_max,
            tag_min_depth,
            prefer_multi_door: self.prefer_multi_door,
        }
    }
}

/// `WeightConfig` with every name resolved — what the placement loop reads.
/// Built once at catalog load; immutable afterwards.
#[derive(Debug, Clone, Default)]
pub struct ResolvedWeights {
    pub tag_max: Vec<(TagId, usize)>,
    pub tag_min_depth: Vec<(TagId, u32)>,
    pub prefer_multi_door: bool,
}

/// Maintained incrementally by the engine across placements; readable by
/// built-ins and (as a snapshot, if ever needed) by consumers.
/// Field semantics per DESIGN.md §9.
#[derive(Debug, Clone, Default)]
pub struct PlacementCounters {
    pub placed_total: usize,
    /// One slot per `TagId` — not a `HashMap` — the entire reason bool tags
    /// are interned.
    pub tag_counts: Vec<usize>,
    pub rooms_since_tag: Vec<usize>,
    /// "Progress toward the ceiling" as the placement loop measures it —
    /// the number that closure bias compares against. Which placements
    /// count is `CountBy`'s whole point: `All` counts everything, `Tag`
    /// counts only rooms carrying the tag (a hallway is a placement too).
    pub progress: usize,
}

impl PlacementCounters {
    /// Sized to the catalog's `TagId` space — the whole point of interning.
    pub fn new(catalog: &Catalog) -> Self {
        Self {
            tag_counts: vec![0; catalog.tag_count()],
            rooms_since_tag: vec![0; catalog.tag_count()],
            ..Default::default()
        }
    }

    /// Records one committed placement. `progresses` says whether this
    /// placement counts toward the configured ceiling (`CountBy::All` →
    /// always; `CountBy::Tag` → only when the room carries that tag).
    /// `carried(i)` asks whether the room carries bool tag ordinal `i` —
    /// a closure because tag storage is `RoomDef`'s private concern.
    ///
    /// This is a hot-path-friendly bulk op: three field updates per tag.
    #[allow(clippy::too_many_arguments)]
    pub fn record(&mut self, tag_count: usize, carried: impl Fn(usize) -> bool, progresses: bool) {
        self.placed_total += 1;
        if progresses {
            self.progress += 1;
        }
        for i in 0..tag_count {
            let has = carried(i);
            if let Some(slot) = self.tag_counts.get_mut(i) {
                *slot += usize::from(has);
            }
            if let Some(slot) = self.rooms_since_tag.get_mut(i) {
                *slot = if has { 0 } else { slot.saturating_add(1) };
            }
        }
    }
}

/// One candidate at one door: its room, the consumer's weight for it, and
/// how many currently-open doors placing it would close (the multi-door
/// bonus's input).
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    /// Kept on the candidate so future adjustments (e.g. anti-repeat via
    /// `recent_templates`) can key on it without changing every call site.
    #[allow(dead_code)]
    pub room: RoomId,
    pub table_weight: f32,
    pub doors_closed: usize,
}

/// Everything the per-placement weight adjustment needs.
#[derive(Debug, Clone, Copy)]
pub struct WeightContext<'a> {
    pub resolved: &'a ResolvedWeights,
    pub counters: &'a PlacementCounters,
}

/// Closure-bias band start: fraction of the ceiling after which weights
/// begin shifting toward caps. Everything below is unadjusted.
const CLOSURE_START: f32 = 0.8;

/// How steep the closure bias is inside the band: door-count weights are
/// raised to `bias ** (doors - 1)` (a 1-door cap gets `bias ** 0 = 1`, a
/// 6-door hallway at full bias gets `0.5 ** 5 ≈ 0.03`). Steeper than a
/// linear scale because the frontier has a *lot* of multi-door rooms to
/// outvote.
const CLOSURE_BIAS: f32 = 0.5;

/// Per-door-closed bonus strength. A modest factor, not a hammer: with
/// 1.0, closing one extra door doubles a candidate — enough to make
/// multi-door junctions common without forcing them.
const MULTI_DOOR_FACTOR: f32 = 1.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// A 6-door room and a 1-door cap, both at table weight 1.0, weighted
    /// at several progress points of a 50-room ceiling.
    #[test]
    fn closure_bias_flips_preference_toward_caps_as_the_ceiling_approaches() {
        let resolved = ResolvedWeights::default();
        let mut counters = PlacementCounters::default();
        let multi = Candidate {
            room: RoomId(0),
            table_weight: 1.0,
            doors_closed: 0,
        };
        let single = Candidate {
            room: RoomId(1),
            table_weight: 1.0,
            doors_closed: 0,
        };
        // Door counts are read from the room's definition, so the test's
        // two candidates need definitions with 6 and 1 doors respectively;
        // synthesize them via the door-count parameter instead of real
        // RoomDefs by weighting a single def against itself — the door
        // count scaling below is what we actually assert on.
        let six_doors = 6u8;
        let one_door = 1u8;

        // Early: no bias at 40% of the ceiling.
        counters.progress = 20;
        let ctx = WeightContext {
            resolved: &resolved,
            counters: &counters,
        };
        let ceiling = Some(50);
        let w_multi_early = candidate_weight(&multi, &test_def(six_doors), &ctx, ceiling);
        let w_single_early = candidate_weight(&single, &test_def(one_door), &ctx, ceiling);

        // Late: 96% of the ceiling — deep inside the bias band.
        counters.progress = 48;
        let ctx = WeightContext {
            resolved: &resolved,
            counters: &counters,
        };
        let w_multi_late = candidate_weight(&multi, &test_def(six_doors), &ctx, ceiling);
        let w_single_late = candidate_weight(&single, &test_def(one_door), &ctx, ceiling);

        let m_early = w_multi_early.expect("multi not excluded");
        let s_early = w_single_early.expect("single not excluded");
        let m_late = w_multi_late.expect("multi not excluded");
        let s_late = w_single_late.expect("single not excluded");

        assert!(
            (m_early - s_early).abs() < 1e-5,
            "below the band both rooms weigh the same: {m_early} vs {s_early}"
        );
        assert!(
            m_late < s_late * 0.2,
            "deep in the band the 6-door room must collapse toward 0: {m_late} vs {s_late}"
        );
        assert!(
            (s_late - 1.0).abs() < 1e-5,
            "the 1-door cap keeps full weight: {s_late}"
        );
    }

    /// `candidate_weight` reads doors from the RoomDef; synthesize one via
    /// the catalog's test constructor.
    fn test_def(doors: u8) -> RoomDef {
        RoomDef::test_def(
            doors as usize,
            crate::generator::geom::Dir::N,
            crate::generator::geom::TileSize {
                width: 1,
                height: 1,
            },
            Vec::new(),
        )
    }

    /// Same, but carrying the given bool tags.
    fn test_def_tagged(doors: u8, tags: &[bool]) -> RoomDef {
        RoomDef::test_def(
            doors as usize,
            crate::generator::geom::Dir::N,
            crate::generator::geom::TileSize {
                width: 1,
                height: 1,
            },
            tags.to_vec(),
        )
    }

    #[test]
    fn tag_max_excludes_regardless_of_table_weight() {
        // Quota 0 on tag ordinal 0: any room carrying it is excluded, even
        // with an enormous consumer weight.
        let resolved = ResolvedWeights {
            tag_max: vec![(TagId(0), 0)],
            tag_min_depth: Vec::new(),
            prefer_multi_door: false,
        };
        let counters = PlacementCounters {
            tag_counts: vec![0],
            rooms_since_tag: vec![0],
            ..Default::default()
        };
        let ctx = WeightContext {
            resolved: &resolved,
            counters: &counters,
        };
        let candidate = Candidate {
            room: RoomId(0),
            table_weight: 1_000_000.0,
            doors_closed: 0,
        };
        // A def carrying tag 0.
        let def = test_def_tagged(1, &[true]);

        assert!(
            candidate_weight(&candidate, &def, &ctx, None).is_none(),
            "a quota-0 tag must exclude the candidate entirely"
        );

        // The same room not carrying the tag is fine.
        let def = test_def_tagged(1, &[false]);
        assert!(candidate_weight(&candidate, &def, &ctx, None).is_some());
    }

    #[test]
    fn multi_door_bonus_scales_with_doors_closed_and_respects_the_switch() {
        assert_eq!(multi_door_bonus(0, true), 1.0);
        assert!((multi_door_bonus(1, true) - 2.0).abs() < 1e-5);
        assert!((multi_door_bonus(3, true) - 4.0).abs() < 1e-5);
        assert_eq!(multi_door_bonus(3, false), 1.0, "disabled means off");
    }

    #[test]
    fn counters_record_tracks_progress_and_tag_recency() {
        let mut counters = PlacementCounters::default();
        counters.tag_counts = vec![0, 0];
        counters.rooms_since_tag = vec![0, 0];

        counters.record(2, |i| i == 0, true);
        counters.record(2, |i| i == 0, true);
        // Third placement does not count toward the ceiling (e.g. a Tag
        // ceiling this room doesn't carry) but still updates recency.
        counters.record(2, |_| false, false);

        assert_eq!(counters.placed_total, 3);
        assert_eq!(counters.progress, 2, "only progressing placements count");
        assert_eq!(counters.tag_counts[0], 2);
        assert_eq!(counters.rooms_since_tag[0], 1, "reset on the second carry");
        assert_eq!(counters.rooms_since_tag[1], 3, "never carried");
    }
}
/// `final = room_weights[room] * builtin_adjust(ctx, room)`).
///
/// `def` is the candidate's own definition — its doors give the door count
/// for closure bias, its tags the hard-gate lookups. `ceiling` is the
/// configured max (already converted to the same counting convention as
/// `counters.progress`); `None` disables closure bias entirely.
///
/// Returns `None` for a hard-excluded candidate (`tag_max` past its quota,
/// `tag_min_depth` below its depth) — exclusion is a gate, not weight 0.0,
/// so a table can never accidentally resurrect a room the config forbid.
///
/// Closure bias: in the last `1 - CLOSURE_START` stretch of progress
/// toward the ceiling, candidates scale by `CLOSURE_BIAS ** (doors - 1)`.
/// Single-door caps keep full weight while high-door rooms collapse — the
/// frontier shrinks as the ceiling approaches instead of being cut off
/// mid-expansion with doors left open (§9, "Ceiling and graceful closure").
pub fn candidate_weight(
    candidate: &Candidate,
    def: &RoomDef,
    ctx: &WeightContext,
    ceiling: Option<usize>,
) -> Option<f32> {
    // Hard gates first: an excluded candidate has no weight at all.
    for &(tag, max) in &ctx.resolved.tag_max {
        if def.bool_tag(tag)
            && ctx
                .counters
                .tag_counts
                .get(tag.0 as usize)
                .copied()
                .unwrap_or(0)
                >= max
        {
            return None;
        }
    }
    for &(tag, min_depth) in &ctx.resolved.tag_min_depth {
        if def.bool_tag(tag)
            && ctx.counters.progress < usize::try_from(min_depth).unwrap_or(usize::MAX)
        {
            return None;
        }
    }

    let doors = u32::from(u8::try_from(def.doors.len()).unwrap_or(u8::MAX));

    // Closure bias, only inside the final stretch of a bounded run.
    // Casts are warn-level and bounded here: progress ≤ ceiling ≤ usize
    // but realistically a few thousand; doors ≤ u8::MAX clamped further by
    // catalog sanity (DESIGN.md's lint policy accepts intentional casts).
    let closure = match ceiling {
        Some(max) if max > 0 => {
            let progress = ctx.counters.progress as f32;
            let max = max as f32;
            let over = (progress / max - CLOSURE_START).max(0.0);
            CLOSURE_BIAS.powf(over / (1.0 - CLOSURE_START) * (doors.max(1) - 1) as f32)
        }
        _ => 1.0,
    };

    let bonus = multi_door_bonus(candidate.doors_closed, ctx.resolved.prefer_multi_door);
    Some(candidate.table_weight * closure * bonus)
}

/// Multi-door preference (DESIGN.md §9, "multi-door preference"):
/// proportional to doors closed, default-on, engine-side — how many open
/// doors a candidate closes is a property of one placement decision,
/// unknowable in a once-per-frame table.
pub fn multi_door_bonus(doors_closed: usize, enabled: bool) -> f32 {
    if !enabled || doors_closed == 0 {
        return 1.0;
    }
    1.0 + doors_closed as f32 * MULTI_DOOR_FACTOR
}

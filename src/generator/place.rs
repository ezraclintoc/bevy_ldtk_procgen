//! Fills one open door: weight the eligible candidate pool, sample, try,
//! remove-and-resample on failure. See ARCHITECTURE.md §4.
//!
//! `attempt_door` is the single-door primitive — deliberately scoped no
//! wider than that. Deferred to whatever drives it across frames (the
//! `generate` system, `plugin/systems.rs`, not written yet):
//! - Which door to attempt next, and the per-frame time budget (DESIGN.md
//!   §5) — this file has no opinion on ordering across doors.
//! - The dead-door failure counter and `DoorAbandoned` (DESIGN.md §9):
//!   `attempt_door` returning `None` is "this call's full candidate pool
//!   was exhausted", i.e. one attempt in that scheme's terms — deciding
//!   whether to try again next frame or give up after N attempts is the
//!   caller's call, not this function's.
//! - Engine-side weight adjustments beyond a room's own `weight` float tag
//!   (closure bias, `tag_max`, `tag_min_depth`, `tag_transition`,
//!   multi-door preference — DESIGN.md §9) and `Catalog::min_clearance`.
//!   Placement here is still fully correct without them — overlap is
//!   checked exactly via the spatial index, not estimated — just not yet
//!   weighted or pruned by any of that.

use rand::RngExt;

use super::catalog::{Catalog, CatalogError, RoomDef};
use super::geom::{Dir, PlacementId, RoomId, TilePos, TileRect, overlaps};
use super::layout::{Layout, OpenDoor, open_door_pos};
use crate::generator::geom::OpenDoorId;

/// One tile step in the direction a door of `dir` opens toward — e.g. a
/// door facing `N` sits on a room's top edge, so the space beyond it is one
/// row further north (higher y, this crate's y-up convention).
fn step(dir: Dir) -> TilePos {
    match dir {
        Dir::N => TilePos { x: 0, y: 1 },
        Dir::S => TilePos { x: 0, y: -1 },
        Dir::E => TilePos { x: 1, y: 0 },
        Dir::W => TilePos { x: -1, y: 0 },
    }
}

/// Rooms with a door facing `dir`, width `width`, as `(RoomId, door_index)`
/// — a room can have more than one door matching, so the specific door is
/// part of the result, not just the room.
fn candidates_for(catalog: &Catalog, dir: Dir, width: u8) -> Vec<(RoomId, u8)> {
    let mut out = Vec::new();
    for &room in catalog.rooms_with_door_facing(dir) {
        let Some(def) = catalog.get(room) else {
            continue;
        };
        for (i, door) in def.doors.iter().enumerate() {
            if door.dir == dir && door.width == width {
                if let Ok(index) = u8::try_from(i) {
                    out.push((room, index));
                }
            }
        }
    }
    out
}

/// Where `candidate`'s `door_index`-th door would have to sit for its
/// footprint to align with `existing_door_pos` — both are the door
/// footprint's min corner (see `catalog.rs`'s `build_door`), one tile step
/// past it in `existing_dir`, so the candidate ends up adjacent, not
/// overlapping.
fn candidate_origin(
    catalog: &Catalog,
    candidate: RoomId,
    door_index: u8,
    existing_door_pos: TilePos,
    existing_dir: Dir,
) -> Option<TilePos> {
    let def = catalog.get(candidate)?;
    let door = def.doors.get(door_index as usize)?;
    let target = existing_door_pos + step(existing_dir);
    Some(target - door.local_pos)
}

/// Room-vs-room overlap only, via the spatial index — no door-clearance
/// check (`Catalog::min_clearance` isn't computed yet). See this file's
/// module doc.
fn fits(layout: &Layout, catalog: &Catalog, room: RoomId, origin: TilePos) -> bool {
    let Some(def) = catalog.get(room) else {
        return false;
    };
    let rect = TileRect {
        min: origin,
        size: def.size,
    };
    layout.nearby(rect).into_iter().all(|id| {
        let Some(other) = layout.get(id) else {
            return true;
        };
        let Some(other_def) = catalog.get(other.room) else {
            return true;
        };
        let other_rect = TileRect {
            min: other.origin,
            size: other_def.size,
        };
        !overlaps(rect, other_rect)
    })
}

/// Every currently-open door that `candidate`, placed at `origin`, would
/// also satisfy — not just the one it was aimed at. ARCHITECTURE.md §4:
/// "every now-satisfied open door the placed room closes is removed, not
/// only the one step 1 was filling." A linear scan over open doors, not the
/// spatial index — deferred alongside the rest of door spatial indexing
/// (`layout.rs`'s `open_doors` slot-map `TODO`).
fn satisfied_doors(
    layout: &Layout,
    catalog: &Catalog,
    candidate: RoomId,
    origin: TilePos,
) -> Vec<OpenDoorId> {
    let Some(def) = catalog.get(candidate) else {
        return Vec::new();
    };

    let mut satisfied = Vec::new();
    for (open_id, open) in layout.open_doors() {
        let Some(open_pos) = open_door_pos(layout, catalog, *open) else {
            continue;
        };
        let Some(open_placement) = layout.get(open.placement) else {
            continue;
        };
        let Some(open_def) = catalog.get(open_placement.room) else {
            continue;
        };
        let Some(open_door_def) = open_def.doors.get(open.door as usize) else {
            continue;
        };

        // Two doors connect when they're exactly one tile apart, facing
        // opposite ways - never at the same tile. `candidate_origin` solved
        // for `existing_door_pos + step(existing_dir) == candidate_door_pos`,
        // so checking equality here has to account for that same step, the
        // same bug `attempt_door`'s own test caught: this used to compare
        // `origin + d.local_pos == open_pos` directly, which can never be
        // true by construction.
        let matches = def.doors.iter().any(|d| {
            let candidate_world_pos = origin + d.local_pos;
            candidate_world_pos + step(d.dir) == open_pos && d.dir == open_door_def.dir.opposite()
        });
        if matches {
            satisfied.push(open_id);
        }
    }
    satisfied
}

fn sample_weighted<R: RngExt>(rng: &mut R, weights: &[f32]) -> Option<usize> {
    let total: f32 = weights.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let mut draw = rng.random_range(0.0..total);
    for (i, &w) in weights.iter().enumerate() {
        if draw < w {
            return Some(i);
        }
        draw -= w;
    }
    weights.len().checked_sub(1)
}

/// Attempts to fill one open door: weight every eligible candidate by its
/// `weight` float tag (absent from the catalog entirely, not just from one
/// room, means every candidate weighs `1.0` — see `catalog.rs`), sample,
/// try to place it, and on failure remove that candidate and resample from
/// what remains — the fix for the old `find_bridging_room` stall
/// (`DESIGN.md`, "Resolved during design"), which tried exactly one
/// candidate and gave up.
///
/// On success, `layout` already reflects it: the placement is committed,
/// `door_id` and every other open door the new room happens to satisfy are
/// closed, and the new room's remaining doors are open. Returns the new
/// `PlacementId` — not just the `RoomId` — since the caller (`generate`,
/// `plugin/systems.rs`) needs the placement's origin too, for `RoomPlaced`.
///
/// `None` means the candidate pool was exhausted without a fit — this is
/// one full attempt in ARCHITECTURE.md §4's terms, not a permanent failure;
/// deciding whether to retry, and how many attempts before giving up on the
/// door, is the caller's job (this file's module doc).
pub fn attempt_door<R: RngExt>(
    layout: &mut Layout,
    catalog: &Catalog,
    door_id: OpenDoorId,
    rng: &mut R,
) -> Option<PlacementId> {
    let door: OpenDoor = layout
        .open_doors()
        .find(|(id, _)| *id == door_id)
        .map(|(_, d)| *d)?;
    let pos = open_door_pos(layout, catalog, door)?;

    let placement = layout.get(door.placement)?;
    let existing_def = catalog.get(placement.room)?;
    let existing_door = existing_def.doors.get(door.door as usize)?;
    let existing_dir = existing_door.dir;
    let width = existing_door.width;

    let weight_tag = catalog.float_tag_id("weight");
    let mut candidates = candidates_for(catalog, existing_dir.opposite(), width);

    while !candidates.is_empty() {
        let weights: Vec<f32> = candidates
            .iter()
            .map(|(room, _)| {
                let base = catalog
                    .get(*room)
                    .and_then(|def| weight_tag.map(|id| def.float_tag(id)))
                    .unwrap_or(1.0);
                base.max(0.0)
            })
            .collect();

        let Some(pick) = sample_weighted(rng, &weights) else {
            break;
        };
        let Some(&(room, door_index)) = candidates.get(pick) else {
            break;
        };

        let placed = candidate_origin(catalog, room, door_index, pos, existing_dir)
            .filter(|&origin| fits(layout, catalog, room, origin))
            .and_then(|origin| {
                let closes = satisfied_doors(layout, catalog, room, origin);
                layout.commit(catalog, room, origin, &closes)
            });

        if placed.is_some() {
            return placed;
        }

        candidates.swap_remove(pick);
    }

    None
}

/// Which room generation begins from. DESIGN.md §2, "Starting room": the
/// generator does not require a designated spawn room — requiring one would
/// mean a five-room test project fails to load until something is tagged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StartRoom {
    /// Weighted pick from the whole catalog (the default).
    #[default]
    Any,
    /// Weighted pick among rooms carrying this bool tag.
    Tagged(String),
    /// The level with this LDtk identifier.
    Named(String),
}

impl StartRoom {
    /// For `CatalogError::NoStartRoomMatched { constraint }` — the error is
    /// only actionable if it names the constraint that nothing satisfied.
    fn describe(&self) -> String {
        match self {
            StartRoom::Any => "any room".to_string(),
            StartRoom::Tagged(tag) => format!("a room tagged {tag:?}"),
            StartRoom::Named(name) => format!("a level named {name:?}"),
        }
    }
}

/// Every room `start` admits, in `Catalog::iter` order — i.e. `RoomId`
/// order, since IDs are catalog indices.
fn start_candidates<'a>(catalog: &'a Catalog, start: &StartRoom) -> Vec<(RoomId, &'a RoomDef)> {
    match start {
        StartRoom::Any => catalog
            .iter()
            .enumerate()
            .map(|(i, def)| (RoomId(i as u32), def))
            .collect(),
        StartRoom::Tagged(name) => {
            let Some(tag) = catalog.tag_id(name) else {
                return Vec::new();
            };
            catalog
                .iter()
                .enumerate()
                .filter(|(_, def)| def.bool_tag(tag))
                .map(|(i, def)| (RoomId(i as u32), def))
                .collect()
        }
        StartRoom::Named(name) => catalog
            .iter()
            .enumerate()
            .filter(|(_, def)| def.identifier == *name)
            .map(|(i, def)| (RoomId(i as u32), def))
            .collect(),
    }
}

/// Picks and commits the very first placement, centered on `anchor` — the
/// dungeon begins where the anchor (player, camera) is, and generation
/// proceeds outward from there (`generate`, `plugin/systems.rs`, calls this
/// only while the Layout is empty). Selection is a weighted pick over the
/// rooms `start` admits, by the same `weight` float tag `attempt_door` uses.
///
/// `NoStartRoomMatched` is conditional exactly as DESIGN.md §2 says: under
/// `StartRoom::Any` the candidate pool is the whole catalog, so the error
/// only arises when a `Tagged`/`Named` constraint matches nothing.
pub fn place_start_room<R: RngExt>(
    layout: &mut Layout,
    catalog: &Catalog,
    start: &StartRoom,
    anchor: TilePos,
    rng: &mut R,
) -> Result<PlacementId, CatalogError> {
    let candidates = start_candidates(catalog, start);
    let no_match = || CatalogError::NoStartRoomMatched {
        constraint: start.describe(),
    };

    let weight_tag = catalog.float_tag_id("weight");
    let weights: Vec<f32> = candidates
        .iter()
        .map(|(_, def)| {
            let base = weight_tag.map(|id| def.float_tag(id)).unwrap_or(1.0);
            base.max(0.0)
        })
        .collect();

    // Uniform fallback when every weight is zero: a zero weight should
    // mean "unselectable in preference", not "the dungeon refuses to
    // start" — especially since DESIGN.md §2 promises `NoStartRoomMatched`
    // cannot arise under `Any`. (`random_range` panics on an empty range,
    // hence the emptiness guard — this crate denies panics.)
    let pick = sample_weighted(rng, &weights).or_else(|| {
        if candidates.is_empty() {
            None
        } else {
            Some(rng.random_range(0..candidates.len()))
        }
    });
    let Some(&(room, def)) = pick.and_then(|i| candidates.get(i)) else {
        return Err(no_match());
    };

    let origin = TilePos {
        x: anchor.x - (def.size.width as i32) / 2,
        y: anchor.y - (def.size.height as i32) / 2,
    };

    // The Layout is empty here, so nothing can overlap and no door can be
    // closed — the start room only opens its own doors. `commit`'s `None`
    // is a `RoomId` from a different Catalog, unreachable by construction
    // (the id above was minted by enumerating this very catalog).
    layout
        .commit(catalog, room, origin, &[])
        .ok_or_else(no_match)
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::SmallRng;

    use super::*;
    use crate::generator::catalog::build_catalog;
    use crate::generator::geom::overlaps;

    fn load_catalog() -> Catalog {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk"
        );
        let raw = std::fs::read_to_string(path).expect("fixture must exist");
        let project: bevy_ecs_ldtk::ldtk::LdtkJson =
            serde_json::from_str(&raw).expect("fixture must be valid LDtk JSON");
        build_catalog(&project).expect("bundled catalog should validate")
    }

    fn room_1() -> RoomId {
        RoomId(0)
    }

    #[test]
    fn step_points_one_tile_away_from_each_direction() {
        assert_eq!(step(Dir::N), TilePos { x: 0, y: 1 });
        assert_eq!(step(Dir::S), TilePos { x: 0, y: -1 });
        assert_eq!(step(Dir::E), TilePos { x: 1, y: 0 });
        assert_eq!(step(Dir::W), TilePos { x: -1, y: 0 });
    }

    #[test]
    fn candidate_origin_aligns_footprints_exactly() {
        // Hand-derived the same way as catalog.rs's local_pos test: place
        // Room_Spawn_1 (12x8, S-door local_pos=(8,0)) at the origin, then
        // compute where a second copy of it must go so its own N-door
        // (local_pos=(7,7)) lands exactly one tile south of the first
        // room's S-door footprint.
        let catalog = load_catalog();
        let room = room_1();
        let def = catalog.get(room).expect("room 0 must exist");
        let s_door = def
            .doors
            .iter()
            .find(|d| d.dir == Dir::S)
            .expect("room 0 must have a south door for this test to mean anything");
        let n_door_index = def
            .doors
            .iter()
            .position(|d| d.dir == Dir::N)
            .expect("room 0 must have a north door for this test to mean anything");

        let existing_door_pos = TilePos { x: 0, y: 0 } + s_door.local_pos;
        let origin = candidate_origin(
            &catalog,
            room,
            u8::try_from(n_door_index).unwrap(),
            existing_door_pos,
            Dir::S,
        )
        .expect("candidate_origin must succeed for a real catalog room");

        let n_door = def
            .doors
            .iter()
            .find(|d| d.dir == Dir::N)
            .expect("checked above");
        let placed_footprint = origin + n_door.local_pos;
        let target = existing_door_pos + step(Dir::S);
        assert_eq!(
            placed_footprint, target,
            "the candidate's own N-door footprint must land exactly on the target tile"
        );
    }

    #[test]
    fn attempt_door_fills_a_door_and_closes_it() {
        let catalog = load_catalog();
        let mut layout = Layout::default();
        let mut rng = SmallRng::seed_from_u64(1234);

        layout
            .commit(&catalog, room_1(), TilePos { x: 0, y: 0 }, &[])
            .expect("initial commit must succeed");
        let before_open = layout.open_doors().count();
        assert!(before_open > 0, "room 0 must have at least one open door");

        let (door_id, _) = layout
            .open_doors()
            .next()
            .expect("just verified at least one is open");

        let placed = attempt_door(&mut layout, &catalog, door_id, &mut rng);
        assert!(
            placed.is_some(),
            "at least one candidate should fit against an otherwise-empty layout"
        );
        assert_eq!(layout.placed_count(), 2);
        assert!(
            layout.open_doors().all(|(id, _)| id != door_id),
            "the door that was filled must no longer be open"
        );
    }

    #[test]
    fn repeated_attempts_grow_the_layout_without_overlap() {
        let catalog = load_catalog();
        let mut layout = Layout::default();
        let mut rng = SmallRng::seed_from_u64(42);

        layout
            .commit(&catalog, room_1(), TilePos { x: 0, y: 0 }, &[])
            .expect("initial commit must succeed");

        // Breadth-first over whatever's open at the start of each round -
        // re-querying open_doors() every iteration rather than assuming a
        // fixed count, since each successful fill changes it.
        for _ in 0..5 {
            let round: Vec<OpenDoorId> = layout.open_doors().map(|(id, _)| id).collect();
            if round.is_empty() {
                break;
            }
            for door_id in round {
                // A door from an earlier iteration in this same round may
                // already be closed by this point (multi-door closing) -
                // attempt_door handles a stale id by simply finding
                // nothing, not by panicking.
                attempt_door(&mut layout, &catalog, door_id, &mut rng);
            }
        }

        assert!(
            layout.placed_count() > 1,
            "at least one door should have been filled across five rounds"
        );

        // Independent check, not reusing `fits`: every pair of placements'
        // real rects, straight from Catalog::get, must not overlap.
        let placements: Vec<(RoomId, TilePos)> =
            layout.iter().map(|(_, p)| (p.room, p.origin)).collect();
        for (i, &(room_a, origin_a)) in placements.iter().enumerate() {
            let def_a = catalog.get(room_a).expect("placed room must exist");
            let rect_a = TileRect {
                min: origin_a,
                size: def_a.size,
            };
            for &(room_b, origin_b) in placements.iter().skip(i + 1) {
                let def_b = catalog.get(room_b).expect("placed room must exist");
                let rect_b = TileRect {
                    min: origin_b,
                    size: def_b.size,
                };
                assert!(
                    !overlaps(rect_a, rect_b),
                    "placements {rect_a:?} and {rect_b:?} must not overlap"
                );
            }
        }
    }

    #[test]
    fn start_room_any_places_centered_on_the_anchor() {
        let catalog = load_catalog();
        let mut layout = Layout::default();
        let mut rng = SmallRng::seed_from_u64(42);
        let anchor = TilePos { x: 10, y: 7 };

        let id = place_start_room(&mut layout, &catalog, &StartRoom::Any, anchor, &mut rng)
            .expect("Any must always match");

        let placement = layout.get(id).expect("just placed");
        let def = catalog
            .get(placement.room)
            .expect("placed room must come from this catalog");
        assert_eq!(
            placement.origin,
            TilePos {
                x: anchor.x - (def.size.width as i32) / 2,
                y: anchor.y - (def.size.height as i32) / 2,
            },
            "the start room must be centered on the anchor"
        );
        assert_eq!(layout.placed_count(), 1);
    }

    #[test]
    fn start_room_named_matches_the_level_identifier() {
        let catalog = load_catalog();
        let identifier = catalog
            .iter()
            .next()
            .expect("fixture has rooms")
            .identifier
            .clone();
        let mut layout = Layout::default();
        let mut rng = SmallRng::seed_from_u64(1);

        let id = place_start_room(
            &mut layout,
            &catalog,
            &StartRoom::Named(identifier.clone()),
            TilePos { x: 0, y: 0 },
            &mut rng,
        )
        .expect("a real level identifier must match");

        let placement = layout.get(id).expect("just placed");
        let def = catalog
            .get(placement.room)
            .expect("placed room must come from this catalog");
        assert_eq!(def.identifier, identifier);
    }

    #[test]
    fn start_room_named_that_matches_nothing_reports_no_start_room_matched() {
        let catalog = load_catalog();
        let mut layout = Layout::default();
        let mut rng = SmallRng::seed_from_u64(1);

        let err = place_start_room(
            &mut layout,
            &catalog,
            &StartRoom::Named("no such level".to_string()),
            TilePos { x: 0, y: 0 },
            &mut rng,
        )
        .expect_err("an unknown level identifier must not place anything");

        assert!(matches!(err, CatalogError::NoStartRoomMatched { .. }));
        assert_eq!(layout.placed_count(), 0);
    }

    #[test]
    fn start_room_tagged_without_the_tag_reports_no_start_room_matched() {
        // The bundled catalog predates the tags redesign (DESIGN.md §2) -
        // no bool tags exist at all, so nothing can carry "boss".
        let catalog = load_catalog();
        let mut layout = Layout::default();
        let mut rng = SmallRng::seed_from_u64(1);

        let err = place_start_room(
            &mut layout,
            &catalog,
            &StartRoom::Tagged("boss".to_string()),
            TilePos { x: 0, y: 0 },
            &mut rng,
        )
        .expect_err("an unknown tag must not place anything");

        assert!(matches!(err, CatalogError::NoStartRoomMatched { .. }));
        assert_eq!(layout.placed_count(), 0);
    }

    #[test]
    fn start_room_reproduces_for_a_fixed_seed() {
        let catalog = load_catalog();
        let anchor = TilePos { x: 3, y: -2 };

        let mut first = Layout::default();
        let mut second = Layout::default();
        for layout in [&mut first, &mut second] {
            let mut rng = SmallRng::seed_from_u64(7);
            place_start_room(layout, &catalog, &StartRoom::Any, anchor, &mut rng)
                .expect("Any must always match");
        }

        let a = first.iter().next().expect("just placed").1;
        let b = second.iter().next().expect("just placed").1;
        assert_eq!(*a, *b, "same seed must reproduce the same start placement");
    }
}

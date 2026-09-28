//! `enforce_ldtk_settings`, and `generate` — the synchronous placement
//! driver, ARCHITECTURE.md §7. `sync_entities` (spawn/cull queue drain)
//! isn't written yet.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::generator::catalog::Catalog;
use crate::generator::geom::{OpenDoorId, TilePos};
use crate::generator::layout::{Layout, open_door_dir, open_door_pos};
use crate::generator::place::attempt_door;
use crate::generator::weight::CountBy;

use super::events::{DoorAbandoned, RoomPlaced};
use super::{CatalogRes, GenerationAnchor, LayoutRes};

/// After this many exhausted attempts (`attempt_door` returning `None`,
/// each one a full candidate-pool exhaustion in `place.rs`'s terms), a door
/// is abandoned rather than retried forever. See DESIGN.md §9, "Dead doors".
const DEAD_DOOR_THRESHOLD: u32 = 3;

/// Simple per-frame room cap standing in for `DESIGN.md` §5's full
/// `GenerationBudget` (`Immediate`/`RoomsPerFrame`/`TimeBudget`), which
/// isn't wired into `GeneratorPlugin` yet — this is a fixed number, not the
/// final API.
const ROOMS_PER_FRAME: usize = 10;

#[derive(Resource, Deref, DerefMut)]
pub(crate) struct GenRng(pub SmallRng);

impl GenRng {
    pub(crate) fn new(seed: Option<u64>) -> Self {
        match seed {
            Some(seed) => Self(SmallRng::seed_from_u64(seed)),
            None => Self(SmallRng::from_rng(&mut rand::rng())),
        }
    }
}

/// `generation_radius` and `max_rooms`, plumbed from `GeneratorPlugin`'s own
/// fields — previously discarded entirely in `Plugin::build`
/// (`let _ = (self.generation_radius, ...)`).
#[derive(Resource, Clone)]
pub(crate) struct GenerationConfig {
    pub generation_radius: f32,
    pub max_rooms: Option<(usize, CountBy)>,
}

/// Failure count per door. Point-looked-up and incremented by exact key,
/// never iterated as a whole during placement, so this `HashMap` doesn't
/// reintroduce the iteration-order hazard `DESIGN.md` §7 warns about — same
/// reasoning as `Catalog::min_clearance`.
#[derive(Resource, Default, Deref, DerefMut)]
pub(crate) struct DoorAttempts(HashMap<OpenDoorId, u32>);

/// Force-set, not a blind `insert_resource` in `Plugin::build`: this must
/// win even if the consumer sets `LdtkSettings` themselves for unrelated
/// fields after adding the plugin. See ARCHITECTURE.md §2.
pub(crate) fn enforce_ldtk_settings(mut settings: ResMut<LdtkSettings>) {
    settings.level_spawn_behavior = LevelSpawnBehavior::UseWorldTranslation {
        load_level_neighbors: false,
    };
}

fn room_count(catalog: &Catalog, layout: &Layout, by: &CountBy) -> usize {
    match by {
        CountBy::All => layout.placed_count(),
        CountBy::Tag(name) => {
            let Some(tag_id) = catalog.tag_id(name) else {
                return 0;
            };
            layout
                .iter()
                .filter(|(_, p)| catalog.get(p.room).is_some_and(|def| def.bool_tag(tag_id)))
                .count()
        }
    }
}

/// Doors currently open within `radius_tiles` of any anchor, nearest-first
/// — `DESIGN.md` §3's locality invariant: every door within
/// `generation_radius` is eligible before any door outside it. Distance is
/// to the *nearest* anchor, a union over however many there are (`DESIGN.md`
/// §3, "multiple anchors").
fn doors_by_distance(
    layout: &Layout,
    catalog: &Catalog,
    anchors: &[TilePos],
    radius_tiles: f32,
) -> Vec<OpenDoorId> {
    let mut candidates: Vec<(OpenDoorId, f32)> = Vec::new();
    for (id, door) in layout.open_doors() {
        let Some(pos) = open_door_pos(layout, catalog, *door) else {
            continue;
        };
        let nearest = anchors
            .iter()
            .map(|a| {
                let dx = (a.x - pos.x) as f32;
                let dy = (a.y - pos.y) as f32;
                (dx * dx + dy * dy).sqrt()
            })
            .fold(f32::INFINITY, f32::min);
        if nearest <= radius_tiles {
            candidates.push((id, nearest));
        }
    }
    candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    candidates.into_iter().map(|(id, _)| id).collect()
}

fn abandon_door(
    layout: &mut Layout,
    catalog: &Catalog,
    door_id: OpenDoorId,
    abandoned_events: &mut MessageWriter<DoorAbandoned>,
) {
    let Some((_, &door)) = layout.open_doors().find(|(id, _)| *id == door_id) else {
        return;
    };
    let at = open_door_pos(layout, catalog, door);
    let dir = open_door_dir(layout, catalog, door);
    let (Some(at), Some(dir)) = (at, dir) else {
        return;
    };
    layout.close_door(door_id);
    abandoned_events.write(DoorAbandoned { at, dir });
}

/// The synchronous placement driver (ARCHITECTURE.md §7): runs
/// `place::attempt_door` over open doors within `generation_radius`,
/// nearest-to-anchor first, up to `ROOMS_PER_FRAME` placements. No
/// `Commands`, no entity queries — `sync_entities` (not written yet) is
/// where spawning happens, kept separate so this stays trivially testable.
///
/// Meta-generation (`RoomWeights`, `DESIGN.md` §9) isn't wired in yet:
/// every candidate currently weighs its own `weight` float tag only, via
/// `place::attempt_door` directly.
pub(crate) fn generate(
    catalog: Res<CatalogRes>,
    mut layout: ResMut<LayoutRes>,
    mut rng: ResMut<GenRng>,
    mut attempts: ResMut<DoorAttempts>,
    config: Res<GenerationConfig>,
    anchors: Query<&GlobalTransform, With<GenerationAnchor>>,
    mut placed_events: MessageWriter<RoomPlaced>,
    mut abandoned_events: MessageWriter<DoorAbandoned>,
) {
    if let Some((max, by)) = &config.max_rooms {
        if room_count(&catalog, &layout, by) >= *max {
            return;
        }
    }

    let grid_size = catalog.grid_size() as f32;
    if grid_size <= 0.0 {
        return;
    }

    // DESIGN.md §3: zero anchors -> idle, not an error. Position-sorting
    // the anchors themselves doesn't change this function's behaviour
    // (nearest-distance is commutative over the set), so it's skipped here
    // despite DESIGN.md §3's general anchor-ordering note.
    let anchor_tiles: Vec<TilePos> = anchors
        .iter()
        .map(|t| {
            let px = t.translation().truncate();
            TilePos {
                x: (px.x / grid_size).floor() as i32,
                y: (px.y / grid_size).floor() as i32,
            }
        })
        .collect();
    if anchor_tiles.is_empty() {
        return;
    }

    let radius_tiles = config.generation_radius / grid_size;
    let ordered = doors_by_distance(&layout, &catalog, &anchor_tiles, radius_tiles);

    let mut placed_this_frame = 0;
    for door_id in ordered {
        if placed_this_frame >= ROOMS_PER_FRAME {
            break;
        }
        // An earlier iteration this same frame may already have closed this
        // door via multi-door satisfaction (place.rs's satisfied_doors).
        if layout.open_doors().all(|(id, _)| id != door_id) {
            continue;
        }

        match attempt_door(&mut layout, &catalog, door_id, &mut rng.0) {
            Some(placement_id) => {
                placed_this_frame += 1;
                attempts.remove(&door_id);
                if let Some(placement) = layout.get(placement_id) {
                    placed_events.write(RoomPlaced {
                        room: placement.room,
                        at: placement.origin,
                    });
                }
            }
            None => {
                let failures = attempts.entry(door_id).or_insert(0);
                *failures += 1;
                if *failures >= DEAD_DOOR_THRESHOLD {
                    attempts.remove(&door_id);
                    abandon_door(&mut layout, &catalog, door_id, &mut abandoned_events);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::catalog::build_catalog;

    /// `room_count`/`doors_by_distance` are pure functions over `&Catalog`/
    /// `&Layout` despite living in the Bevy-facing file, so this uses the
    /// same real-catalog-fixture pattern as `generator/`'s own tests.
    /// `generate` itself isn't tested here — it needs a real `App` to
    /// exercise (asset loading, multiple frame ticks), which is a slower,
    /// different kind of test than the rest of this crate has so far; that
    /// gap is real, not silently skipped.
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

    #[test]
    fn room_count_all_matches_placed_count() {
        let catalog = load_catalog();
        let mut layout = Layout::default();
        assert_eq!(room_count(&catalog, &layout, &CountBy::All), 0);

        layout
            .commit(
                &catalog,
                crate::generator::geom::RoomId(0),
                TilePos { x: 0, y: 0 },
                &[],
            )
            .expect("commit must succeed");
        assert_eq!(room_count(&catalog, &layout, &CountBy::All), 1);
    }

    #[test]
    fn room_count_by_tag_is_zero_when_the_catalog_has_no_such_tag() {
        // The bundled catalog predates the tags redesign (DESIGN.md §2) -
        // no bool tags exist at all, so this must return 0 rather than
        // panicking or treating "tag not found" as "count everything".
        let catalog = load_catalog();
        let mut layout = Layout::default();
        layout
            .commit(
                &catalog,
                crate::generator::geom::RoomId(0),
                TilePos { x: 0, y: 0 },
                &[],
            )
            .expect("commit must succeed");

        assert_eq!(
            room_count(&catalog, &layout, &CountBy::Tag("boss".to_string())),
            0
        );
    }

    #[test]
    fn doors_by_distance_filters_by_radius_and_sorts_nearest_first() {
        let catalog = load_catalog();
        let mut layout = Layout::default();
        let room = crate::generator::geom::RoomId(0);
        layout
            .commit(&catalog, room, TilePos { x: 0, y: 0 }, &[])
            .expect("commit must succeed");

        // An anchor far to the east: doors nearer the east edge should sort
        // first, and a small enough radius should exclude the far side
        // entirely.
        let anchor = TilePos { x: 100, y: 4 };

        let unlimited = doors_by_distance(&layout, &catalog, &[anchor], f32::INFINITY);
        assert_eq!(
            unlimited.len(),
            layout.open_doors().count(),
            "an infinite radius must include every open door"
        );

        let none_in_range = doors_by_distance(&layout, &catalog, &[anchor], 0.0);
        assert!(
            none_in_range.is_empty(),
            "a zero radius must exclude every door that isn't exactly on the anchor's tile"
        );

        // Nearest-first: the first id in the unlimited-radius result must be
        // the actual closest door, checked independently of doors_by_distance
        // itself by recomputing distance straight from open_door_pos.
        let closest_independently = layout
            .open_doors()
            .filter_map(|(id, door)| {
                let pos = open_door_pos(&layout, &catalog, *door)?;
                let dx = (anchor.x - pos.x) as f32;
                let dy = (anchor.y - pos.y) as f32;
                Some((id, (dx * dx + dy * dy).sqrt()))
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .map(|(id, _)| id);

        assert_eq!(unlimited.first().copied(), closest_independently);
    }
}

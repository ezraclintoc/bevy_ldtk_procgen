//! `enforce_ldtk_settings`, `generate` (the synchronous placement driver,
//! ARCHITECTURE.md §7), and `sync_entities` (the spawn/cull queue drain).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::generator::catalog::{Catalog, DoorSource, RoomDef, TagId};
use crate::generator::error::CatalogErrors;
use crate::generator::frontier::OriginFrontier;
use crate::generator::geom::{OpenDoorId, PlacementId, RoomId, TilePos, TileRect, TileSize};
use crate::generator::layout::{Layout, open_door_dir, open_door_pos};
use crate::generator::place::{StartRoom, WeightParams, attempt_door, place_start_room};
use crate::generator::weight::{CountBy, PlacementCounters, ResolvedWeights, WeightContext};

use super::events::{
    DoorAbandoned, GenerationFailed, Regenerate, RoomDespawned, RoomPlaced, RoomSpawned,
};
use super::load::{GenerationState, LdtkHandle};
use super::{
    CatalogRes, CullingPolicy, GenerationAnchor, GenerationOrder, GenerationSeed, LayoutRes,
    OriginReach, SpawnBudget, SpawnRate, StreamingRadii,
};

/// After this many exhausted attempts (`attempt_door` returning `None`,
/// each one a full candidate-pool exhaustion in `place.rs`'s terms), a door
/// is abandoned rather than retried forever. See DESIGN.md §9, "Dead doors".
///
/// An exhausted attempt is not proof of permanent blockage: when the
/// frontier around a door is still growing, candidates fail only
/// temporarily and the door becomes fillable once the neighbourhood
/// settles. Measured on the bundled catalog (seed 1234): every door fills
/// eventually, but a door near churn can stay unfillable for dozens of
/// rounds — 3 strikes abandoned 6 fillable doors and capped the dungeon at
/// 41 rooms, while a high threshold lets the same run reach 57. DESIGN.md
/// §9's "repeated" is therefore dozens, not a handful; the real fix is the
/// retry-suppression note below, and a premature abandon is strictly worse
/// than a bounded re-check (the check is cheap: one candidate scan).
const DEAD_DOOR_THRESHOLD: u32 = 300;

/// Per-frame placement budgets, standing in for `DESIGN.md` §5's
/// `GenerationBudget` (not yet public API). Placement measures ~10 µs per
/// room independent of dungeon size.
///
/// `NearestAnchor` is capped by COUNT: the anchor moves between frames, so
/// how many rooms a frame places changes which doors are nearest next — a
/// time budget would make a fixed-seed run depend on machine speed.
const ROOMS_PER_FRAME: usize = 64;

/// `FromOrigin` is capped by TIME (~200 rooms a frame), enough for its disc
/// to keep pace with a fast anchor. That costs it nothing: its sequence
/// doesn't depend on how many rooms each frame gets through.
const GENERATION_BUDGET: Duration = Duration::from_millis(2);

/// Hard cap alongside [`GENERATION_BUDGET`], so a coarse clock can't let one
/// frame run away.
const MAX_PLACEMENTS_PER_FRAME: usize = 512;

/// The consumer's once-per-frame weight table (DESIGN.md §9). Written by a
/// consumer system piped into the public [`apply_weights`] — never mutated
/// by the engine — and read by `generate` when weighting candidates.
///
/// Output index `i` corresponds to `RoomId(i)`. A table shorter than the
/// catalog is legal: missing entries fall back to each room's own `weight`
/// float tag (the `apply_weights` doc explains why this is a fallback
/// rather than an error). `None` (the default) means no consumer table —
/// every candidate weighs its base weight only.
///
/// [`apply_weights`]: crate::apply_weights
#[derive(Resource, Default, Deref, DerefMut)]
pub struct RoomWeights(pub Option<Vec<f32>>);

/// The pipe target consumers attach their weight system to (DESIGN.md §9):
///
/// ```ignore
/// app.add_systems(Update, my_weights.pipe(apply_weights).in_set(GenerationSet::Weights));
/// ```
///
/// Stores the table for `generate` to read. A table shorter than the
/// catalog is stored as-is — `generate` treats missing tail entries as the
/// rooms' base weight — so this can never index out of bounds and never
/// panics (`AGENTS.md` lint policy); the consumer gets the correct-for-the-
/// prefix behaviour rather than a hard failure, because a length mismatch
/// is a config bug worth tolerating gracefully, not corrupting state over.
pub fn apply_weights(In(table): In<Vec<f32>>, mut room_weights: ResMut<RoomWeights>) {
    *room_weights = RoomWeights(Some(table));
}

/// Above this many spatial-hash cells, a "what's near the anchor" query is
/// answered by a plain scan instead: a huge radius covers more cells than
/// there are things to find.
fn scan_is_cheaper(rect: TileRect, cell_size: u32, items: usize) -> bool {
    let cell = f64::from(cell_size.max(1));
    let cells =
        (f64::from(rect.size.width) / cell + 1.0) * (f64::from(rect.size.height) / cell + 1.0);
    cells > items as f64
}

/// The square of tiles within `radius` of `center`, or `None` if the radius
/// is too large to express (or not finite) — the caller then scans.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // range-checked
fn square_around(center: TilePos, radius: f32) -> Option<TileRect> {
    if !radius.is_finite() || !(0.0..1.0e6).contains(&radius) {
        return None;
    }
    let r = radius.ceil() as u32;
    Some(
        TileRect {
            min: center,
            size: TileSize {
                width: 1,
                height: 1,
            },
        }
        .inflate(r),
    )
}

/// Deterministic generation's frontier. The origin is tile (0, 0), so
/// `Default` is the real initial state.
#[derive(Resource, Default)]
pub(crate) struct OriginFrontierRes(pub OriginFrontier);

/// Under `CullingPolicy::Disabled` every placement spawns, in order; this is
/// how many already have, so the spawn pass doesn't rescan them each frame.
#[derive(Resource, Default)]
pub(crate) struct SpawnCursor(usize);

#[derive(Resource, Deref, DerefMut)]
pub(crate) struct GenRng(pub SmallRng);

impl GenRng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(SmallRng::seed_from_u64(seed))
    }
}

/// The seed to generate from: the one asked for, or a fresh random one.
/// Drawn as a number rather than seeding the RNG from entropy directly, so
/// even a random dungeon has a seed to report and rebuild (`GenerationSeed`).
pub(crate) fn resolve_seed(seed: Option<u64>) -> u64 {
    seed.unwrap_or_else(rand::random)
}

/// `generation_radius`, `max_rooms`, and `start`, plumbed from
/// `GeneratorPlugin`'s own fields — previously discarded entirely in
/// `Plugin::build` (`let _ = (self.generation_radius, ...)`). `weights`
/// starts unresolved (empty) and is filled in by `poll_ldtk_load` once the
/// catalog exists — tag names resolve against the loaded catalog, once
/// (DESIGN.md §2).
/// `generation_radius`, `max_rooms`, and `start`, plumbed from
/// `GeneratorPlugin`'s own fields — previously discarded entirely in
/// `Plugin::build` (`let _ = (self.generation_radius, ...)`). `weights`
/// starts unresolved (empty) and is filled in by `poll_ldtk_load` once the
/// catalog exists — tag names resolve against the loaded catalog, once
/// (DESIGN.md §2).
#[derive(Resource, Clone)]
pub(crate) struct GenerationConfig {
    pub door_source: DoorSource,
    pub max_rooms: Option<(usize, CountBy)>,
    pub start: StartRoom,
    pub weight_config: crate::generator::weight::WeightConfig,
    /// Set when the catalog loads; `None` until then (and for catalogs
    /// with no tags at all, resolving to empty rules).
    pub resolved: Option<ResolvedWeights>,
}

/// Failure count per door. Point-looked-up and incremented by exact key,
/// never iterated as a whole during placement, so this `HashMap` doesn't
/// reintroduce the iteration-order hazard `DESIGN.md` §7 warns about — same
/// reasoning as `Catalog::door_reservation`.
#[derive(Resource, Default, Deref, DerefMut)]
pub(crate) struct DoorAttempts(HashMap<OpenDoorId, u32>);

/// Engine-maintained placement counters (DESIGN.md §9) — the per-placement
/// memory a once-per-frame weight table lacks. Updated by `generate` on
/// every commit, read by `candidate_weight` through `WeightContext`.
#[derive(Resource, Default, Deref, DerefMut)]
pub(crate) struct PlacementCountersRes(pub PlacementCounters);

/// One spawned room entity: a "world" for `bevy_ecs_ldtk` (`LdtkProjectHandle`
/// + `LevelSet`) whose single level child the crate spawns at
/// `UseWorldTranslation`'s offset. One world per placement is what allows
/// the same level (a room template) to be instanced at many positions — a
/// single `LevelSet` can hold an iid only once.
///
/// Also the `RoomSpawned { entity }` handle consumers attach gameplay to.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct RoomWorld {
    placement: PlacementId,
}

/// Marks a `RoomWorld` whose `RoomSpawned` event has been sent.
#[derive(Component)]
pub(crate) struct Announced;

/// Translation for a room's world entity such that the level content's min
/// corner lands on `origin` in Bevy's y-up pixel space — tiles → px and the
/// one y-flip, both at this boundary (ARCHITECTURE.md §2).
pub(crate) fn room_translation(origin: TilePos, def: &RoomDef, grid_size: f32) -> Vec2 {
    let px_hei = def.size.height as f32 * grid_size;
    let target = Vec2::new(origin.x as f32 * grid_size, origin.y as f32 * grid_size);
    // `bevy_ecs_ldtk`'s `UseWorldTranslation` places the level child at
    // (world_x, -(world_y + px_hei)) relative to this room entity — the
    // level's min corner in LDtk's y-down world position, flipped exactly
    // once, here. The room's translation cancels it.
    let level_offset = Vec2::new(def.world_offset_px.x, -(def.world_offset_px.y + px_hei));
    target - level_offset
}

/// Distance in tiles from the nearest anchor to `rect` — `INFINITY` with no
/// anchors (DESIGN.md §3: zero anchors -> idle).
fn nearest_anchor_distance(rect: TileRect, anchors: &[TilePos]) -> f32 {
    anchors
        .iter()
        .map(|a| rect.distance_to(*a))
        .fold(f32::INFINITY, f32::min)
}

/// Policy gate for introducing a room entity — the per-frame cap and the
/// already-spawned check are `sync_entities`' own.
fn should_spawn(
    culling: CullingPolicy,
    rect: TileRect,
    anchors: &[TilePos],
    grid_size: f32,
) -> bool {
    culling
        .spawn_radius()
        .is_none_or(|radius_px| nearest_anchor_distance(rect, anchors) <= radius_px / grid_size)
}

/// Policy gate for despawning a room entity. Never with zero anchors — a
/// missing anchor is the normal state during loading (DESIGN.md §3), not a
/// reason to cull the world.
fn should_cull(
    culling: CullingPolicy,
    rect: TileRect,
    anchors: &[TilePos],
    grid_size: f32,
) -> bool {
    if anchors.is_empty() {
        return false;
    }
    culling
        .despawn_radius()
        .is_some_and(|radius_px| nearest_anchor_distance(rect, anchors) > radius_px / grid_size)
}

/// Force-set, not a blind `insert_resource` in `Plugin::build`: this must
/// win even if the consumer sets `LdtkSettings` themselves for unrelated
/// fields after adding the plugin. See ARCHITECTURE.md §2.
///
/// `Option<ResMut<_>>`: a missing `LdtkSettings` means `LdtkPlugin` was never
/// added, and the honest report for that is a warning plus a load that never
/// finishes — not a panic out of a library (`AGENTS.md`, lint policy). Bevy's
/// own guidance is the same: check for the resource rather than assert
/// another plugin exists, since plugin order is the consumer's to choose.
pub(crate) fn enforce_ldtk_settings(world: &mut World) {
    let Some(mut settings) = world.get_resource_mut::<LdtkSettings>() else {
        warn!("GeneratorPlugin needs LdtkPlugin: LdtkSettings is missing");
        return;
    };
    settings.level_spawn_behavior = LevelSpawnBehavior::UseWorldTranslation {
        load_level_neighbors: false,
    };
}

/// Anchor positions in y-up tile coordinates, position-sorted — never
/// entity order: entity IDs depend on spawn order and are reused
/// (DESIGN.md §3, "multiple anchors"). The distances both systems compute
/// from this are commutative over the set and don't need the order; the
/// start-room bootstrap picks *one* anchor and does.
fn anchor_tiles(
    anchors: &Query<&GlobalTransform, With<GenerationAnchor>>,
    grid_size: f32,
) -> Vec<TilePos> {
    let mut tiles: Vec<TilePos> = anchors
        .iter()
        .map(|t| {
            let px = t.translation().truncate();
            TilePos {
                x: (px.x / grid_size).floor() as i32,
                y: (px.y / grid_size).floor() as i32,
            }
        })
        .collect();
    tiles.sort_by_key(|p| (p.x, p.y));
    tiles
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
    // Local query per anchor through the door index, unless the radius is
    // so large that scanning the whole frontier is cheaper.
    let mut near: Vec<OpenDoorId> = Vec::new();
    let mut scan = false;
    for &anchor in anchors {
        match square_around(anchor, radius_tiles) {
            Some(rect)
                if !scan_is_cheaper(
                    rect,
                    catalog.spatial_cell_size(),
                    layout.open_door_count(),
                ) =>
            {
                near.extend(layout.doors_near(rect));
            }
            _ => scan = true,
        }
    }
    if scan {
        near = layout.open_doors().map(|(id, _)| id).collect();
    }
    near.sort_by_key(|id| (id.slot, id.generation));
    near.dedup();

    let mut candidates: Vec<(OpenDoorId, f32)> = Vec::new();
    for id in near {
        let Some(door) = layout.door(id) else {
            continue;
        };
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
    // Distance, then slot: a total order, so ties don't depend on the
    // spatial query's output order.
    candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.slot.cmp(&b.0.slot)));
    candidates.into_iter().map(|(id, _)| id).collect()
}

fn abandon_door(
    layout: &mut Layout,
    catalog: &Catalog,
    door_id: OpenDoorId,
    abandoned_events: &mut MessageWriter<DoorAbandoned>,
) {
    let Some(&door) = layout.door(door_id) else {
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
/// `Commands`, no entity queries — `sync_entities` is where spawning
/// happens, kept separate so this stays trivially testable.
///
/// Candidates are weighted by the consumer's `RoomWeights` table combined
/// with the engine-side per-placement adjustments (`DESIGN.md` §9); with no
/// table set, each weighs its own `weight` float tag alone.
pub(crate) fn generate(
    catalog: Res<CatalogRes>,
    mut layout: ResMut<LayoutRes>,
    mut rng: ResMut<GenRng>,
    mut attempts: ResMut<DoorAttempts>,
    mut counters: ResMut<PlacementCountersRes>,
    room_weights: Res<RoomWeights>,
    config: Res<GenerationConfig>,
    radii: Res<StreamingRadii>,
    order: Res<GenerationOrder>,
    mut reach: ResMut<OriginReach>,
    mut frontier: ResMut<OriginFrontierRes>,
    anchors: Query<&GlobalTransform, With<GenerationAnchor>>,
    mut placed_events: MessageWriter<RoomPlaced>,
    mut abandoned_events: MessageWriter<DoorAbandoned>,
    mut failed_events: MessageWriter<GenerationFailed>,
    mut next_state: ResMut<NextState<GenerationState>>,
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

    // DESIGN.md §3: zero anchors -> idle, not an error.
    let anchors = anchor_tiles(&anchors, grid_size);
    if anchors.is_empty() {
        return;
    }

    let mut placed_this_frame = 0;

    // Bootstrap: the first placement, centered on the first anchor — or on
    // the origin under `FromOrigin`, so where the anchor happened to stand
    // doesn't shift the whole dungeon. Nothing else can start generation —
    // the door loop below only fills doors that already exist, and an empty
    // Layout has none.
    if layout.placed_count() == 0 {
        let anchor = match *order {
            GenerationOrder::FromOrigin => TilePos::default(),
            GenerationOrder::NearestAnchor => {
                let Some(&anchor) = anchors.first() else {
                    return;
                };
                anchor
            }
        };
        match place_start_room(
            &mut layout,
            &catalog,
            &config.start,
            anchor,
            &mut rng.0,
            WeightParams {
                table: room_weights.0.as_deref(),
                ctx: None,
                ceiling: None,
            },
        ) {
            Ok(placement_id) => {
                placed_this_frame += 1;
                if let Some(placement) = layout.get(placement_id) {
                    placed_events.write(RoomPlaced {
                        room: placement.room,
                        at: placement.origin,
                    });
                }
            }
            Err(error) => {
                // DESIGN.md §2: `NoStartRoomMatched` is conditional — an
                // error only when the developer asked for a constraint that
                // nothing satisfies. Fail loudly (§6: a silent stall is how
                // you end up asking why the dungeon never started).
                failed_events.write(GenerationFailed(CatalogErrors(vec![error])));
                next_state.set(GenerationState::Failed);
                return;
            }
        }
    }

    // Meta-generation inputs, assembled once per frame. The ceiling arrives
    // pre-converted into `progress`'s counting convention (see
    // `progress_ceiling`) so `candidate_weight` compares like with like.
    let ceiling = progress_ceiling(&config.max_rooms, &catalog);
    // Rebuilt per door below: `WeightContext` borrows `counters`, which
    // must be mutably updated after each commit, so the borrow can't span
    // the whole loop.
    let resolved_ref = config.resolved.as_ref();
    let table = room_weights.0.as_deref();
    let started = Instant::now();
    let generation_tiles = radii.generation_radius.max(0.0) / grid_size;

    // Which door to fill next, per `GenerationOrder`. `NearestAnchor` takes
    // a snapshot of the in-radius doors once per frame; `FromOrigin` asks
    // the frontier each time, because a fresh placement can open a door
    // nearer the origin than the rest of the queue.
    let mut snapshot = match *order {
        GenerationOrder::NearestAnchor => {
            *reach = OriginReach::default();
            doors_by_distance(&layout, &catalog, &anchors, generation_tiles).into_iter()
        }
        GenerationOrder::FromOrigin => {
            let travelled = anchors
                .iter()
                .map(|a| (a.x as f32).hypot(a.y as f32))
                .fold(reach.travelled / grid_size, f32::max);
            reach.travelled = travelled * grid_size;
            reach.radius = reach.travelled + radii.generation_radius.max(0.0);
            Vec::new().into_iter()
        }
    };
    let reach_tiles = reach.radius / grid_size;

    loop {
        let over_budget = match *order {
            GenerationOrder::NearestAnchor => placed_this_frame >= ROOMS_PER_FRAME,
            GenerationOrder::FromOrigin => {
                placed_this_frame >= MAX_PLACEMENTS_PER_FRAME
                    || started.elapsed() >= GENERATION_BUDGET
            }
        };
        if over_budget || ceiling.is_some_and(|max| counters.progress >= max) {
            break;
        }
        let door_id = match *order {
            GenerationOrder::NearestAnchor => {
                let Some(door_id) = snapshot.next() else {
                    break;
                };
                // An earlier iteration this same frame may already have
                // closed this door via multi-door satisfaction.
                if layout.door(door_id).is_none() {
                    continue;
                }
                door_id
            }
            GenerationOrder::FromOrigin => {
                let Some(door_id) = frontier.0.next(&layout, &catalog, reach_tiles) else {
                    break;
                };
                door_id
            }
        };

        let weight_params = WeightParams {
            table,
            ctx: resolved_ref.map(|resolved| WeightContext {
                resolved,
                counters: &counters,
            }),
            ceiling,
        };

        match attempt_door(&mut layout, &catalog, door_id, &mut rng.0, weight_params) {
            Some(placement_id) => {
                placed_this_frame += 1;
                attempts.remove(&door_id);
                if let Some(placement) = layout.get(placement_id) {
                    // Counters must reflect the placement that just
                    // happened, before the next door weights against them —
                    // per-placement factors would otherwise read stale
                    // within a single frame (DESIGN.md §9, engine side).
                    if let Some(def) = catalog.get(placement.room) {
                        let tag_count = catalog.tag_count();
                        counters.record(
                            tag_count,
                            |i| {
                                u16::try_from(i)
                                    .ok()
                                    .map(TagId)
                                    .is_some_and(|id| def.bool_tag(id))
                            },
                            progresses(&config.max_rooms, &catalog, def),
                        );
                    }
                    placed_events.write(RoomPlaced {
                        room: placement.room,
                        at: placement.origin,
                    });
                }
            }
            // Deterministic order cannot retry: a retry later would draw
            // from the RNG at a point that depends on frame timing, and the
            // door would block the whole queue meanwhile. With reservations
            // every open door is fillable by its cap, so this only happens
            // when the weight table zeroes every candidate.
            None if *order == GenerationOrder::FromOrigin => {
                abandon_door(&mut layout, &catalog, door_id, &mut abandoned_events);
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

/// The ceiling in `PlacementCounters::progress`'s counting convention:
/// `All` counts every placement, `Tag` counts only rooms carrying the tag
/// (resolved; a tag the catalog doesn't define can never be carried, so a
/// `Tag` ceiling can never be reached — treated as unbounded).
fn progress_ceiling(max: &Option<(usize, CountBy)>, catalog: &Catalog) -> Option<usize> {
    match max {
        None => None,
        Some((max, CountBy::All)) => Some(*max),
        Some((max, CountBy::Tag(name))) => {
            // With no tags in the catalog at all, nothing can carry the
            // tag — but the catalog also predates the schema, so `All`-like
            // behaviour would surprise. Unbounded is the honest reading.
            catalog.tag_id(name).map(|_| *max)
        }
    }
}

/// Whether one committed placement counts toward the ceiling: `All` always,
/// `Tag` only when the room carries the tag. A name the catalog doesn't
/// define can never match — nothing carries a tag the project never declared.
fn progresses(max: &Option<(usize, CountBy)>, catalog: &Catalog, def: &RoomDef) -> bool {
    match max {
        None => false,
        Some((_, CountBy::All)) => true,
        Some((_, CountBy::Tag(name))) => catalog.tag_id(name).is_some_and(|id| def.bool_tag(id)),
    }
}

/// Consumer-written [`Regenerate`]: discard the layout and every spawned
/// room, reseed, and let `generate` rebuild around the anchor's current
/// position.
///
/// The Layout is truth and spawned entities are a cache over it
/// (DESIGN.md §1), so regenerating means throwing the truth away — and every
/// derived structure with it: the open-door set (inside `Layout`), the
/// per-door failure counts, and the placement counters. The counters are the
/// subtle one: they are per *catalog*, so keeping them would let the second
/// dungeon inherit the first one's tag quotas and closure progress and think
/// it was already at a ceiling it has never been near.
///
/// `RoomDespawned` is emitted per dropped room, in placement order, so a
/// consumer that tracks entities through those messages closes its
/// bookkeeping instead of holding ids that are now dangling.
#[allow(clippy::too_many_arguments)] // a Bevy system's parameters are its inputs
pub(crate) fn apply_regeneration(
    mut regenerate: MessageReader<Regenerate>,
    catalog: Res<CatalogRes>,
    mut layout: ResMut<LayoutRes>,
    mut rng: ResMut<GenRng>,
    mut current_seed: ResMut<GenerationSeed>,
    mut attempts: ResMut<DoorAttempts>,
    mut counters: ResMut<PlacementCountersRes>,
    mut frontier: ResMut<OriginFrontierRes>,
    mut reach: ResMut<OriginReach>,
    mut cursor: ResMut<SpawnCursor>,
    rooms: Query<(Entity, &RoomWorld)>,
    mut despawned_events: MessageWriter<RoomDespawned>,
    mut commands: Commands,
) {
    // Several presses in one frame collapse to the last: two regenerations
    // cannot both happen, and the newest seed is the one that was asked for.
    let Some(seed) = regenerate.read().last().map(|message| message.seed) else {
        return;
    };

    // Deterministic order (placement slot, not query order) — same reasoning
    // as culling's, so the `RoomDespawned` stream doesn't vary run to run.
    let mut doomed: Vec<(u32, Entity, RoomId)> = rooms
        .iter()
        .filter_map(|(entity, room_world)| {
            let placement = layout.get(room_world.placement)?;
            Some((room_world.placement.0, entity, placement.room))
        })
        .collect();
    doomed.sort_by_key(|(slot, _, _)| *slot);
    for (_, entity, room) in doomed {
        commands.entity(entity).despawn();
        despawned_events.write(RoomDespawned { room });
    }

    *layout = LayoutRes::default();
    *frontier = OriginFrontierRes::default();
    *reach = OriginReach::default();
    *cursor = SpawnCursor::default();
    *attempts = DoorAttempts::default();
    *counters = PlacementCountersRes(PlacementCounters::new(&catalog));
    let seed = resolve_seed(seed);
    *rng = GenRng::new(seed);
    current_seed.0 = seed;
}

/// Drains the spawn queue and the cull queue against the (already updated)
/// Layout — ARCHITECTURE.md §7. All `Commands` and entity access live here;
/// `generate` deliberately has none.
///
/// Spawned entities are a cache over the Layout (DESIGN.md §1): this system
/// never touches `LayoutRes`, so culling can't reopen doors or break seed
/// reproduction (DESIGN.md §8).
#[allow(clippy::too_many_arguments)] // a Bevy system's parameters are its inputs
pub(crate) fn sync_entities(
    mut commands: Commands,
    mut cursor: ResMut<SpawnCursor>,
    catalog: Res<CatalogRes>,
    layout: Res<LayoutRes>,
    radii: Res<StreamingRadii>,
    budget: Res<SpawnBudget>,
    mut controller: ResMut<SpawnController>,
    mut rate: ResMut<SpawnRate>,
    time: Res<Time<Real>>,
    handle: Res<LdtkHandle>,
    anchors: Query<&GlobalTransform, With<GenerationAnchor>>,
    rooms: Query<(Entity, &RoomWorld, Option<&Announced>, Option<&Children>)>,
    level_content: Query<&Children, With<LevelIid>>,
    mut spawned_events: MessageWriter<RoomSpawned>,
    mut despawned_events: MessageWriter<RoomDespawned>,
) {
    let grid_size = catalog.grid_size() as f32;
    if grid_size <= 0.0 {
        return;
    }
    let anchors = anchor_tiles(&anchors, grid_size);
    let tiles_per_frame = match *budget {
        SpawnBudget::Fixed(tiles) => tiles,
        SpawnBudget::Adaptive {
            min_tiles,
            max_tiles,
        } => controller.update(time.delta_secs() * 1000.0, min_tiles, max_tiles),
    };
    if rate.tiles_per_frame != tiles_per_frame {
        rate.tiles_per_frame = tiles_per_frame;
    }

    // `RoomSpawned` fires when the room's entities actually exist: the
    // level child `bevy_ecs_ldtk` spawns has produced content of its own
    // (layers, entities). Until then the room root exists but is empty.
    for (entity, room_world, announced, children) in rooms.iter() {
        if announced.is_some() {
            continue;
        }
        let Some(placement) = layout.get(room_world.placement) else {
            continue;
        };
        let has_content = children.into_iter().flatten().copied().any(|child| {
            level_content
                .get(child)
                .is_ok_and(|content| content.iter().next().is_some())
        });
        if !has_content {
            continue;
        }
        commands.entity(entity).insert(Announced);
        spawned_events.write(RoomSpawned {
            room: placement.room,
            at: placement.origin,
            entity,
        });
    }

    // Cull first: a moving anchor frees entities before new ones are
    // introduced. Deterministic order (placement index, not query order) so
    // the `RoomDespawned` stream itself doesn't vary run to run.
    let mut doomed: Vec<(u32, Entity, RoomId)> = Vec::new();
    for (entity, room_world, _, _) in rooms.iter() {
        let Some(placement) = layout.get(room_world.placement) else {
            continue;
        };
        let Some(def) = catalog.get(placement.room) else {
            continue;
        };
        let rect = TileRect {
            min: placement.origin,
            size: def.size,
        };
        if should_cull(radii.culling, rect, &anchors, grid_size) {
            doomed.push((room_world.placement.0, entity, placement.room));
        }
    }
    doomed.sort_by_key(|(slot, _, _)| *slot);
    for (_, entity, room) in doomed {
        commands.entity(entity).despawn();
        despawned_events.write(RoomDespawned { room });
    }

    // Spawn queue. Never a pass over the whole Layout: under a radius the
    // room index answers "what's near an anchor" locally, and under
    // `Disabled` a cursor skips placements already spawned.
    let live: HashSet<PlacementId> = rooms.iter().map(|(_, w, _, _)| w.placement).collect();
    let mut queue: Vec<(f32, PlacementId)> = Vec::new();
    match radii.culling.spawn_radius() {
        None => {
            // Budgeted here rather than below: the cursor must not move
            // past a room this frame doesn't spawn.
            let mut planned = 0u32;
            while cursor.0 < layout.placed_count() {
                let id = PlacementId(u32::try_from(cursor.0).unwrap_or(u32::MAX));
                let area = room_area(&layout, &catalog, id);
                if planned > 0 && planned.saturating_add(area) > tiles_per_frame {
                    break;
                }
                cursor.0 += 1;
                if !live.contains(&id) {
                    planned = planned.saturating_add(area);
                    queue.push((0.0, id));
                }
            }
        }
        Some(radius_px) => {
            let radius_tiles = radius_px / grid_size;
            let mut near: Vec<PlacementId> = Vec::new();
            let mut scan = false;
            for &anchor in &anchors {
                match square_around(anchor, radius_tiles) {
                    Some(rect)
                        if !scan_is_cheaper(
                            rect,
                            catalog.spatial_cell_size(),
                            layout.placed_count(),
                        ) =>
                    {
                        near.extend(layout.nearby(rect));
                    }
                    _ => scan = true,
                }
            }
            if scan {
                near = layout.iter().map(|(id, _)| id).collect();
            }
            near.sort_unstable_by_key(|id| id.0);
            near.dedup();
            for id in near {
                if live.contains(&id) {
                    continue;
                }
                let Some(placement) = layout.get(id) else {
                    continue;
                };
                let Some(def) = catalog.get(placement.room) else {
                    continue;
                };
                let rect = TileRect {
                    min: placement.origin,
                    size: def.size,
                };
                if should_spawn(radii.culling, rect, &anchors, grid_size) {
                    queue.push((nearest_anchor_distance(rect, &anchors), id));
                }
            }
            // Nearest first, so the per-frame cap only ever delays the far
            // edge; placement order breaks ties (a `HashMap` anywhere in
            // this path would make entity spawn order — and every `Entity`
            // id consumers see — vary run to run, DESIGN.md §7).
            queue.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.0.cmp(&b.1.0)));
        }
    }

    // Spawn in order until the tile budget is spent. Stop rather than skip
    // to a smaller room: the next-nearest room would otherwise wait behind
    // every smaller, further one, and it is first in line next frame.
    let mut spent = 0u32;
    for (_, placement_id) in queue {
        let Some(placement) = layout.get(placement_id) else {
            continue;
        };
        let Some(def) = catalog.get(placement.room) else {
            continue;
        };
        let area = def.size.width.saturating_mul(def.size.height);
        if spent > 0 && spent.saturating_add(area) > tiles_per_frame {
            break;
        }
        spent = spent.saturating_add(area);
        controller.spawned(area);
        commands.spawn((
            LdtkProjectHandle::from(handle.0.clone()),
            LevelSet::from_iids([def.iid.clone()]),
            Transform::from_translation(
                room_translation(placement.origin, def, grid_size).extend(0.0),
            ),
            Visibility::default(),
            Name::new(def.identifier.clone()),
            RoomWorld {
                placement: placement_id,
            },
        ));
    }
}

/// [`SpawnBudget::Adaptive`]'s state. Spawn cost doesn't land in
/// `sync_entities` itself but over the next frames, in `bevy_ecs_ldtk`'s
/// level processing and tilemap meshing, so the only honest measure is the
/// frame time — compared against the same machine's frame time when
/// nothing is spawning, which also makes it work under vsync (where a frame
/// over budget shows as a missed refresh, not a slightly longer frame).
#[derive(Resource, Debug, Clone, Copy)]
pub(crate) struct SpawnController {
    /// Current budget, tiles.
    tiles: f32,
    /// Growth stops here: just below the last budget that ran long. Creeps
    /// back up while frames stay smooth, since that spike may have been
    /// something else (a hiccup, the game's own loading).
    ceiling: f32,
    /// Typical frame time with nothing spawning, ms. `None` until the
    /// first quiet frame.
    baseline: Option<f32>,
    /// Frames since a room was last spawned. Its cost shows up for a few
    /// frames after, so only frames well clear of spawning feed the
    /// baseline, and only frames close to it are judged.
    quiet: u32,
    /// Tiles spawned since the last `update`. A budget only grows when it
    /// was actually used: rooms trickling in a few at a time cost little
    /// whatever the budget says, and growing on them would leave a budget
    /// far above anything tested waiting for the next big burst.
    spent: u32,
    /// Judged frames since one last ran long.
    smooth: u32,
}

impl Default for SpawnController {
    fn default() -> Self {
        Self {
            tiles: 512.0,
            ceiling: f32::MAX,
            baseline: None,
            quiet: u32::MAX,
            spent: 0,
            smooth: 0,
        }
    }
}

/// After a long frame, the ceiling stays where it was put for this many
/// smooth frames before it starts lifting again.
const CEILING_HOLD_FRAMES: u32 = 120;

/// How many frames a spawn's cost keeps showing up in frame time.
const SPAWN_EFFECT_FRAMES: u32 = 3;

impl SpawnController {
    /// Feed last frame's duration; returns this frame's budget.
    #[allow(clippy::cast_precision_loss)] // tile budgets are far below 2^24
    pub(crate) fn update(&mut self, frame_ms: f32, min_tiles: u32, max_tiles: u32) -> u32 {
        let (min, max) = (min_tiles as f32, max_tiles.max(min_tiles) as f32);
        let spent = std::mem::take(&mut self.spent) as f32;
        if frame_ms.is_finite() && frame_ms > 0.0 {
            let baseline = *self.baseline.get_or_insert(frame_ms);
            if self.quiet > SPAWN_EFFECT_FRAMES {
                // Slow average: one long idle frame shouldn't move it much.
                self.baseline = Some(baseline + (frame_ms - baseline) * 0.05);
            } else {
                // While spawning, a faster frame still pulls the baseline
                // down (smoothed: frame times jitter, under vsync especially,
                // and one fluke-short frame must not become the bar). It
                // only creeps up when frames run long with the budget already
                // at its minimum, i.e. the baseline itself is wrong: a window
                // moved to a slower display, or a heavier scene. Creeping
                // otherwise would absorb the spawn cost into the baseline and
                // let the budget run away.
                if frame_ms < baseline {
                    self.baseline = Some(baseline + (frame_ms - baseline) * 0.1);
                } else if self.tiles <= min {
                    self.baseline = Some(baseline * 1.002);
                }
                // Long = over the baseline by 30% + 1 ms, and by at least
                // 4 ms. Under vsync that means a missed refresh; without
                // vsync, a few ms of spawn cost on a fast machine isn't a
                // stutter, and a tighter bar would pin the budget at its
                // minimum.
                if frame_ms > baseline + (baseline * 0.3 + 1.0).max(4.0) {
                    // What actually ran long is what was spawned, not the
                    // (possibly unused) budget.
                    let cost = if spent > 0.0 {
                        spent.min(self.tiles)
                    } else {
                        self.tiles
                    };
                    self.ceiling = (cost * 0.75).max(min);
                    self.tiles = cost * 0.5;
                    self.smooth = 0;
                } else if spent * 2.0 >= self.tiles {
                    // The ceiling holds for a while after a long frame, so a
                    // long load probes it at most every couple of seconds,
                    // then lifts quickly (the additive part climbs out of a
                    // low one).
                    self.smooth = self.smooth.saturating_add(1);
                    if self.smooth > CEILING_HOLD_FRAMES {
                        self.ceiling = (self.ceiling * 1.01 + 8.0).min(f32::MAX);
                    }
                    let next = self.tiles + (self.tiles / 8.0).max(32.0);
                    self.tiles = next.min(self.ceiling.max(self.tiles));
                }
            }
        }
        self.quiet = self.quiet.saturating_add(1);
        self.tiles = self.tiles.clamp(min, max);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to u32 inputs
        let tiles = self.tiles as u32;
        tiles
    }

    /// A room of `tiles` area was spawned this frame.
    pub(crate) fn spawned(&mut self, tiles: u32) {
        self.quiet = 0;
        self.spent = self.spent.saturating_add(tiles);
    }
}

/// A placed room's area in tiles — what [`SpawnBudget`] counts. 0 for an id
/// the Layout doesn't know, which never happens for ids it handed out.
fn room_area(layout: &Layout, catalog: &Catalog, id: PlacementId) -> u32 {
    layout
        .get(id)
        .and_then(|p| catalog.get(p.room))
        .map_or(0, |def| def.size.width.saturating_mul(def.size.height))
}

/// Keeps [`StreamingRadii`] sane whenever anything writes it — the builder,
/// a settings menu, a debug slider: negative radii are zeroed and the
/// despawn ring is pushed out to [`CullingPolicy::min_despawn`], so no
/// configuration can load and unload a room on alternate frames. Each
/// correction is logged, since it means the caller asked for something
/// other than what runs.
pub(crate) fn sanitize_radii(catalog: Res<CatalogRes>, mut radii: ResMut<StreamingRadii>) {
    if !radii.is_changed() {
        return;
    }
    let grid_size = catalog.grid_size() as f32;
    let fixed = StreamingRadii {
        generation_radius: radii.generation_radius.max(0.0),
        culling: radii.culling.sanitized(grid_size),
    };
    if fixed != *radii {
        warn!(
            "StreamingRadii adjusted to keep a hysteresis band between the spawn and despawn \
             rings: {:?} -> {:?}",
            *radii, fixed
        );
        // Without change detection, so the correction itself isn't seen as
        // a fresh write next frame.
        *radii.bypass_change_detection() = fixed;
    }
    if let Some(spawn) = fixed.culling.spawn_radius()
        && fixed.generation_radius < spawn
    {
        warn!(
            "generation radius {} is inside the spawn radius {spawn}: rooms will appear at the \
             edge of view as they are placed, instead of being placed ahead of it",
            fixed.generation_radius
        );
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

    /// The one test that would catch "the rooms don't line up": it pins our
    /// spawn math to `bevy_ecs_ldtk`'s *own* conversion function rather than to
    /// a re-derivation of our formula.
    ///
    /// `pre_spawn_level` places a level child at
    /// `ldtk_pixel_coords_to_translation(IVec2::new(world_x, world_y + px_hei), 0)`
    /// under `UseWorldTranslation`. Recomputing `-(world_y + px_hei)` here —
    /// which this test used to do — passes even when the sign or the height
    /// handling is wrong, because it is the same expression the production code
    /// uses. A one-tile error in that expression is exactly a room drawn one
    /// tile off its placement, so it has to be checked against the library that
    /// actually does the drawing.
    #[test]
    #[allow(clippy::cast_possible_truncation)] // world offsets are whole pixels
    fn room_translation_lands_level_content_on_the_origin_corner() {
        let catalog = load_catalog();
        let grid_size = catalog.grid_size() as f32;
        let origin = TilePos { x: 5, y: -3 };

        for def in catalog.iter() {
            let translation = room_translation(origin, def, grid_size);

            // The level child's offset, from the crate that spawns it.
            let px_hei = def.size.height as f32 * grid_size;
            let level_offset = bevy_ecs_ldtk::utils::ldtk_pixel_coords_to_translation(
                IVec2::new(
                    def.world_offset_px.x.round() as i32,
                    (def.world_offset_px.y + px_hei).round() as i32,
                ),
                0,
            );

            assert_eq!(
                translation + level_offset,
                Vec2::new(origin.x as f32 * grid_size, origin.y as f32 * grid_size),
                "{}'s content must land exactly on the placement origin",
                def.identifier
            );
        }
    }

    fn unit_rect() -> TileRect {
        TileRect {
            min: TilePos { x: 0, y: 0 },
            size: crate::generator::geom::TileSize {
                width: 1,
                height: 1,
            },
        }
    }

    #[test]
    fn cull_hysteresis_leaves_a_band_that_keeps_state() {
        // Radius 10px at 2px/tile: respawn within 5 tiles, despawn beyond
        // 5 * 1.25 = 6.25 tiles. Against `unit_rect` (max corner (1, 1)),
        // an anchor's x minus 1 is its distance in tiles.
        let policy = CullingPolicy::radius(10.0);
        let grid_size = 2.0;

        let near = [TilePos { x: 2, y: 0 }]; // 1 tile away
        assert!(should_spawn(policy, unit_rect(), &near, grid_size));
        assert!(!should_cull(policy, unit_rect(), &near, grid_size));

        let band = [TilePos { x: 7, y: 0 }]; // 6 tiles away: in the band
        assert!(!should_spawn(policy, unit_rect(), &band, grid_size));
        assert!(
            !should_cull(policy, unit_rect(), &band, grid_size),
            "the hysteresis band must keep a spawned room spawned"
        );

        let far = [TilePos { x: 20, y: 0 }]; // 19 tiles away
        assert!(!should_spawn(policy, unit_rect(), &far, grid_size));
        assert!(should_cull(policy, unit_rect(), &far, grid_size));
    }

    #[test]
    fn culling_disabled_spawns_everything_and_culls_nothing() {
        let policy = CullingPolicy::Disabled;
        let far = [TilePos { x: 20, y: 0 }];
        assert!(should_spawn(policy, unit_rect(), &far, 2.0));
        assert!(!should_cull(policy, unit_rect(), &far, 2.0));
    }

    #[test]
    fn no_anchors_idles_instead_of_culling() {
        // The normal state during loading (DESIGN.md §3) — a missing anchor
        // must not wipe the world.
        let policy = CullingPolicy::radius(10.0);
        assert!(!should_spawn(policy, unit_rect(), &[], 2.0));
        assert!(!should_cull(policy, unit_rect(), &[], 2.0));
    }

    /// No configuration — even a despawn ring set *inside* the spawn ring —
    /// may load and unload a room as the anchor wobbles back and forth.
    /// Simulates a room's spawned state while the anchor oscillates between
    /// every pair of adjacent tiles (and pairs two tiles apart, the snap
    /// width `min_despawn` guards): each oscillation may change the state
    /// at most once.
    #[test]
    fn sanitized_rings_never_thrash() {
        let grid_size = 2.0;
        for (spawn, despawn) in [(10.0, 5.0), (10.0, 10.0), (10.0, 10.5), (40.0, 41.0)] {
            let policy = CullingPolicy::Radius { spawn, despawn }.sanitized(grid_size);
            let min = CullingPolicy::min_despawn(spawn, grid_size);
            assert!(policy.despawn_radius().is_some_and(|d| d >= min));
            for start in 0..60 {
                for wobble in [1, 2] {
                    let mut spawned = false;
                    let mut flips = 0;
                    for step in 0..20 {
                        let x = start + if step % 2 == 0 { 0 } else { wobble };
                        let at = [TilePos { x, y: 0 }];
                        let next = if spawned {
                            !should_cull(policy, unit_rect(), &at, grid_size)
                        } else {
                            should_spawn(policy, unit_rect(), &at, grid_size)
                        };
                        flips += usize::from(next != spawned);
                        spawned = next;
                    }
                    assert!(
                        flips <= 1,
                        "spawn {spawn} despawn {despawn}: anchor wobbling at x={start}±{wobble} \
                         flipped the room {flips} times"
                    );
                }
            }
        }
    }

    /// A machine for [`SpawnController`] to run on: a frame costs `base_ms`
    /// plus `ms_per_tile` for what was spawned the frame before (spawn cost
    /// lands a frame late), rounded up to whole refreshes under vsync.
    struct Machine {
        base_ms: f32,
        ms_per_tile: f32,
        vsync_ms: Option<f32>,
    }

    impl Machine {
        fn frame_ms(&self, tiles_spawned_last_frame: u32) -> f32 {
            let work = self.base_ms + tiles_spawned_last_frame as f32 * self.ms_per_tile;
            match self.vsync_ms {
                Some(refresh) => (work / refresh).ceil() * refresh,
                None => work,
            }
        }
    }

    /// Runs 60 quiet frames, then 1200 with an endless backlog of rooms.
    /// Returns (mean tiles per frame, fraction of frames that ran long)
    /// over the last 1000 frames.
    fn run(machine: &Machine) -> (f32, f32) {
        let mut controller = SpawnController::default();
        let mut last = 0u32;
        let (mut tiles_sum, mut long, mut judged) = (0u64, 0u32, 0u32);
        for frame in 0..1260 {
            let frame_ms = machine.frame_ms(last);
            let budget = controller.update(frame_ms, 32, 16_384);
            if frame >= 260 {
                judged += 1;
                tiles_sum += u64::from(budget);
                let base = machine.frame_ms(0);
                if frame_ms > base + (base * 0.3 + 1.0).max(4.0) {
                    long += 1;
                }
            }
            last = if frame >= 60 {
                controller.spawned(budget);
                budget
            } else {
                0
            };
        }
        (
            tiles_sum as f32 / judged as f32,
            long as f32 / judged as f32,
        )
    }

    #[test]
    fn adaptive_spawn_budget_settles_on_each_machine() {
        // This machine, measured: ~2.6 us/tile, 3 ms frames, vsync off.
        let fast = Machine {
            base_ms: 3.0,
            ms_per_tile: 0.0026,
            vsync_ms: None,
        };
        // 3x slower, on a 60 Hz display: 833 tiles fit before a refresh is
        // missed.
        let slow = Machine {
            base_ms: 10.0,
            ms_per_tile: 0.008,
            vsync_ms: Some(1000.0 / 60.0),
        };
        // 10x slower, barely holding 60 Hz: 100 tiles fit.
        let weak = Machine {
            base_ms: 14.0,
            ms_per_tile: 0.026,
            vsync_ms: Some(1000.0 / 60.0),
        };
        for (name, machine, min_mean) in [
            ("fast", fast, 500.0),
            ("slow", slow, 300.0),
            ("weak", weak, 50.0),
        ] {
            let (mean, long) = run(&machine);
            println!(
                "{name}: {mean:.0} tiles/frame, {:.1}% frames long",
                long * 100.0
            );
            assert!(mean >= min_mean, "{name}: only {mean:.0} tiles/frame");
            assert!(
                long < 0.02,
                "{name}: {:.1}% of frames ran long",
                long * 100.0
            );
        }
    }

    /// Rooms trickling in (a few tiles a frame) must not inflate the
    /// budget: the next burst would be spawned all at once, untested.
    #[test]
    fn trickle_spawning_does_not_grow_the_budget() {
        let mut controller = SpawnController::default();
        for _ in 0..60 {
            controller.update(3.0, 32, 16_384);
        }
        let mut budget = 0;
        for _ in 0..600 {
            budget = controller.update(3.0, 32, 16_384);
            controller.spawned(40);
        }
        assert_eq!(budget, 512, "budget grew to {budget} on 40-tile frames");
    }

    /// Dragging the window from a 144 Hz display to a 60 Hz one mid-load:
    /// every frame now takes 16.7 ms, which the old baseline would call
    /// long forever. The budget must recover instead of staying pinned at
    /// its minimum.
    #[test]
    fn budget_recovers_after_moving_to_a_slower_display() {
        let fast_display = Machine {
            base_ms: 3.0,
            ms_per_tile: 0.0026,
            vsync_ms: Some(1000.0 / 144.0),
        };
        let slow_display = Machine {
            vsync_ms: Some(1000.0 / 60.0),
            ..fast_display
        };
        let mut controller = SpawnController::default();
        let mut last = 0;
        for frame in 0..1200 {
            let machine = if frame < 300 {
                &fast_display
            } else {
                &slow_display
            };
            let budget = controller.update(machine.frame_ms(last), 32, 16_384);
            controller.spawned(budget);
            last = budget;
        }
        assert!(
            last > 1000,
            "budget stuck at {last} after the display change"
        );
    }
}

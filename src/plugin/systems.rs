//! `enforce_ldtk_settings`, `generate` (the synchronous placement driver,
//! ARCHITECTURE.md §7), and `sync_entities` (the spawn/cull queue drain).

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::generator::catalog::{Catalog, RoomDef, TagId};
use crate::generator::error::CatalogErrors;
use crate::generator::geom::{OpenDoorId, PlacementId, RoomId, TilePos, TileRect};
use crate::generator::layout::{Layout, open_door_dir, open_door_pos};
use crate::generator::place::{StartRoom, WeightParams, attempt_door, place_start_room};
use crate::generator::weight::{CountBy, PlacementCounters, ResolvedWeights, WeightContext};

use super::events::{DoorAbandoned, GenerationFailed, RoomDespawned, RoomPlaced, RoomSpawned};
use super::load::{GenerationState, LdtkHandle};
use super::{CatalogRes, CullingPolicy, GenerationAnchor, LayoutRes};

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

/// Simple per-frame room cap standing in for `DESIGN.md` §5's full
/// `GenerationBudget` (`Immediate`/`RoomsPerFrame`/`TimeBudget`), which
/// isn't wired into `GeneratorPlugin` yet — this is a fixed number, not the
/// final API.
const ROOMS_PER_FRAME: usize = 10;

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
/// app.add_systems(Update, my_weights.pipe(apply_weights).in_set(GenerationSet::Weights));/// ```
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

/// How many room entities `sync_entities` introduces per frame. Entity
/// spawning — `bevy_ecs_ldtk` materializing tilemaps — is throttled
/// separately and independently of generation itself (DESIGN.md §5): the
/// constant above caps placement work, this caps the tilemap-spawn cost.
const ROOM_SPAWNS_PER_FRAME: usize = 2;

/// A room is despawned beyond `CullingPolicy::Radius` times this margin,
/// but respawned within the radius itself — the two thresholds make the
/// hysteresis band DESIGN.md §8 requires, so an anchor wobbling on the
/// boundary doesn't flicker rooms in and out.
const CULL_HYSTERESIS: f32 = 1.25;

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
    pub generation_radius: f32,
    pub max_rooms: Option<(usize, CountBy)>,
    pub start: StartRoom,
    pub culling: CullingPolicy,
    pub weight_config: crate::generator::weight::WeightConfig,
    /// Set when the catalog loads; `None` until then (and for catalogs
    /// with no tags at all, resolving to empty rules).
    pub resolved: Option<ResolvedWeights>,
}

/// Failure count per door. Point-looked-up and incremented by exact key,
/// never iterated as a whole during placement, so this `HashMap` doesn't
/// reintroduce the iteration-order hazard `DESIGN.md` §7 warns about — same
/// reasoning as `Catalog::min_clearance`.
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
    match culling {
        CullingPolicy::Disabled => true,
        CullingPolicy::Radius(radius_px) => {
            nearest_anchor_distance(rect, anchors) <= radius_px / grid_size
        }
    }
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
    match culling {
        CullingPolicy::Disabled => false,
        CullingPolicy::Radius(radius_px) => {
            nearest_anchor_distance(rect, anchors) > radius_px * CULL_HYSTERESIS / grid_size
        }
    }
}

/// Force-set, not a blind `insert_resource` in `Plugin::build`: this must
/// win even if the consumer sets `LdtkSettings` themselves for unrelated
/// fields after adding the plugin. See ARCHITECTURE.md §2.
pub(crate) fn enforce_ldtk_settings(mut settings: ResMut<LdtkSettings>) {
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
/// `Commands`, no entity queries — `sync_entities` is where spawning
/// happens, kept separate so this stays trivially testable.
///
/// Meta-generation (`RoomWeights`, `DESIGN.md` §9) isn't wired in yet:
/// every candidate currently weighs its own `weight` float tag only, via
/// `place::attempt_door` directly.
pub(crate) fn generate(
    catalog: Res<CatalogRes>,
    mut layout: ResMut<LayoutRes>,
    mut rng: ResMut<GenRng>,
    mut attempts: ResMut<DoorAttempts>,
    mut counters: ResMut<PlacementCountersRes>,
    room_weights: Res<RoomWeights>,
    config: Res<GenerationConfig>,
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

    // Bootstrap: the first placement, centered on the first anchor. Nothing
    // else can start generation — the door loop below only fills doors that
    // already exist, and an empty Layout has none.
    if layout.placed_count() == 0 {
        let Some(&anchor) = anchors.first() else {
            return;
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

    let radius_tiles = config.generation_radius / grid_size;
    let ordered = doors_by_distance(&layout, &catalog, &anchors, radius_tiles);

    // Meta-generation inputs, assembled once per frame. The ceiling arrives
    // pre-converted into `progress`'s counting convention (see
    // `progress_ceiling`) so `candidate_weight` compares like with like.
    let ceiling = progress_ceiling(&config.max_rooms, &catalog);
    // Rebuilt per door below: `WeightContext` borrows `counters`, which
    // must be mutably updated after each commit, so the borrow can't span
    // the whole loop.
    let resolved_ref = config.resolved.as_ref();
    let table = room_weights.0.as_deref();

    for door_id in ordered {
        if placed_this_frame >= ROOMS_PER_FRAME {
            break;
        }
        // An earlier iteration this same frame may already have closed this
        // door via multi-door satisfaction (place.rs's satisfied_doors).
        if layout.open_doors().all(|(id, _)| id != door_id) {
            continue;
        }

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

/// Drains the spawn queue and the cull queue against the (already updated)
/// Layout — ARCHITECTURE.md §7. All `Commands` and entity access live here;
/// `generate` deliberately has none.
///
/// Spawned entities are a cache over the Layout (DESIGN.md §1): this system
/// never touches `LayoutRes`, so culling can't reopen doors or break seed
/// reproduction (DESIGN.md §8).
pub(crate) fn sync_entities(
    mut commands: Commands,
    catalog: Res<CatalogRes>,
    layout: Res<LayoutRes>,
    config: Res<GenerationConfig>,
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
        if should_cull(config.culling, rect, &anchors, grid_size) {
            doomed.push((room_world.placement.0, entity, placement.room));
        }
    }
    doomed.sort_by_key(|(slot, _, _)| *slot);
    for (_, entity, room) in doomed {
        commands.entity(entity).despawn();
        despawned_events.write(RoomDespawned { room });
    }

    // Spawn queue: every placement without a live room entity whose
    // nearest point is within the policy's radius, in Layout order (a
    // `HashMap` anywhere in this path would make entity spawn order — and
    // so every `Entity` id consumers see — vary run to run, DESIGN.md §7).
    let live: HashSet<PlacementId> = rooms.iter().map(|(_, w, _, _)| w.placement).collect();
    let mut spawned_this_frame = 0;
    for (placement_id, placement) in layout.iter() {
        if spawned_this_frame >= ROOM_SPAWNS_PER_FRAME {
            break;
        }
        if live.contains(&placement_id) {
            continue;
        }
        let Some(def) = catalog.get(placement.room) else {
            continue;
        };
        let rect = TileRect {
            min: placement.origin,
            size: def.size,
        };
        if !should_spawn(config.culling, rect, &anchors, grid_size) {
            continue;
        }
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
        spawned_this_frame += 1;
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

    #[test]
    fn room_translation_lands_level_content_on_the_origin_corner() {
        let catalog = load_catalog();
        let grid_size = catalog.grid_size() as f32;
        let def = catalog.iter().next().expect("fixture has rooms");
        let origin = TilePos { x: 5, y: -3 };

        let translation = room_translation(origin, def, grid_size);

        // bevy_ecs_ldtk's UseWorldTranslation offset for the level child,
        // recomputed here independently of `room_translation` — the two
        // must cancel exactly at the origin corner.
        let px_hei = def.size.height as f32 * grid_size;
        let level_offset = Vec2::new(def.world_offset_px.x, -(def.world_offset_px.y + px_hei));
        assert_eq!(
            translation + level_offset,
            Vec2::new(origin.x as f32 * grid_size, origin.y as f32 * grid_size),
            "the level content's min corner must land on the placement origin"
        );
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
        let policy = CullingPolicy::Radius(10.0);
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
        let policy = CullingPolicy::Radius(10.0);
        assert!(!should_spawn(policy, unit_rect(), &[], 2.0));
        assert!(!should_cull(policy, unit_rect(), &[], 2.0));
    }
}

//! Bevy integration layer: `GeneratorPlugin`, `GenerationSet`, and resource
//! and event registration. See ARCHITECTURE.md §1 and §7.

use std::sync::Arc;

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

pub(crate) mod events;
pub(crate) mod load;
pub(crate) mod systems;

use crate::generator::catalog::{Catalog, DoorSource};
use crate::generator::layout::Layout;
use crate::generator::place::StartRoom;
use crate::generator::weight::CountBy;
pub use events::{
    DoorAbandoned, GenerationFailed, GenerationSettled, Regenerate, RoomDespawned, RoomPlaced,
    RoomSpawned,
};
pub use load::GenerationState;
use systems::{GenRng, GenerationConfig, PlacementCountersRes};
pub use systems::{RoomWeights, apply_weights};

/// The loaded catalog, behind an `Arc` so it is cheap to share and impossible
/// to mutate accidentally (`DESIGN.md` §1: immutable after load).
#[derive(Resource, Debug, Clone, Default, Deref, DerefMut)]
pub struct CatalogRes(pub Arc<Catalog>);

/// The generator's output: which rooms were placed where (`DESIGN.md` §1).
///
/// Append-only and never touched by culling — spawned entities are a cache
/// over this, which is what makes revisiting an area reproduce it exactly.
#[derive(Resource, Debug, Clone, Default, Deref, DerefMut)]
pub struct LayoutRes(pub Layout);

/// Generation follows entities carrying this (`DESIGN.md` §3).
///
/// There is no camera fallback on purpose: attach it to a player, a camera, or
/// a scripted focus point, and generation follows whichever you chose.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct GenerationAnchor;

/// The one scheduling seam that is public API (`DESIGN.md` §9).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenerationSet {
    /// Where a consumer's weight-table system belongs. Adding it to this set
    /// is what orders it before generation, instead of leaving two writers of
    /// `RoomWeights` to race in an unspecified order.
    Weights,
}

/// Whether and how spawned room entities are culled (DESIGN.md §8).
///
/// Culling only ever despawns entities — the Layout is the truth and is
/// never touched (`AGENTS.md`, invariants), so a culled area regenerates
/// identically when revisited.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CullingPolicy {
    /// Every placement spawns and stays spawned; the consumer drives any
    /// despawning themselves.
    Disabled,
    /// Two rings around the anchors, in PIXELS, both measured to a room's
    /// nearest point: a room spawns once it is within `spawn` of an anchor
    /// and despawns once it is beyond `despawn` from every anchor. Between
    /// them is a hysteresis band where a room keeps whatever state it is
    /// already in, so an anchor wobbling on a ring doesn't flicker rooms in
    /// and out (DESIGN.md §8).
    ///
    /// The band is enforced, not trusted: `despawn` is raised to at least
    /// [`CullingPolicy::min_despawn`] if set lower (with a warning), so no
    /// configuration — including one changed at runtime — can load and
    /// unload the same room on alternate frames.
    Radius {
        /// Spawn ring, px.
        spawn: f32,
        /// Despawn ring, px. Must clear `spawn` by the minimum band.
        despawn: f32,
    },
}

impl CullingPolicy {
    /// Spawn within `spawn` px, despawn beyond `spawn` × 1.25.
    pub fn radius(spawn: f32) -> Self {
        CullingPolicy::Radius {
            spawn,
            despawn: spawn * DEFAULT_HYSTERESIS,
        }
    }

    /// A room spawns while its nearest point is within this many pixels of
    /// an anchor. `None` under [`CullingPolicy::Disabled`]: every room spawns.
    pub fn spawn_radius(self) -> Option<f32> {
        match self {
            CullingPolicy::Disabled => None,
            CullingPolicy::Radius { spawn, .. } => Some(spawn),
        }
    }

    /// A room despawns once its nearest point is beyond this many pixels from
    /// every anchor. `None` under [`CullingPolicy::Disabled`]: nothing
    /// despawns.
    pub fn despawn_radius(self) -> Option<f32> {
        match self {
            CullingPolicy::Disabled => None,
            CullingPolicy::Radius { despawn, .. } => Some(despawn),
        }
    }

    /// The smallest despawn ring that still leaves a real hysteresis band
    /// outside `spawn`, for a project whose tiles are `grid_size` px: 10% of
    /// the spawn ring or two tiles, whichever is wider. Two tiles because an
    /// anchor's position is snapped to a tile before distances are taken —
    /// a band narrower than that snap could be crossed both ways by a
    /// sub-tile wobble.
    pub fn min_despawn(spawn: f32, grid_size: f32) -> f32 {
        spawn + (spawn * 0.1).max(grid_size * 2.0)
    }

    /// This policy with negative radii zeroed and `despawn` raised to
    /// [`CullingPolicy::min_despawn`] where it was too close to `spawn`.
    pub fn sanitized(self, grid_size: f32) -> Self {
        match self {
            CullingPolicy::Disabled => CullingPolicy::Disabled,
            CullingPolicy::Radius { spawn, despawn } => {
                let spawn = spawn.max(0.0);
                CullingPolicy::Radius {
                    spawn,
                    despawn: despawn.max(Self::min_despawn(spawn, grid_size)),
                }
            }
        }
    }
}

/// [`CullingPolicy::radius`]'s despawn ring over its spawn ring.
const DEFAULT_HYSTERESIS: f32 = 1.25;

/// The three rings, in pixels, that decide what exists around the anchors:
///
/// - **generation** — open doors within it are filled. This is the "math"
///   ring: placing a room is ~10 µs of pure bookkeeping in the [`Layout`],
///   no entities, so it can run far ahead of what is loaded.
/// - **spawn** / **despawn** — [`CullingPolicy`]: which placed rooms have
///   entities.
///
/// Generation should normally reach past the spawn ring, so rooms exist as
/// math before the anchor gets close enough to load them, and a fast anchor
/// never meets an unplaced edge. Each ring is independent; the plugin warns
/// (but allows) a generation ring inside the spawn ring.
///
/// A resource, not a builder-only setting, so it can change at runtime — a
/// settings menu, a zoom level, a debug slider — and the next frame
/// generates and culls against the new values. Shrinking it never touches
/// the Layout (culling only despawns entities), so nothing placed is lost.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct StreamingRadii {
    /// Open doors within this many pixels are filled. Under
    /// [`GenerationOrder::FromOrigin`], this is how far past the anchor's
    /// furthest-ever distance from the origin generation reaches.
    pub generation_radius: f32,
    /// Which placed rooms are spawned, and which are despawned.
    pub culling: CullingPolicy,
}

/// Which open door is filled next — and so whether the dungeon depends on
/// how it was explored.
///
/// Changing it mid-run takes effect immediately, but a dungeon only has the
/// [`GenerationOrder::FromOrigin`] guarantee if it was generated that way
/// from its first room: send [`Regenerate`] after switching.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GenerationOrder {
    /// Fill the doors nearest any anchor first, within the generation
    /// radius of an anchor. The cheapest to keep up with a moving anchor,
    /// but path-dependent: the same seed explored along a different route
    /// places different rooms.
    #[default]
    NearestAnchor,
    /// Deterministic: start at tile (0, 0) and always fill the open door
    /// nearest that origin. Generation reaches a disc around the origin of
    /// radius (furthest the anchors have ever been from it) + generation
    /// radius — see [`OriginReach`]. With a fixed seed and a weight table
    /// that doesn't read live state, the same rooms land in the same places
    /// however the dungeon was walked, and however many rooms each frame
    /// got through.
    ///
    /// The cost is area: the disc fills completely, so walking `d` pixels
    /// in a straight line places ~`d²` worth of rooms rather than a
    /// corridor's worth (~10 µs each, spread over frames by the budget).
    FromOrigin,
}

/// How much placement (Layout data, no entities) each frame may do. Placing
/// a room costs ~10 µs whatever the dungeon's size, so these mostly decide
/// how fast generation keeps up with a fast-moving anchor. A resource, so
/// it can change at runtime, e.g. raised behind a loading screen.
///
/// The two orders are budgeted differently on purpose. Under
/// [`GenerationOrder::NearestAnchor`] the anchor moves between frames, so
/// how many rooms a frame places changes which doors are nearest next: a
/// time budget would make a fixed-seed run depend on machine speed, so it
/// is a count. [`GenerationOrder::FromOrigin`]'s sequence doesn't depend on
/// how many rooms each frame gets through, so it can use time.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementBudget {
    /// Rooms placed per frame at most under `NearestAnchor`. Default 64
    /// (~0.6 ms here).
    pub nearest_anchor_rooms: usize,
    /// Time spent placing per frame under `FromOrigin`. Default 2 ms
    /// (~200 rooms here), enough for the disc to keep up with a fast
    /// anchor.
    pub from_origin_time: std::time::Duration,
    /// Rooms placed per frame at most under `FromOrigin`, alongside the
    /// time budget, so a coarse clock can't let one frame run away.
    /// Default 512.
    pub from_origin_max_rooms: usize,
}

impl Default for PlacementBudget {
    fn default() -> Self {
        Self {
            nearest_anchor_rooms: 64,
            from_origin_time: std::time::Duration::from_millis(2),
            from_origin_max_rooms: 512,
        }
    }
}

/// How much tilemap the plugin may spawn per frame, counted in tiles of room
/// area: spawning a room costs in proportion to its tiles
/// (`bevy_ecs_tilemap` makes one entity per tile), so a tile budget treats a
/// project of small rooms and one of large rooms alike, whatever the grid
/// size. Nearest rooms spawn first; a room bigger than the whole budget still
/// spawns, alone.
///
/// The cost is per machine (~2.6 µs a tile on the machine the defaults were
/// measured on), which is why the default adapts. A resource, so it can
/// change at runtime, e.g. a large fixed budget behind a loading screen.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnBudget {
    /// Learn what this machine can afford. The plugin tracks the normal
    /// frame time while nothing is spawning; while spawning, it raises the
    /// budget as long as frames stay near that, and halves it when a frame
    /// runs long (a missed vsync, or ~30% over the baseline), remembering a
    /// ceiling just below the level that caused it. A slow machine loads
    /// rooms more slowly instead of stuttering; a fast one loads them
    /// sooner. [`SpawnRate`] shows the current value.
    Adaptive {
        /// Never below this, however slow frames get. At least one room
        /// spawns each frame regardless, so the dungeon always keeps
        /// loading.
        min_tiles: u32,
        /// Never above this.
        max_tiles: u32,
    },
    /// Exactly this many tiles a frame, whatever it costs.
    Fixed(u32),
}

impl Default for SpawnBudget {
    fn default() -> Self {
        Self::Adaptive {
            min_tiles: 32,
            max_tiles: 16_384,
        }
    }
}

/// The spawn budget in effect this frame, in tiles: the fixed value, or
/// where [`SpawnBudget::Adaptive`] has settled. Maintained by the plugin;
/// read it, don't write it.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpawnRate {
    /// Tiles of room area spawned per frame at most.
    pub tiles_per_frame: u32,
}

/// The seed the current dungeon is generated from — the one passed to
/// [`GeneratorPlugin::with_seed`] or [`Regenerate`], or, when none was, the
/// random one the plugin drew. Every dungeon has one, so any dungeon can be
/// reported and rebuilt: send `Regenerate { seed: Some(seed.0) }`.
///
/// Maintained by the plugin; read it, don't write it. Rebuilding is exact
/// under [`GenerationOrder::FromOrigin`]; under
/// [`GenerationOrder::NearestAnchor`] the anchor's route is an input too
/// (`DESIGN.md` §7), so the same seed walked differently diverges.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationSeed(pub u64);

/// How far [`GenerationOrder::FromOrigin`] generation reaches, in pixels,
/// from world (0, 0): the furthest any anchor has been from it plus the
/// generation radius. Maintained by the plugin; read it, don't write it.
/// Stays 0 under [`GenerationOrder::NearestAnchor`].
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct OriginReach {
    /// Furthest distance any anchor has reached from the origin, px.
    pub travelled: f32,
    /// Generation reaches this far from the origin, px: `travelled` plus
    /// [`StreamingRadii::generation_radius`].
    pub radius: f32,
}

/// Either a path to load via `AssetServer` (the common case — see this
/// file's `GeneratorPlugin::new`) or an already-loaded handle (for a
/// consumer that wants to control preloading — `GeneratorPlugin::from_handle`).
#[derive(Clone)]
pub(crate) enum ProjectSource {
    Path(String),
    Handle(Handle<LdtkProject>),
}

/// What to load, parked here until `load::begin_load` runs.
///
/// The plugin cannot resolve this in `Plugin::build`: `AssetServer` only
/// exists once `AssetPlugin` has been built, and plugins are evaluated
/// strictly in the order they are added, so reading it there would panic on
/// a perfectly reasonable `add_plugins(GeneratorPlugin::..)` before
/// `DefaultPlugins`. Storing the source and loading from `Startup` makes the
/// plugin order-independent by construction — every plugin's `build` has
/// run by the time any `Startup` system does.
#[derive(Resource)]
pub(crate) struct ProjectSourceRes(pub ProjectSource);

/// The whole library as one plugin: load a project, build a catalog, and keep
/// placing rooms around whatever carries [`GenerationAnchor`].
///
/// Configuration is the builder methods below; everything else a consumer
/// needs is a resource (`CatalogRes`, `LayoutRes`, [`RoomWeights`]) or a
/// message (the types in `events`).
///
/// Requires `LdtkPlugin` and an asset pipeline (`DefaultPlugins`); it no
/// longer requires them to be added *before* it, and reports their absence
/// rather than panicking.
pub struct GeneratorPlugin {
    source: ProjectSource,
    generation_radius: f32,
    seed: Option<u64>,
    max_rooms: Option<(usize, CountBy)>,
    start: StartRoom,
    culling: CullingPolicy,
    order: GenerationOrder,
    placement_budget: PlacementBudget,
    spawn_budget: SpawnBudget,
    door_source: DoorSource,
    weight_config: crate::generator::weight::WeightConfig,
}

impl GeneratorPlugin {
    /// Load the project at `path` (relative to the asset root) and keep
    /// rooms spawned within `view_radius` pixels of an anchor.
    ///
    /// The other rings derive from it (each overridable — see
    /// [`StreamingRadii`]): rooms are placed out to twice the view radius,
    /// so the Layout runs ahead of what is loaded, and despawned beyond
    /// 1.25×. Doors are only filled within the generation ring, which is
    /// what keeps the dungeon streaming rather than generating a whole level
    /// up front (`DESIGN.md` §3).
    pub fn new(path: impl Into<String>, view_radius: f32) -> Self {
        Self::with_source(ProjectSource::Path(path.into()), view_radius)
    }

    fn with_source(source: ProjectSource, view_radius: f32) -> Self {
        Self {
            source,
            generation_radius: view_radius * 2.0,
            seed: None,
            max_rooms: None,
            start: StartRoom::default(),
            culling: CullingPolicy::radius(view_radius),
            order: GenerationOrder::default(),
            placement_budget: PlacementBudget::default(),
            spawn_budget: SpawnBudget::default(),
            door_source: DoorSource::default(),
            weight_config: Default::default(),
        }
    }

    /// For a consumer that wants to control asset loading/preloading
    /// themselves — `DESIGN.md` §3's original `Handle<LdtkProject>` input.
    /// `GeneratorPlugin::new` covers the common case without this.
    pub fn from_handle(handle: Handle<LdtkProject>, view_radius: f32) -> Self {
        Self::with_source(ProjectSource::Handle(handle), view_radius)
    }

    /// How far, in pixels, open doors are filled — the "math" ring. Defaults
    /// to twice the view radius.
    pub fn with_generation_radius(mut self, radius: f32) -> Self {
        self.generation_radius = radius;
        self
    }

    /// Which open door is filled next (defaults to
    /// [`GenerationOrder::NearestAnchor`]). [`GenerationOrder::FromOrigin`]
    /// makes the dungeon independent of how it is explored.
    pub fn with_order(mut self, order: GenerationOrder) -> Self {
        self.order = order;
        self
    }

    /// Fix the RNG seed, making the dungeon reproducible run to run — the
    /// highest-value debugging affordance the library has (`DESIGN.md` §3).
    /// Defaults to a random seed.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Stop generating once `max` placements of the `by` kind exist — a
    /// ceiling, not a target: closure bias steers the frontier into caps as it
    /// approaches, so the dungeon finishes instead of truncating mid-run
    /// (`DESIGN.md` §9).
    pub fn with_max_rooms(mut self, max: usize, by: CountBy) -> Self {
        self.max_rooms = Some((max, by));
        self
    }

    /// Which room generation begins from (DESIGN.md §2, "Starting room").
    /// Defaults to `StartRoom::Any` — a weighted pick from the whole
    /// catalog, so a test project needs no tagging to load.
    pub fn with_start(mut self, start: StartRoom) -> Self {
        self.start = start;
        self
    }

    /// How much placement each frame may do (see [`PlacementBudget`]).
    pub fn with_placement_budget(mut self, budget: PlacementBudget) -> Self {
        self.placement_budget = budget;
        self
    }

    /// How much tilemap to spawn per frame (see [`SpawnBudget`]). Defaults
    /// to adapting to the machine.
    pub fn with_spawn_budget(mut self, budget: SpawnBudget) -> Self {
        self.spawn_budget = budget;
        self
    }

    /// Where room doors come from: `Door` entities
    /// ([`DoorSource::Entities`], the default) or gaps in an `IntGrid` wall
    /// layer ([`DoorSource::IntGridGaps`]).
    pub fn with_door_source(mut self, source: DoorSource) -> Self {
        self.door_source = source;
        self
    }

    /// Culling policy (DESIGN.md §8). Defaults to
    /// `CullingPolicy::radius(view_radius)`.
    pub fn with_culling(mut self, culling: CullingPolicy) -> Self {
        self.culling = culling;
        self
    }

    /// Meta-generation configuration (DESIGN.md §9): closure ceiling,
    /// per-tag quotas and depth gates, multi-door preference. Tag names
    /// resolve once against the loaded catalog. Defaults to inert — every
    /// room weighs its own `weight` float tag only.
    pub fn with_weight_config(mut self, config: crate::generator::weight::WeightConfig) -> Self {
        self.weight_config = config;
        self
    }
}

impl Plugin for GeneratorPlugin {
    /// Registration only — no asset loading, no world queries. `build` is
    /// the one part of the plugin lifecycle whose order relative to other
    /// plugins the consumer controls, so anything order-sensitive
    /// (`AssetServer`, `LdtkSettings`) is left to a `Startup` system: see
    /// `ProjectSourceRes` and `systems::enforce_ldtk_settings`.
    fn build(&self, app: &mut App) {
        let seed = systems::resolve_seed(self.seed);
        app.insert_resource(ProjectSourceRes(self.source.clone()))
            .insert_resource(GenerationSeed(seed))
            .insert_resource(GenRng::new(seed))
            .insert_resource(GenerationConfig {
                door_source: self.door_source.clone(),
                max_rooms: self.max_rooms.clone(),
                start: self.start.clone(),
                weight_config: self.weight_config.clone(),
                resolved: None,
            })
            .insert_resource(StreamingRadii {
                generation_radius: self.generation_radius,
                culling: self.culling,
            })
            .insert_resource(self.order)
            .insert_resource(self.placement_budget)
            .insert_resource(self.spawn_budget)
            .init_resource::<SpawnRate>()
            .init_resource::<systems::SpawnController>()
            .init_resource::<OriginReach>()
            .init_resource::<systems::OriginFrontierRes>()
            .init_resource::<systems::SpawnCursor>()
            .init_resource::<RoomWeights>()
            .init_resource::<PlacementCountersRes>()
            .init_resource::<CatalogRes>()
            .init_resource::<LayoutRes>()
            .init_state::<GenerationState>()
            .add_message::<RoomPlaced>()
            .add_message::<RoomSpawned>()
            .add_message::<RoomDespawned>()
            .add_message::<GenerationFailed>()
            .add_message::<DoorAbandoned>()
            .add_message::<GenerationSettled>()
            .add_message::<Regenerate>()
            .add_systems(Startup, (systems::enforce_ldtk_settings, load::begin_load))
            .add_systems(
                Update,
                load::poll_ldtk_load.run_if(in_state(GenerationState::Loading)),
            )
            .add_systems(
                Update,
                (
                    systems::apply_regeneration,
                    systems::sanitize_radii,
                    systems::generate,
                    systems::sync_entities,
                )
                    .chain()
                    .after(GenerationSet::Weights)
                    .run_if(in_state(GenerationState::Ready)),
            );
    }
}

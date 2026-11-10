//! Bevy integration layer: `GeneratorPlugin`, `GenerationSet`, and resource
//! and event registration. See ARCHITECTURE.md §1 and §7.

use std::sync::Arc;

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

pub(crate) mod events;
pub(crate) mod load;
pub(crate) mod systems;

use crate::generator::catalog::Catalog;
use crate::generator::layout::Layout;
use crate::generator::place::StartRoom;
use crate::generator::weight::CountBy;
pub use events::{
    DoorAbandoned, GenerationFailed, GenerationSettled, Regenerate, RoomDespawned, RoomPlaced,
    RoomSpawned,
};
pub use load::GenerationState;
use systems::{DoorAttempts, GenRng, GenerationConfig, PlacementCountersRes};
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
    /// Keep a room spawned while it is within `f32` PIXELS of any anchor;
    /// despawn it once it is beyond that distance times a small hysteresis
    /// margin. The band between the two thresholds keeps whatever state
    /// the room is already in, so an anchor wobbling on the boundary
    /// doesn't flicker (DESIGN.md §8). Should be at least
    /// `generation_radius`, or a room can be culled the same frame it is
    /// placed.
    Radius(f32),
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
    weight_config: crate::generator::weight::WeightConfig,
}

impl GeneratorPlugin {
    /// Load the project at `path` (relative to the asset root) and keep
    /// rooms spawned within `generation_radius` pixels of an anchor.
    ///
    /// Doors are only filled within that radius, which is what bounds the
    /// work per frame and keeps the dungeon streaming rather than generating
    /// a whole level up front (`DESIGN.md` §3).
    pub fn new(path: impl Into<String>, generation_radius: f32) -> Self {
        Self {
            source: ProjectSource::Path(path.into()),
            generation_radius,
            seed: None,
            max_rooms: None,
            start: StartRoom::default(),
            culling: CullingPolicy::Radius(generation_radius * 2.0),
            weight_config: Default::default(),
        }
    }

    /// For a consumer that wants to control asset loading/preloading
    /// themselves — `DESIGN.md` §3's original `Handle<LdtkProject>` input.
    /// `GeneratorPlugin::new` covers the common case without this.
    pub fn from_handle(handle: Handle<LdtkProject>, generation_radius: f32) -> Self {
        Self {
            source: ProjectSource::Handle(handle),
            generation_radius,
            seed: None,
            max_rooms: None,
            start: StartRoom::default(),
            culling: CullingPolicy::Radius(generation_radius * 2.0),
            weight_config: Default::default(),
        }
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

    /// Culling policy (DESIGN.md §8). Defaults to
    /// `CullingPolicy::Radius(generation_radius * 2.0)` — rooms stay
    /// spawned well past the distance at which they are generated.
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
        app.insert_resource(ProjectSourceRes(self.source.clone()))
            .insert_resource(GenRng::new(self.seed))
            .insert_resource(GenerationConfig {
                generation_radius: self.generation_radius,
                max_rooms: self.max_rooms.clone(),
                start: self.start.clone(),
                culling: self.culling,
                weight_config: self.weight_config.clone(),
                resolved: None,
            })
            .init_resource::<RoomWeights>()
            .init_resource::<DoorAttempts>()
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
                    systems::generate,
                    systems::sync_entities,
                )
                    .chain()
                    .after(GenerationSet::Weights)
                    .run_if(in_state(GenerationState::Ready)),
            );
    }
}

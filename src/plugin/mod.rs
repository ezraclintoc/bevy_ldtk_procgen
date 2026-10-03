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
    DoorAbandoned, GenerationFailed, GenerationSettled, RoomDespawned, RoomPlaced, RoomSpawned,
};
pub use load::GenerationState;
use load::LdtkHandle;
use systems::{DoorAttempts, GenRng, GenerationConfig, PlacementCountersRes};
pub use systems::{RoomWeights, apply_weights};

#[derive(Resource, Debug, Clone, Default, Deref, DerefMut)]
pub struct CatalogRes(pub Arc<Catalog>);

#[derive(Resource, Debug, Clone, Default, Deref, DerefMut)]
pub struct LayoutRes(pub Layout);

#[derive(Component, Debug, Clone, Copy, Default)]
pub struct GenerationAnchor;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenerationSet {
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
enum ProjectSource {
    Path(String),
    Handle(Handle<LdtkProject>),
}

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

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

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
    /// Loading the project here, not in a separate wrapper: `AssetServer`
    /// only exists once `DefaultPlugins` has built, so this requires
    /// `GeneratorPlugin` to be added after `DefaultPlugins` — the same
    /// ordering requirement `bevy_ecs_ldtk`'s own `LdtkPlugin` has.
    fn build(&self, app: &mut App) {
        let handle = match &self.source {
            ProjectSource::Path(path) => app.world().resource::<AssetServer>().load(path.clone()),
            ProjectSource::Handle(handle) => handle.clone(),
        };

        app.insert_resource(LdtkHandle(handle))
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
            .add_systems(Startup, systems::enforce_ldtk_settings)
            .add_systems(
                Update,
                load::poll_ldtk_load.run_if(in_state(GenerationState::Loading)),
            )
            .add_systems(
                Update,
                (systems::generate, systems::sync_entities)
                    .chain()
                    .after(GenerationSet::Weights)
                    .run_if(in_state(GenerationState::Ready)),
            );
    }
}

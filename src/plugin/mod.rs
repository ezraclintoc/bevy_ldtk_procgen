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
use crate::generator::weight::CountBy;
pub use events::{
    DoorAbandoned, GenerationFailed, GenerationSettled, RoomDespawned, RoomPlaced, RoomSpawned,
};
pub use load::GenerationState;

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
}

impl GeneratorPlugin {
    pub fn new(path: impl Into<String>, generation_radius: f32) -> Self {
        Self {
            source: ProjectSource::Path(path.into()),
            generation_radius,
            seed: None,
            max_rooms: None,
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
}

impl Plugin for GeneratorPlugin {
    /// Loading the project here, not in a separate wrapper: `AssetServer`
    /// only exists once `DefaultPlugins` has built, so this requires
    /// `GeneratorPlugin` to be added after `DefaultPlugins` — the same
    /// ordering requirement `bevy_ecs_ldtk`'s own `LdtkPlugin` has.
    fn build(&self, app: &mut App) {
        let _handle = match &self.source {
            ProjectSource::Path(path) => app.world().resource::<AssetServer>().load(path.clone()),
            ProjectSource::Handle(handle) => handle.clone(),
        };

        let _ = (self.generation_radius, &self.seed, &self.max_rooms);

        app.init_resource::<CatalogRes>()
            .init_resource::<LayoutRes>()
            .init_state::<GenerationState>()
            .add_message::<RoomPlaced>()
            .add_message::<RoomSpawned>()
            .add_message::<RoomDespawned>()
            .add_message::<GenerationFailed>()
            .add_message::<DoorAbandoned>()
            .add_message::<GenerationSettled>()
            .add_systems(Startup, systems::enforce_ldtk_settings);
    }
}

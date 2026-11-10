//! `GenerationState`, and the system that turns a loaded `LdtkProject` into
//! a `Catalog`. See DESIGN.md §4.

use std::sync::Arc;

use bevy::asset::LoadState;
use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use crate::generator::catalog::{CatalogError, build_catalog};
use crate::generator::error::CatalogErrors;
use crate::generator::weight::PlacementCounters;

use super::CatalogRes;
use super::events::GenerationFailed;
use super::systems::{GenerationConfig, PlacementCountersRes};
use super::{ProjectSource, ProjectSourceRes};

/// Where the plugin is in its own lifecycle (`DESIGN.md` §4).
///
/// A Bevy state rather than a queue of flags: consumers gate their own systems
/// on it (`run_if(in_state(GenerationState::Ready))`) instead of checking
/// whether some resource happens to be populated yet.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GenerationState {
    /// Waiting for the `.ldtk` to load and its catalog to build. Also where
    /// the plugin stays if no asset pipeline exists at all.
    #[default]
    Loading,
    /// Reserved for an in-progress first placement batch once `place.rs`
    /// exists. `build_catalog` is synchronous and fast, so nothing currently
    /// spends a frame in this state — `Loading` goes straight to `Ready`.
    Building,
    /// Catalog built, generation running. Placement itself is continuous, so
    /// there is no "finished"; an idle dungeon is still `Ready`.
    Ready,
    /// The catalog was rejected, or `StartRoom` matched nothing. Terminal: the
    /// details are in `GenerationFailed`, and nothing will be placed.
    Failed,
}

/// Starts the load, once per app, from `Startup`.
///
/// Deliberately not `GeneratorPlugin::build`: plugins are evaluated in the
/// order they are added, so `AssetServer` may not exist yet there. By
/// `Startup` every plugin has been built, whatever the order — so this
/// needs no ordering requirement and no panic. If there is still no
/// `AssetServer`, no Bevy asset pipeline is present at all; the app then
/// idles in `GenerationState::Loading` (DESIGN.md §4: generation idles when
/// it has nothing to generate, it never aborts the host process).
pub(crate) fn begin_load(
    mut commands: Commands,
    source: Res<ProjectSourceRes>,
    asset_server: Option<Res<AssetServer>>,
) {
    let Some(asset_server) = asset_server else {
        warn!(
            "GeneratorPlugin found no AssetServer: add DefaultPlugins (or AssetPlugin) \
             and generation will stay in GenerationState::Loading"
        );
        return;
    };
    let handle = match &source.0 {
        ProjectSource::Path(path) => asset_server.load(path.clone()),
        ProjectSource::Handle(handle) => handle.clone(),
    };
    commands.insert_resource(LdtkHandle(handle));
}

/// Holds the handle so `poll_ldtk_load` can check on it every frame.
/// `pub(crate)`, not exported — an implementation detail of how loading is
/// driven, not part of the plugin's configuration surface.
#[derive(Resource, Deref)]
pub(crate) struct LdtkHandle(pub Handle<LdtkProject>);

/// Runs only in `GenerationState::Loading` (see `GeneratorPlugin::build`).
/// `AssetServer::load_state` never panics on the handle it's given, so this
/// has no failure mode of its own to guard against.
pub(crate) fn poll_ldtk_load(
    asset_server: Option<Res<AssetServer>>,
    handle: Option<Res<LdtkHandle>>,
    projects: Res<Assets<LdtkProject>>,
    mut config: ResMut<GenerationConfig>,
    mut counters: ResMut<PlacementCountersRes>,
    mut catalog_res: ResMut<CatalogRes>,
    mut failed: MessageWriter<GenerationFailed>,
    mut next_state: ResMut<NextState<GenerationState>>,
) {
    // Optional rather than required: `begin_load` is what proves both exist,
    // and when it couldn't (no asset pipeline) idling here is correct — the
    // alternative is a panic for a configuration mistake the state machine
    // already reports (`GenerationState::Loading` forever).
    let (Some(asset_server), Some(handle)) = (asset_server, handle) else {
        return;
    };
    match asset_server.load_state(handle.0.id()) {
        LoadState::NotLoaded | LoadState::Loading => {}
        LoadState::Failed(err) => {
            let errors = CatalogErrors(vec![CatalogError::AssetLoadFailed {
                message: err.to_string(),
            }]);
            failed.write(GenerationFailed(errors));
            next_state.set(GenerationState::Failed);
        }
        LoadState::Loaded => {
            let Some(project) = projects.get(&handle.0) else {
                // AssetServer reports Loaded a frame before Assets<T> can
                // see it, in some cases — try again next frame rather than
                // treat this as a failure.
                return;
            };
            match build_catalog(project.json_data()) {
                Ok(catalog) => {
                    // One-time resolution of config tag names against the
                    // loaded catalog (DESIGN.md §2) — never by string inside
                    // the placement loop.
                    let resolved = config.weight_config.resolve(&catalog);
                    config.resolved = Some(resolved);
                    // Sized to this catalog's TagId space; rebuilt here so a
                    // second load (asset replaced) can't leave stale counters.
                    *counters = PlacementCountersRes(PlacementCounters::new(&catalog));
                    *catalog_res = CatalogRes(Arc::new(catalog));
                    next_state.set(GenerationState::Ready);
                }
                Err(errors) => {
                    failed.write(GenerationFailed(errors));
                    next_state.set(GenerationState::Failed);
                }
            }
        }
    }
}

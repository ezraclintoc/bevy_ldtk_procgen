//! `GenerationState`, and the system that turns a loaded `LdtkProject` into
//! a `Catalog`. See DESIGN.md §4.

use std::sync::Arc;

use bevy::asset::LoadState;
use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use crate::generator::catalog::{CatalogError, build_catalog};
use crate::generator::error::CatalogErrors;

use super::CatalogRes;
use super::events::GenerationFailed;

#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GenerationState {
    #[default]
    Loading,
    /// Reserved for an in-progress first placement batch once `place.rs`
    /// exists. `build_catalog` is synchronous and fast, so nothing currently
    /// spends a frame in this state — `Loading` goes straight to `Ready`.
    Building,
    Ready,
    Failed,
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
    asset_server: Res<AssetServer>,
    handle: Res<LdtkHandle>,
    projects: Res<Assets<LdtkProject>>,
    mut catalog_res: ResMut<CatalogRes>,
    mut failed: MessageWriter<GenerationFailed>,
    mut next_state: ResMut<NextState<GenerationState>>,
) {
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

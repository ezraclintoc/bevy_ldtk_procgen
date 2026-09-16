//! `enforce_ldtk_settings`. See ARCHITECTURE.md §2.
//!
//! `generate` and `sync_entities` (ARCHITECTURE.md §7) are not written yet —
//! nothing currently drives `GenerationState` past `Loading`.

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

/// Force-set, not a blind `insert_resource` in `Plugin::build`: this must
/// win even if the consumer sets `LdtkSettings` themselves for unrelated
/// fields after adding the plugin. See ARCHITECTURE.md §2.
pub(crate) fn enforce_ldtk_settings(mut settings: ResMut<LdtkSettings>) {
    settings.level_spawn_behavior = LevelSpawnBehavior::UseWorldTranslation {
        load_level_neighbors: false,
    };
}

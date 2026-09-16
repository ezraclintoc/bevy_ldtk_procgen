//! `GenerationState`. See DESIGN.md §4.
//!
//! Converting a loaded `LdtkProject` into the generator's neutral input type
//! and driving state transitions is not written yet — the state currently
//! never advances past `Loading`.

use bevy::prelude::*;

#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GenerationState {
    #[default]
    Loading,
    Building,
    Ready,
    Failed,
}

//! Pure generation core: no `bevy` or `bevy_ecs_ldtk` imports anywhere under
//! this module, so it stays testable without an `App`. See ARCHITECTURE.md §1.
//!
//! Submodules are `pub(crate)`, not `pub`: `mod generator;` in `lib.rs` is
//! private, so nothing here is reachable from outside the crate regardless —
//! but `pub(crate)` (not plain `mod`) is required for `lib.rs` itself to
//! reach two levels down and re-export anything through `prelude`. Rust
//! privacy isn't visible to a grandparent module: `lib.rs` declaring
//! `generator` doesn't make `generator`'s own private submodules visible to
//! `lib.rs`, only to `generator` and its descendants.

pub(crate) mod catalog;
pub(crate) mod error;
pub(crate) mod geom;
pub(crate) mod layout;
pub(crate) mod place;
pub(crate) mod spatial;
pub(crate) mod weight;

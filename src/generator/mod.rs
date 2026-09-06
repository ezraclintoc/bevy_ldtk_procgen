//! Pure generation core: no `bevy` or `bevy_ecs_ldtk` imports anywhere under
//! this module, so it stays testable without an `App`. See ARCHITECTURE.md §1.

mod catalog;
mod error;
mod geom;
mod layout;
mod place;
mod spatial;
mod weight;

//! `CatalogErrors`. See DESIGN.md §6.

use super::catalog::CatalogError;

/// Every error collected during one catalog build, not just the first.
#[derive(Debug, Clone, Default)]
pub struct CatalogErrors(pub Vec<CatalogError>);

impl CatalogErrors {
    /// Whether the build found no problems. Derived from the vector, not
    /// tracked separately — one source of truth.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many problems were found — every one, not just the first.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterates the problems in the order the build found them (level order,
    /// then door order inside a level).
    pub fn iter(&self) -> std::slice::Iter<'_, CatalogError> {
        self.0.iter()
    }
}

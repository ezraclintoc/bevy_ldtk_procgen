//! `CatalogErrors`. See DESIGN.md §6.

use super::catalog::CatalogError;

/// Every error collected during one catalog build, not just the first.
#[derive(Debug, Clone, Default)]
pub struct CatalogErrors(pub Vec<CatalogError>);

impl CatalogErrors {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, CatalogError> {
        self.0.iter()
    }
}

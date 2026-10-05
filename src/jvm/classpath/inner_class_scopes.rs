//! Inner Kotlin classes decoded within their outer classes' type-parameter scopes.

use super::Classpath;
use crate::jvm::classreader::{ClassInfo, ReadError};

impl Classpath {
    /// Decode an inner Kotlin class's metadata in its outer class's type-parameter scope. The outer
    /// class is the one this classpath serves; an absent outer leaves the class as parsed.
    pub(super) fn class_within_outer(&self, class: ClassInfo) -> Result<ClassInfo, ReadError> {
        match class
            .metadata_outer_class()
            .and_then(|outer| self.find_name(outer))
        {
            Some(outer) => class.within_outer(&outer),
            None => Ok(class),
        }
    }
}

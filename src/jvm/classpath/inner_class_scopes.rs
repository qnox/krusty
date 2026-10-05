//! Inner Kotlin classes decoded within their outer classes' type-parameter scopes.

use std::sync::Arc;

use super::Classpath;
use crate::jvm::classreader::ClassInfo;
use crate::types::TypeName;

impl Classpath {
    /// Decode an inner Kotlin class's metadata in the type-parameter scope of the outer class this
    /// classpath serves. Another classpath sharing the inner class's entry may serve a different
    /// outer, so the result belongs to this classpath only. An absent outer leaves the class as
    /// parsed; an outer whose scope the inner metadata cannot decode in is a load error.
    pub(super) fn class_within_outer(
        &self,
        internal: TypeName,
        class: Arc<ClassInfo>,
    ) -> Option<Arc<ClassInfo>> {
        let Some(outer) = class
            .metadata_outer_class()
            .and_then(|outer| self.find_name(outer))
        else {
            return Some(class);
        };
        match class.within_outer(&outer) {
            Ok(decoded) => Some(Arc::new(decoded)),
            Err(error) => {
                self.record_class_load_error(internal, error);
                None
            }
        }
    }
}

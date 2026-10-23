//! Library classifiers a consumer of an analysis names by their fully qualified name.

use super::SourceSetAnalysis;
use crate::symbol_resolver::{semantic_sam_signature, SymbolResolver};
use crate::types::{Ty, TypeName};

impl SourceSetAnalysis {
    /// The library classifier a fully qualified name denotes, resolved as a fully qualified type
    /// reference is, or `None` when the name is unknown or names a typealias.
    pub fn library_classifier(&self, qualified_name: &str) -> Option<TypeName> {
        let selected = SymbolResolver::new(&*self.symbols.libraries)
            .fully_qualified_declaration(qualified_name)?;
        selected.alias.is_none().then_some(selected.classifier)
    }

    /// Whether Kotlin converts a lambda to the interface `classifier`.
    pub fn converts_lambdas_to(&self, classifier: TypeName) -> bool {
        semantic_sam_signature(&*self.symbols.libraries, Ty::obj_name(classifier)).is_some()
    }
}

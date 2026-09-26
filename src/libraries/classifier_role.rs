//! The role a classifier record plays in type checks and casts, read off its declaration facts.

use super::LibraryType;
use crate::types::{ClassifierRole, Ty};

impl LibraryType {
    /// The role this declaration plays in type checks and casts: a mapped collection face, or a
    /// plain `FunctionN` classifier whose arity is its declared callable signature's.
    pub fn classifier_role(&self) -> Option<ClassifierRole> {
        if let Some(collection) = self.mapped_collection {
            return Some(ClassifierRole::MappedCollection(collection));
        }
        if !self.represents_function_type() {
            return None;
        }
        match self.callable_signature? {
            Ty::Fun(signature) if !signature.suspend => u8::try_from(signature.params.len())
                .ok()
                .map(ClassifierRole::FunctionOfArity),
            _ => None,
        }
    }
}

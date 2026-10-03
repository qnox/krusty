//! The role a classifier record plays in type checks and casts, read off its declaration facts.

use super::LibraryType;
use crate::types::{ClassifierRole, Ty};

impl LibraryType {
    /// The role this declaration plays in type checks and casts: a mapped collection face, or a
    /// non-reflective function classifier. Function arity is the source value-parameter count;
    /// backends own any physical parameters they add.
    pub fn classifier_role(&self) -> Option<ClassifierRole> {
        if let Some(collection) = self.mapped_collection {
            return Some(ClassifierRole::MappedCollection(collection));
        }
        if !self.represents_function_type() {
            return None;
        }
        match self.callable_signature? {
            Ty::Fun(signature) => {
                let arity = u8::try_from(signature.params.len()).ok()?;
                Some(if signature.suspend {
                    ClassifierRole::SuspendFunctionOfArity(arity)
                } else {
                    ClassifierRole::FunctionOfArity(arity)
                })
            }
            _ => None,
        }
    }
}

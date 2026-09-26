//! The semantic role of a type-operation target that a plain instance test or cast cannot decide.
//!
//! A mutable Kotlin collection shares its platform interface with its read-only face, and a
//! function type erases to a class every lambda of any arity may implement. Both roles come from
//! declaration facts: the mapped-collection builtins and the function-classifier builtins (or a
//! `Ty::Fun` signature). A backend maps a role to its own runtime checks.

use crate::types::wk::{self, CollectionKind};
use crate::types::Ty;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeCheckRole {
    /// The mutable face of a mapped collection classifier.
    MutableCollection(CollectionKind),
    /// A non-suspend function type of this arity (receiver and context parameters included).
    FunctionOfArity(u8),
}

impl TypeCheckRole {
    /// The role of the target `ty` of an `is` or `as` (nullability aside).
    pub fn of(ty: Ty) -> Option<Self> {
        match ty.non_null() {
            Ty::Obj(name, _) => {
                if let Some(collection) = wk::mapped_collection(name) {
                    return collection
                        .mutable
                        .then_some(Self::MutableCollection(collection.kind));
                }
                let classifier = crate::libraries::function_classifiers::classifier(name)?;
                if classifier.is_suspend() || classifier.is_reflective() {
                    return None;
                }
                u8::try_from(classifier.arity())
                    .ok()
                    .map(Self::FunctionOfArity)
            }
            Ty::Fun(signature) if !signature.suspend => u8::try_from(signature.params.len())
                .ok()
                .map(Self::FunctionOfArity),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{intern_fnsig, type_name, FnSig};

    #[test]
    fn mutable_collections_and_plain_function_types_have_roles() {
        let entry = Ty::Obj(type_name("kotlin/collections/MutableMap.MutableEntry"), &[]);
        assert_eq!(
            TypeCheckRole::of(Ty::nullable(entry)),
            Some(TypeCheckRole::MutableCollection(CollectionKind::MapEntry))
        );
        assert_eq!(
            TypeCheckRole::of(Ty::Obj(type_name("kotlin/collections/List"), &[])),
            None
        );
        let function = |suspend| {
            Ty::Fun(intern_fnsig(FnSig {
                params: vec![Ty::Int, Ty::String],
                ret: Ty::Int,
                context_count: 0,
                has_receiver: true,
                suspend,
            }))
        };
        assert_eq!(
            TypeCheckRole::of(function(false)),
            Some(TypeCheckRole::FunctionOfArity(2))
        );
        assert_eq!(TypeCheckRole::of(function(true)), None);
    }

    /// A written `FunctionN` classifier takes its arity from the builtins declaration, not from
    /// how many type arguments the reference happens to carry.
    #[test]
    fn a_function_classifier_takes_its_declared_arity() {
        let written = Ty::Obj(type_name("kotlin/Function2"), &[]);
        assert_eq!(
            TypeCheckRole::of(written),
            Some(TypeCheckRole::FunctionOfArity(2))
        );
        let suspending = Ty::Obj(type_name("kotlin/coroutines/SuspendFunction1"), &[]);
        assert_eq!(TypeCheckRole::of(suspending), None);
    }
}

//! Provider-backed planning facts for checked runtime type operations.
//!
//! The resolver owns declaration lookup. This module reduces both sides of an `is` operation to
//! the declaration-published classifier role that Kotlin compares after erasure, then hands only
//! the finalized equality decision to checked FIR.

use std::collections::HashSet;

use crate::symbol_source::SymbolSource;
use crate::types::{ClassifierRole, Ty};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErasedBound {
    Classifier(Ty),
    Function(ClassifierRole),
}

pub(super) fn same_erased_upper_bound(source: &dyn SymbolSource, operand: Ty, target: Ty) -> bool {
    erased_bound(source, operand)
        .is_some_and(|operand| Some(operand) == erased_bound(source, target))
}

fn erased_bound(source: &dyn SymbolSource, ty: Ty) -> Option<ErasedBound> {
    let mut current = ty.non_null();
    let mut seen = HashSet::new();
    loop {
        match current {
            Ty::TyParam(name, bound) if seen.insert(name) => current = bound.non_null(),
            Ty::DefinitelyNotNull(inner) => current = inner.non_null(),
            Ty::Obj(name, _) => {
                let role = source.classifier(name).and_then(|ty| ty.classifier_role());
                return Some(match role {
                    Some(
                        role @ (ClassifierRole::FunctionOfArity(_)
                        | ClassifierRole::SuspendFunctionOfArity(_)),
                    ) => ErasedBound::Function(role),
                    Some(ClassifierRole::MappedCollection(_)) | None => {
                        ErasedBound::Classifier(Ty::obj_name(name).canonical_semantic())
                    }
                });
            }
            Ty::Fun(signature) => {
                let arity = u8::try_from(signature.params.len()).ok()?;
                return Some(ErasedBound::Function(if signature.suspend {
                    ClassifierRole::SuspendFunctionOfArity(arity)
                } else {
                    ClassifierRole::FunctionOfArity(arity)
                }));
            }
            Ty::TyParam(..)
            | Ty::Nullable(_)
            | Ty::PlatformNullable(_)
            | Ty::Null
            | Ty::Nothing
            | Ty::Error
            | Ty::Pending
            | Ty::InProjection(_)
            | Ty::OutProjection(_)
            | Ty::StarProjection(_)
            | Ty::Intersection(_) => return None,
            builtin => return Some(ErasedBound::Classifier(builtin)),
        }
    }
}

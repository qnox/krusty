//! Provider-backed planning facts for checked runtime type operations.
//!
//! The resolver owns declaration lookup. This module reduces both sides of an `is` operation to
//! the declaration-published classifier role that Kotlin compares after erasure, then hands only
//! the finalized equality decision to checked FIR.

use std::collections::HashSet;

use crate::symbol_source::SymbolSource;
use crate::types::{ClassifierRole, Ty, TyKind};

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
        match current.kind() {
            TyKind::TyParam(name, bound) | TyKind::Var(name, bound) if seen.insert(name) => {
                current = bound.non_null()
            }
            TyKind::DefinitelyNotNull(inner) => current = inner.non_null(),
            TyKind::Obj(name, _) => {
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
            TyKind::Fun(signature) => {
                let arity = u8::try_from(signature.params.len()).ok()?;
                return Some(ErasedBound::Function(if signature.suspend {
                    ClassifierRole::SuspendFunctionOfArity(arity)
                } else {
                    ClassifierRole::FunctionOfArity(arity)
                }));
            }
            TyKind::TyParam(..)
            | TyKind::Var(..)
            | TyKind::Nullable(_)
            | TyKind::PlatformNullable(_)
            | TyKind::Null
            | TyKind::Nothing
            | TyKind::Error
            | TyKind::Pending
            | TyKind::InProjection(_)
            | TyKind::OutProjection(_)
            | TyKind::StarProjection(_)
            | TyKind::Intersection(_) => return None,
            _ => return Some(ErasedBound::Classifier(current)),
        }
    }
}

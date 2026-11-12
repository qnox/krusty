//! The builtin primitive classes' implementations of a supertype's member.
//!
//! A primitive value read through one of its classifier's supertypes (`Number`, `Comparable<Int>`)
//! is an instance of that classifier, so a call of the supertype's member on it runs the
//! classifier's override (`Int.toLong` for `Number.toLong`). A target that carries such a value as
//! an object dispatches the call itself, and asks here which declaration each primitive runs. The
//! member hierarchy's override edges answer it; the member's name only collects the candidates.

use crate::fir::ExternalCallableId;
use crate::libraries::FunctionInfo;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

use super::{declared_member_declarations, member_declarations_in_hierarchy, supertype_preorder};

/// Every builtin primitive type, each the only instance type of its own final classifier.
const PRIMITIVE_TYPES: [Ty; 8] = [
    Ty::Boolean,
    Ty::Byte,
    Ty::Short,
    Ty::Char,
    Ty::Int,
    Ty::Long,
    Ty::Float,
    Ty::Double,
];

/// What one primitive runs for a call of a supertype's member.
pub(crate) struct PrimitiveOverride {
    /// The primitive whose classifier is a proper subtype of the member's.
    pub(crate) receiver: Ty,
    /// The primitive classifier's declaration overriding the member; `None` when the classifier
    /// declares none and inherits the member's own implementation.
    pub(crate) declaration: Option<FunctionInfo>,
}

/// Each primitive whose classifier is a proper subtype of `owner`, with its override of `owner`'s
/// member `name` that the provider published as `identity`. Empty when no primitive classifier is
/// below `owner`, or `owner` declares no such member.
pub(crate) fn primitive_overrides(
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &str,
    identity: ExternalCallableId,
) -> Vec<PrimitiveOverride> {
    let subtypes = PRIMITIVE_TYPES
        .into_iter()
        .filter(|primitive| {
            primitive.kotlin_class_internal().is_some_and(|classifier| {
                supertype_preorder(source, classifier)
                    .get(&owner)
                    .is_some_and(|&visit| visit > 0)
            })
        })
        .collect::<Vec<_>>();
    if subtypes.is_empty() {
        return Vec::new();
    }
    let (declared, _) =
        declared_member_declarations(source, Ty::obj_name(owner), name).into_parts();
    let Some(base) = declared
        .overloads
        .iter()
        .find(|function| function.callable.external_identity == Some(identity))
    else {
        return Vec::new();
    };
    let base = super::function_declaration_identity(base);
    subtypes
        .into_iter()
        .map(|receiver| {
            let (functions, _) =
                member_declarations_in_hierarchy(source, receiver, name).into_parts();
            // A class declares at most one override of a member.
            let declaration = functions
                .overloads
                .into_iter()
                .find(|function| function.callable.overridden_declarations.contains(&base));
            PrimitiveOverride {
                receiver,
                declaration,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;

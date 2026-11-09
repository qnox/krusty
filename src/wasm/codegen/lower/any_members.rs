//! Calls of `kotlin.Any`'s members on dependency declarations: `Any`'s own `equals`, `hashCode`
//! and `toString`, and every library declaration overriding one (`String.hashCode`,
//! `Int.equals`, a library data class's `toString`).
//!
//! The checked semantic role says which of `Any`'s members a call invokes: `Any`'s declarations
//! publish it, and selection decorates the call edge of an override that inherits it. That role,
//! never the declaration's spelling or signature, is the authority here. A scalar's `hashCode`
//! is also selected as the provider's `PrimitiveHashCode` operation. Such a declaration has no body to call on this target: its
//! implementation is the receiver's own representation, which the runtime answers through the
//! same operations that realize box slots, generated members and string templates.

use crate::backend::class_tables::AnyMember;
use crate::backend::CheckedBackendCallables;
use crate::libraries::CompilerIntrinsic;
use crate::types::SemanticCallRole;

use super::*;

/// The `kotlin.Any` member the dependency call `expression` invokes, from its checked selection.
pub(in crate::wasm::codegen) fn call_member(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    expression: u32,
) -> Option<AnyMember> {
    let IrExpr::Call {
        callee,
        dispatch_receiver: receiver @ Some(_),
        ..
    } = ir.expr(expression)
    else {
        return None;
    };
    let Callee::External {
        target, defaults, ..
    } = &**callee
    else {
        return None;
    };
    if !defaults.is_empty() {
        return None;
    }
    let callable = callables.callable(*target)?;
    member_of(
        ir.semantic_call_roles.get(&expression).copied(),
        callable.semantic_role,
        callable.compiler_operation(),
        receiver.and_then(|receiver| ir.checked_type(receiver)),
    )
}

/// Which of `Any`'s members a selected call is.
///
/// `Any`'s own declarations carry their role, and selection decorates the call edge of an
/// override that inherits one (`String.hashCode`), whose own declaration carries none; when both
/// are recorded they must agree. A role that is not one of `Any`'s members, or one that contradicts
/// the provider's `PrimitiveHashCode` operation, is no member. Without a role only that operation
/// on a checked scalar receiver is `hashCode`: it never admits a `String` or another reference.
fn member_of(
    edge_role: Option<SemanticCallRole>,
    declaration_role: Option<SemanticCallRole>,
    operation: Option<CompilerIntrinsic>,
    receiver: Option<Ty>,
) -> Option<AnyMember> {
    let primitive_hash = operation == Some(CompilerIntrinsic::PrimitiveHashCode);
    let role = match (edge_role, declaration_role) {
        (Some(edge), Some(declared)) if edge != declared => return None,
        (edge, declared) => edge.or(declared),
    };
    let member = match role {
        Some(SemanticCallRole::KotlinAnyEquals) => AnyMember::Equals,
        Some(SemanticCallRole::KotlinAnyHashCode) => AnyMember::HashCode,
        Some(SemanticCallRole::KotlinAnyToString) => AnyMember::ToString,
        Some(_) => return None,
        None => {
            return (primitive_hash && receiver.is_some_and(is_scalar_hash_receiver))
                .then_some(AnyMember::HashCode);
        }
    };
    (!primitive_hash || member == AnyMember::HashCode).then_some(member)
}

/// The receivers whose `hashCode` the provider selects as its `PrimitiveHashCode` operation.
fn is_scalar_hash_receiver(ty: Ty) -> bool {
    matches!(
        ty,
        Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char | Ty::Int | Ty::Long | Ty::Float | Ty::Double
    )
}

impl Body<'_> {
    /// `receiver.equals(other)`, `receiver.hashCode()` or `receiver.toString()`.
    pub(super) fn dependency_any_member_call(
        &mut self,
        member: AnyMember,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        match (member, args) {
            (AnyMember::Equals, [other]) => self.object_equality(receiver, *other),
            (AnyMember::HashCode, []) => {
                let ty = self
                    .type_of(receiver)?
                    .ok_or_else(|| "a `hashCode` receiver that does not complete".to_string())?;
                self.generated_property_hash(receiver, ty)
            }
            (AnyMember::ToString, []) => self.string_of(receiver),
            _ => Err(format!("a malformed call of `kotlin.Any`'s {member:?}")),
        }
    }
}

#[cfg(test)]
mod tests;

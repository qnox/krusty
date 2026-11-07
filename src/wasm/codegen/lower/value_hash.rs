//! Hashes of statically known builtin values, selected by frozen declaration identity.
//!
//! This does not admit a call on `Any`: boxes and ordinary object default slots need their own
//! complete realization before such a call can dispatch. Registry admission and emission use the
//! same checked-call classifier, so no dependency body is skipped on a spelling or carrier guess.

use super::*;
use crate::backend::BackendCallableFact;
use crate::ir::ExprId;
use crate::libraries::CompilerIntrinsic;
use crate::types::SemanticCallRole;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::wasm::codegen) struct ValueHash(Ty);

fn declaration_kind(
    operation: Option<CompilerIntrinsic>,
    role: Option<SemanticCallRole>,
    receiver: Ty,
) -> Option<ValueHash> {
    if role.is_some_and(|role| role != SemanticCallRole::KotlinAnyHashCode) {
        return None;
    }
    let hash_member = role == Some(SemanticCallRole::KotlinAnyHashCode);
    let scalar = matches!(
        receiver,
        Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char | Ty::Int | Ty::Long | Ty::Float | Ty::Double
    );
    if (scalar && (operation == Some(CompilerIntrinsic::PrimitiveHashCode) || hash_member))
        || (receiver == Ty::String && hash_member)
    {
        Some(ValueHash(receiver))
    } else {
        None
    }
}

fn selected_kind(
    ir: &IrFile,
    receiver: ExprId,
    fact: &BackendCallableFact,
    call_role: Option<SemanticCallRole>,
) -> Option<ValueHash> {
    if !fact.is_member_function() || !fact.params.is_empty() || fact.ret != Ty::Int {
        return None;
    }
    // This is the checker's recorded node type, including nullability; never infer a narrower
    // receiver from its syntax, current value, physical owner, or a similarly named declaration.
    declaration_kind(
        fact.compiler_operation(),
        call_role,
        ir.checked_type(receiver)?,
    )
}

pub(in crate::wasm::codegen) fn call_kind(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    expression: ExprId,
) -> Option<(ValueHash, ExprId)> {
    let IrExpr::Call {
        callee,
        dispatch_receiver: Some(receiver),
        args,
    } = ir.expr(expression)
    else {
        return None;
    };
    let Callee::External {
        target,
        params,
        ret,
        defaults,
        extension_receiver_parameter: None,
        ..
    } = &**callee
    else {
        return None;
    };
    if !params.is_empty() || !args.is_empty() || !defaults.is_empty() || *ret != Ty::Int {
        return None;
    }
    // Selection decorates this call edge with inherited builtin identity. The frozen declaration
    // may be String's override rather than Any's declaration, so its undecorated role is not the
    // authority for this operation.
    Some((
        selected_kind(
            ir,
            *receiver,
            callables.callable(*target)?,
            ir.semantic_call_roles.get(&expression).copied(),
        )?,
        *receiver,
    ))
}

impl Body<'_> {
    pub(super) fn value_hash_call(&mut self, expression: ExprId) -> Result<(), Unsupported> {
        let (ValueHash(ty), receiver) = call_kind(self.ir, self.callables, expression)
            .ok_or("a builtin hash call without its checked declaration and receiver")?;
        self.value(receiver, ty)?;
        // One hash of each value kind for every caller: the same functions realize boxes'
        // `hashCode` slots and generated data-class hashes.
        if ty == Ty::String {
            self.code.call(self.runtime.hashing.string_hash);
            return Ok(());
        }
        if !self.runtime.hashing.emit_scalar_hash(&mut self.code, ty) {
            return Err("an unsupported builtin hash receiver".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_and_nullable_values_do_not_admit_unimplemented_dispatch_slots() {
        for ty in [
            Ty::obj_name(crate::types::wk::any()),
            Ty::nullable(Ty::String),
            Ty::nullable(Ty::Int),
            Ty::Unit,
        ] {
            assert_eq!(
                declaration_kind(None, Some(SemanticCallRole::KotlinAnyHashCode), ty),
                None
            );
        }
    }

    #[test]
    fn a_builtin_receiver_without_a_selected_hash_declaration_is_not_enough() {
        assert_eq!(declaration_kind(None, None, Ty::String), None);
        assert_eq!(
            declaration_kind(None, Some(SemanticCallRole::KotlinAnyEquals), Ty::Int),
            None
        );
        assert_eq!(
            declaration_kind(
                Some(CompilerIntrinsic::PrimitiveHashCode),
                Some(SemanticCallRole::KotlinAnyEquals),
                Ty::Int
            ),
            None
        );
    }

    #[test]
    fn every_scalar_hash_intrinsic_has_a_complete_static_realization() {
        for ty in [
            Ty::Boolean,
            Ty::Byte,
            Ty::Short,
            Ty::Char,
            Ty::Int,
            Ty::Long,
            Ty::Float,
            Ty::Double,
        ] {
            assert_eq!(
                declaration_kind(Some(CompilerIntrinsic::PrimitiveHashCode), None, ty),
                Some(ValueHash(ty))
            );
        }
    }

    #[test]
    fn only_the_typed_hash_role_admits_a_string_receiver() {
        assert_eq!(
            declaration_kind(None, Some(SemanticCallRole::KotlinAnyHashCode), Ty::String),
            Some(ValueHash(Ty::String))
        );
        assert_eq!(
            declaration_kind(Some(CompilerIntrinsic::PrimitiveHashCode), None, Ty::String),
            None
        );
    }
}

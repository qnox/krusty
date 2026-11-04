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

fn selected_kind(ir: &IrFile, receiver: ExprId, fact: &BackendCallableFact) -> Option<ValueHash> {
    if !fact.is_member_function() || !fact.params.is_empty() || fact.ret != Ty::Int {
        return None;
    }
    // This is the checker's recorded node type, including nullability; never infer a narrower
    // receiver from its syntax, current value, physical owner, or a similarly named declaration.
    declaration_kind(
        fact.compiler_operation(),
        fact.semantic_role,
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
    Some((
        selected_kind(ir, *receiver, callables.callable(*target)?)?,
        *receiver,
    ))
}

impl Body<'_> {
    pub(super) fn value_hash_call(&mut self, expression: ExprId) -> Result<(), Unsupported> {
        let (ValueHash(ty), receiver) = call_kind(self.ir, self.callables, expression)
            .ok_or("a builtin hash call without its checked declaration and receiver")?;
        self.value(receiver, ty)?;
        match ty {
            Ty::String => {
                self.code.ref_as_non_null().call(self.runtime.string_hash);
            }
            Ty::Boolean => {
                self.code
                    .if_(BlockType::Value(ValType::I32))
                    .i32_const(1231)
                    .else_()
                    .i32_const(1237)
                    .end();
            }
            Ty::Byte | Ty::Short | Ty::Char | Ty::Int => {}
            Ty::Float => {
                let value = self.local(ValType::F32);
                self.code
                    .local_tee(value)
                    .local_get(value)
                    .op(0x5c)
                    .if_(BlockType::Value(ValType::I32))
                    .i32_const(0x7fc00000)
                    .else_()
                    .local_get(value)
                    .op(0xbc)
                    .end();
            }
            Ty::Double => {
                let value = self.local(ValType::F64);
                self.code
                    .local_tee(value)
                    .local_get(value)
                    .op(0x62)
                    .if_(BlockType::Value(ValType::I64))
                    .i64_const(0x7ff8000000000000)
                    .else_()
                    .local_get(value)
                    .op(0xbd)
                    .end();
                self.hash_long_bits();
            }
            Ty::Long => self.hash_long_bits(),
            _ => return Err("an unsupported builtin hash receiver".to_owned()),
        }
        Ok(())
    }

    fn hash_long_bits(&mut self) {
        let bits = self.local(ValType::I64);
        self.code
            .local_tee(bits)
            .local_get(bits)
            .i64_const(32)
            .op(SHR_U64)
            .op(XOR64)
            .op(0xa7);
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

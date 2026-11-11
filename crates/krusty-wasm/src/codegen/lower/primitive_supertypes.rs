//! `kotlin.Number` conversions and `kotlin.Comparable.compareTo` on a boxed scalar.
//!
//! Both declarations are abstract. A primitive read as one of them is its box, and the box's value
//! is what the member computes: `Number.toLong` converts that value, and `compareTo` orders two
//! values of the same scalar. A file that declares its own `Number` or `Comparable` is left to
//! ordinary dispatch, which this backend does not emit a body for.

use krusty::backend::CheckedBackendCallables;
use krusty::fir::ExternalCallableId;
use krusty::ir::IrFile;
use krusty::types::SemanticCallRole;

use super::super::super::encode::{BlockType, HeapType};
use super::super::super::objects::REFERENCE;
use super::objects::any;
use super::*;

/// Whether `target` is a `Number` conversion or `Comparable.compareTo` this file can realize from
/// boxed scalars. A source `Number` or `Comparable` stays on the ordinary dependency path.
pub(in crate::codegen) fn is_realized(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    target: ExternalCallableId,
) -> bool {
    let Some(role) = callables
        .callable(target)
        .and_then(|callable| callable.semantic_role)
    else {
        return false;
    };
    match role {
        SemanticCallRole::KotlinNumberConversion => !declares_own_number(ir),
        SemanticCallRole::KotlinComparableCompareTo => !declares_own_comparable(ir),
        _ => false,
    }
}

fn declares_own_number(ir: &IrFile) -> bool {
    let number = krusty::types::wk::number();
    ir.classes.iter().any(|class| class.superclass == number)
}

fn declares_own_comparable(ir: &IrFile) -> bool {
    let comparable = krusty::types::wk::comparable();
    let enum_class = krusty::types::wk::kotlin_enum();
    ir.classes.iter().any(|class| {
        class.is_enum
            || class.is_enum_entry
            || class.superclass == enum_class
            || class
                .interfaces
                .iter()
                .any(|interface| interface == comparable)
    })
}

impl Body<'_> {
    pub(super) fn primitive_supertype_call(
        &mut self,
        target: ExternalCallableId,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let callable = self.callables.callable(target).ok_or_else(|| {
            "a primitive supertype call without frozen callable facts".to_string()
        })?;
        let role = callable.semantic_role;
        let result = callable.ret;
        match role {
            Some(SemanticCallRole::KotlinNumberConversion) => {
                if !args.is_empty() {
                    return Err("a `Number` conversion with arguments".to_string());
                }
                self.number_conversion(receiver, result)
            }
            Some(SemanticCallRole::KotlinComparableCompareTo) => {
                let &[argument] = args else {
                    return Err("`compareTo` with an unexpected operand shape".to_string());
                };
                self.comparable_compare_to(receiver, argument)
            }
            _ => Err("a primitive supertype call the selector did not name".to_string()),
        }
    }

    /// `receiver.toLong()` (and the other `Number` conversions) by the box the receiver is.
    fn number_conversion(&mut self, receiver: u32, target: Ty) -> Result<(), Unsupported> {
        if !matches!(
            target,
            Ty::Byte | Ty::Short | Ty::Char | Ty::Int | Ty::Long | Ty::Float | Ty::Double
        ) {
            return Err(format!("a `Number` conversion to `{}`", describe(target)));
        }
        let result = carrier(target)?
            .ok_or_else(|| format!("a `Number` conversion to `{}`", describe(target)))?;
        self.value(receiver, any())?;
        let receiver_local = self.local(REFERENCE);
        self.code.local_set(receiver_local);
        self.code.block(BlockType::Value(result));
        for source in [
            Ty::Byte,
            Ty::Short,
            Ty::Int,
            Ty::Long,
            Ty::Float,
            Ty::Double,
        ] {
            let Some(form) = self.scalar_box(source)? else {
                continue;
            };
            self.code
                .local_get(receiver_local)
                .ref_test(false, HeapType::Concrete(form.structure))
                .if_(BlockType::Empty);
            self.code.local_get(receiver_local);
            form.emit_unbox(&mut self.code);
            self.convert_number(source, target)?;
            // The `if` is depth 0 and the result block is depth 1.
            self.code.br(1).end();
        }
        self.throw_class_cast()?;
        self.code.end();
        Ok(())
    }

    fn comparable_compare_to(&mut self, receiver: u32, argument: u32) -> Result<(), Unsupported> {
        let argument_ty = self
            .type_of(argument)?
            .ok_or_else(|| "a `compareTo` argument that does not complete".to_string())?;
        let scalar =
            carrier(argument_ty)?.is_some_and(|value| !matches!(value, ValType::Ref { .. }));
        if scalar {
            self.compare_unboxed(receiver, argument, argument_ty)
        } else {
            self.compare_boxed(receiver, argument)
        }
    }

    /// `compareTo` whose argument is already the scalar (`Comparable<Char>.compareTo('a')`).
    fn compare_unboxed(
        &mut self,
        receiver: u32,
        argument: u32,
        operand: Ty,
    ) -> Result<(), Unsupported> {
        let value =
            carrier(operand)?.ok_or_else(|| format!("`compareTo` on `{}`", describe(operand)))?;
        let form = self
            .scalar_box(operand)?
            .ok_or_else(|| format!("`compareTo` on `{}`", describe(operand)))?;
        self.value(receiver, any())?;
        let receiver_local = self.local(REFERENCE);
        self.code
            .local_tee(receiver_local)
            .ref_test(false, HeapType::Concrete(form.structure))
            .op(EQZ32)
            .if_(BlockType::Empty);
        self.throw_class_cast()?;
        self.code.end().local_get(receiver_local);
        form.emit_unbox(&mut self.code);
        let left = self.local(value);
        self.code.local_set(left);
        self.value(argument, operand)?;
        let right = self.local(value);
        self.code.local_set(right);
        self.three_way(operand, left, right)
    }

    /// `compareTo` of two references (`T.compareTo` where `T : Comparable<T>`). Both are boxes of
    /// one scalar, and a different box is not a value of that scalar.
    fn compare_boxed(&mut self, receiver: u32, argument: u32) -> Result<(), Unsupported> {
        self.value(receiver, any())?;
        let receiver_local = self.local(REFERENCE);
        self.code.local_set(receiver_local);
        self.value(argument, any())?;
        let argument_local = self.local(REFERENCE);
        self.code.local_set(argument_local);
        self.code.block(BlockType::Value(ValType::I32));
        for operand in [
            Ty::Boolean,
            Ty::Byte,
            Ty::Short,
            Ty::Char,
            Ty::Int,
            Ty::Long,
        ] {
            let Some(form) = self.scalar_box(operand)? else {
                continue;
            };
            let value = carrier(operand)?.expect("a scalar box has a scalar carrier");
            self.code
                .local_get(receiver_local)
                .ref_test(false, HeapType::Concrete(form.structure))
                .if_(BlockType::Empty);
            self.code
                .local_get(argument_local)
                .ref_test(false, HeapType::Concrete(form.structure))
                .op(EQZ32)
                .if_(BlockType::Empty);
            self.throw_class_cast()?;
            self.code.end();
            self.code.local_get(receiver_local);
            form.emit_unbox(&mut self.code);
            let left = self.local(value);
            self.code.local_set(left);
            self.code.local_get(argument_local);
            form.emit_unbox(&mut self.code);
            let right = self.local(value);
            self.code.local_set(right);
            self.three_way(operand, left, right)?;
            self.code.br(1).end();
        }
        self.throw_class_cast()?;
        self.code.end();
        Ok(())
    }

    fn scalar_box(&mut self, ty: Ty) -> Result<Option<crate::boxes::BoxForm>, Unsupported> {
        let Some(value) = carrier(ty)? else {
            return Ok(None);
        };
        if matches!(value, ValType::Ref { .. }) {
            return Ok(None);
        }
        self.runtime.box_form(self.module, ty, value)
    }

    /// The `Number` conversion of the scalar on the stack from `source` to `target`.
    fn convert_number(&mut self, source: Ty, target: Ty) -> Result<(), Unsupported> {
        if source == target {
            return Ok(());
        }
        let from = carrier(source)?
            .ok_or_else(|| format!("a `Number` conversion from `{}`", describe(source)))?;
        let to = carrier(target)?
            .ok_or_else(|| format!("a `Number` conversion to `{}`", describe(target)))?;
        match (from, to) {
            (ValType::I32, ValType::I32)
            | (ValType::I64, ValType::I64)
            | (ValType::F32, ValType::F32)
            | (ValType::F64, ValType::F64) => {}
            (ValType::I32, ValType::I64) => {
                self.code.op(EXTEND_I32_S);
            }
            (ValType::I32, ValType::F32) => {
                self.code.op(0xb2);
            }
            (ValType::I32, ValType::F64) => {
                self.code.op(0xb7);
            }
            (ValType::I64, ValType::I32) => {
                self.code.op(WRAP_I64);
            }
            (ValType::I64, ValType::F32) => {
                self.code.op(0xb4);
            }
            (ValType::I64, ValType::F64) => {
                self.code.op(0xb9);
            }
            (ValType::F32, ValType::I32) => {
                self.code.trunc_sat(0);
            }
            (ValType::F32, ValType::I64) => {
                self.code.trunc_sat(4);
            }
            (ValType::F32, ValType::F64) => {
                self.code.op(0xbb);
            }
            (ValType::F64, ValType::I32) => {
                self.code.trunc_sat(2);
            }
            (ValType::F64, ValType::I64) => {
                self.code.trunc_sat(6);
            }
            (ValType::F64, ValType::F32) => {
                self.code.op(0xb6);
            }
            _ => {
                return Err(format!(
                    "a `Number` conversion from `{}` to `{}`",
                    describe(source),
                    describe(target)
                ));
            }
        }
        match target {
            Ty::Byte => {
                self.code.op(0xc0);
            }
            Ty::Short => {
                self.code.op(0xc1);
            }
            Ty::Char => {
                self.code.i32_const(0xffff).op(AND32);
            }
            _ => {}
        }
        Ok(())
    }

    /// `-1`, `0` or `1` from the two scalar locals.
    fn three_way(&mut self, operand: Ty, left: u32, right: u32) -> Result<(), Unsupported> {
        let value =
            carrier(operand)?.ok_or_else(|| format!("`compareTo` on `{}`", describe(operand)))?;
        let (greater, less) = match value {
            ValType::I32 => (GT_S32, LT_S32),
            ValType::I64 => (GT_S64, LT_S64),
            _ => return Err(format!("`compareTo` on `{}`", describe(operand))),
        };
        self.code
            .local_get(left)
            .local_get(right)
            .op(greater)
            .local_get(left)
            .local_get(right)
            .op(less)
            .op(SUB32);
        Ok(())
    }

    fn throw_class_cast(&mut self) -> Result<(), Unsupported> {
        let failure = self.runtime.exceptions.ensure_class(
            self.module,
            self.classifiers,
            krusty::libraries::RuntimeExceptionKind::ClassCast.classifier(),
        )?;
        self.runtime
            .exceptions
            .construct_class(&mut self.code, failure, None, self.runtime.string);
        self.code.throw(self.runtime.exceptions.tag);
        Ok(())
    }
}

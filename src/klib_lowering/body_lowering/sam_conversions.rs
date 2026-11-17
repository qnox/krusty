//! Serialized SAM conversions consume a declaration selected before backend entry.
//!
//! A producer serializes a conversion in one of two forms: a `SAM_CONVERSION` of a function value,
//! or a lambda whose rich reference has the interface's class type (the Native stdlib's form of
//! `Comparator { a, b -> ... }`). Both select the same checked declaration and attach it the same
//! way; only how the function value is lowered differs.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr, IrSamTarget};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrFunctionId, KlibIrType, KlibIrTypeId};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol};
use crate::types::{Ty, TyKind};

impl BodyLowering<'_> {
    pub(super) fn sam_conversion(
        &mut self,
        operand: KlibIrTypeId,
        argument: KlibIrExprId,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let mut target = self.sam_target(operand, ty)?;
        let source = self.semantic(
            self.arena
                .expr(argument)
                .ty
                .ok_or_else(|| mismatch("a SAM operand has no serialized type"))?,
        )?;
        let TyKind::Fun(function) = source.non_null().kind() else {
            return unsupported("a SAM conversion of a value without a function shape");
        };
        if function.suspend && !target.suspend {
            return Err(mismatch(
                "a suspend function implements a non-suspend SAM method",
            ));
        }
        target.source_suspend = function.suspend;
        target.nullable = source.is_nullable();
        if target.nullable && !ty.is_nullable() {
            return Err(mismatch(
                "a nullable function implements a non-null SAM target",
            ));
        }
        let expected = sam_function_type(&target, function.suspend);
        if !self
            .linker
            .is_subtype(self.header.type_parameters(), source.non_null(), expected)
        {
            return Err(mismatch(
                "a SAM conversion's function differs from its checked method shape",
            ));
        }
        let value = self.expression(argument)?;
        self.converted(value, target, ty)
    }

    /// A lambda serialized with the functional interface's type `ty` (`ty_id` in the arena): the
    /// closure of `invoke` at the checked method's shape, converted like a function value.
    pub(super) fn converted_closure(
        &mut self,
        ty_id: KlibIrTypeId,
        invoke: KlibIrFunctionId,
        overridden: &KlibIrSymbol,
        ty: Ty,
        source_form: Option<&str>,
    ) -> Lowered<ExprId> {
        let target = self.sam_target(ty_id, ty)?;
        // The reference names the declaration it overrides: by exact identity, the checked
        // method or one of the dispatch frames it implements, never a same-named member.
        if !self.overrides_checked_method(ty_id, overridden)? {
            return Err(mismatch(
                "a converted lambda overrides another method than its checked SAM method",
            ));
        }
        if target.suspend {
            return unsupported("a suspend closure");
        }
        let value = self.closure(invoke, sam_function_type(&target, false), source_form)?;
        self.converted(value, target, ty)
    }

    /// The checked SAM declaration of the classifier `operand` names, applied at `ty`.
    fn sam_target(&mut self, operand: KlibIrTypeId, ty: Ty) -> Lowered<IrSamTarget> {
        let target_type = self.semantic(operand)?;
        if target_type != ty {
            return Err(mismatch("a SAM conversion is typed apart from its target"));
        }
        let KlibIrType::Simple { classifier, .. } = self.arena.ty(operand) else {
            return unsupported("a SAM conversion without a nominal target");
        };
        let KlibIrSignature::Public(signature) = &classifier.signature else {
            return unsupported("a SAM conversion to a non-public classifier");
        };
        let declaration = self.linker.sam_declaration(signature)?;
        declaration.applied(target_type.non_null()).ok_or_else(|| {
            mismatch("a SAM conversion differs from its checked classifier application")
        })
    }

    /// Whether `overridden` is one of the declarations the checked SAM declaration of the
    /// classifier `operand` names allows a converted lambda to override.
    fn overrides_checked_method(
        &self,
        operand: KlibIrTypeId,
        overridden: &KlibIrSymbol,
    ) -> Lowered<bool> {
        let KlibIrType::Simple { classifier, .. } = self.arena.ty(operand) else {
            return unsupported("a SAM conversion without a nominal target");
        };
        let KlibIrSignature::Public(signature) = &classifier.signature else {
            return unsupported("a SAM conversion to a non-public classifier");
        };
        let KlibIrSignature::Public(method) = &overridden.signature else {
            return Ok(false);
        };
        let declaration = self.linker.sam_declaration(signature)?;
        Ok(declaration.overridable.iter().any(|callable| {
            callable.declaration
                == crate::libraries::KlibDeclarationSignature::Public(method.clone())
        }))
    }

    /// `value`, a function value of `target`'s method shape, as the functional interface `ty`.
    /// A converted lambda has the interface's type, not its function type: a use that expects
    /// the interface (a field initializer, an argument) must not see an implicit conversion.
    fn converted(&mut self, value: ExprId, mut target: IrSamTarget, ty: Ty) -> Lowered<ExprId> {
        if !target
            .contravariant_parameters
            .iter()
            .any(|&capture| capture)
        {
            if let IrExpr::Lambda {
                impl_fn, sam: None, ..
            } = self.ir.expr(value)
            {
                let implementation = *impl_fn;
                target.wraps_function_value = false;
                target.nullable = false;
                self.ir.lambda_sam_signature.insert(
                    implementation,
                    (target.declared_parameters.clone(), target.declared_result),
                );
                if target.suspend && !self.ir.suspend_funs.contains(&implementation) {
                    self.ir.suspend_funs.push(implementation);
                }
                let IrExpr::Lambda { sam, .. } = &mut self.ir.exprs[value as usize] else {
                    unreachable!()
                };
                *sam = Some(Box::new(target));
                return Ok(self.typed(value, ty));
            }
        }
        let name = format!(
            "$klib_sam_delegate_{}_{}",
            self.function.function,
            self.ir.functions.len()
        );
        let (_, expression) = self
            .ir
            .sam_function_value_adapter(target, value, name)
            .ok_or_else(|| mismatch("a SAM method has too many invocation parameters"))?;
        Ok(self.typed(expression, ty))
    }
}

/// The function type a SAM method's implementation has.
fn sam_function_type(target: &IrSamTarget, suspend: bool) -> Ty {
    Ty::fun_with_shape(
        target.parameters.clone(),
        target.result,
        target.context_count as usize,
        target.has_receiver,
        suspend,
    )
}

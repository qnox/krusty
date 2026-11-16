//! Serialized SAM conversions consume a declaration selected before backend entry.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrType, KlibIrTypeId};
use crate::metadata::klib_ir::KlibIrSignature;
use crate::types::{Ty, TyKind};

impl BodyLowering<'_> {
    pub(super) fn sam_conversion(
        &mut self,
        operand: KlibIrTypeId,
        argument: KlibIrExprId,
        ty: Ty,
    ) -> Lowered<ExprId> {
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
        let mut target = crate::symbol_resolver::apply_sam_declaration(
            &declaration.classifier,
            &declaration.target,
            target_type.non_null(),
        )
        .ok_or_else(|| {
            mismatch("a SAM conversion differs from its checked classifier application")
        })?;
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
        let expected = Ty::fun_with_shape(
            target.parameters.clone(),
            target.result,
            target.context_count as usize,
            target.has_receiver,
            function.suspend,
        );
        if !self
            .linker
            .is_subtype(self.header.type_parameters(), source.non_null(), expected)
        {
            return Err(mismatch(
                "a SAM conversion's function differs from its checked method shape",
            ));
        }
        let value = self.expression(argument)?;
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
                return Ok(value);
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
        Ok(expression)
    }
}

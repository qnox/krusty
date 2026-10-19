//! Exact recovery of Kotlin's serialized inline-argument container.
//!
//! The Native producer has already inlined tiny stdlib wrappers such as floating-point bit
//! conversion before serializing them. It represents one evaluated receiver as two immutable
//! compiler aliases around a returnable/inlined block. Checked common IR has neither producer
//! construct, so this module validates that complete shape and substitutes the single receiver
//! read. Any different compiler block continues to decline.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::ExprId;
use crate::metadata::klib_ir::tree::{KlibIrExprKind, KlibIrStatement, KlibIrVariable};
use crate::metadata::klib_ir::KlibIrSymbolKind;
use crate::types::Ty;

const TEMPORARY: &str = "IR_TEMPORARY_VARIABLE";
const INLINE_PARAMETER: &str = "IR_TEMPORARY_VARIABLE_FOR_INLINED_PARAMETER";

impl BodyLowering<'_> {
    /// Lower the one compiler container shape that is semantically a single evaluation followed
    /// by a call. The two producer aliases are not source locals and therefore do not acquire
    /// common-IR slots or debug identities.
    pub(super) fn inline_argument_container(
        &mut self,
        statements: &[KlibIrStatement],
        ty: Ty,
    ) -> Lowered<ExprId> {
        let [KlibIrStatement::Variable(outer_id), KlibIrStatement::Expression(returnable_id)] =
            statements
        else {
            return unsupported("an inline argument container of another shape");
        };
        let outer = self.arena.variable(*outer_id);
        immutable_alias(outer, TEMPORARY)?;
        let outer_initializer = outer.initializer.ok_or_else(|| {
            mismatch("an inline argument container's temporary has no initializer")
        })?;

        let returnable = self.arena.expr(*returnable_id);
        let KlibIrExprKind::ReturnableBlock {
            symbol: returnable_symbol,
            statements: returnable_statements,
            origin: None,
        } = &returnable.kind
        else {
            return unsupported("an inline argument container without its returnable block");
        };
        let [KlibIrStatement::Expression(inlined_id)] = returnable_statements.as_slice() else {
            return unsupported("an inline returnable block of another shape");
        };

        let inlined = self.arena.expr(*inlined_id);
        let KlibIrExprKind::InlinedFunctionBlock {
            inlined_function: Some(inlined_function),
            statements: inlined_statements,
            origin: None,
            ..
        } = &inlined.kind
        else {
            return unsupported("an inline function block of another shape");
        };
        if inlined_function.kind != KlibIrSymbolKind::Function {
            return Err(mismatch("an inlined function identity is not a function"));
        }
        let [KlibIrStatement::Variable(parameter_id), KlibIrStatement::Expression(return_id)] =
            inlined_statements.as_slice()
        else {
            return unsupported("an inlined function body of another shape");
        };
        let parameter = self.arena.variable(*parameter_id);
        immutable_alias(parameter, INLINE_PARAMETER)?;
        if parameter.base.symbol == outer.base.symbol {
            return Err(mismatch(
                "an inline parameter repeats its argument identity",
            ));
        }
        let parameter_initializer = parameter
            .initializer
            .ok_or_else(|| mismatch("an inlined parameter alias has no argument initializer"))?;
        if !matches!(
            &self.arena.expr(parameter_initializer).kind,
            KlibIrExprKind::GetValue { symbol, .. } if symbol == &outer.base.symbol
        ) {
            return Err(mismatch(
                "an inlined parameter alias does not read its argument temporary",
            ));
        }

        let returned = self.arena.expr(*return_id);
        let KlibIrExprKind::Return { target, value } = &returned.kind else {
            return unsupported("an inlined function block without its value return");
        };
        if target != returnable_symbol {
            return Err(mismatch("an inline return names another returnable block"));
        }
        let call = self.arena.expr(*value);
        let KlibIrExprKind::Call {
            access,
            super_qualifier: None,
        } = &call.kind
        else {
            return unsupported("an inline argument container whose value is not a direct call");
        };
        let arguments = super::calls::arguments(access);
        let [Some(argument)] = arguments.as_slice() else {
            return unsupported("an inline argument container call with another argument shape");
        };
        if !matches!(
            &self.arena.expr(*argument).kind,
            KlibIrExprKind::GetValue { symbol, .. } if symbol == &parameter.base.symbol
        ) {
            return Err(mismatch(
                "an inline argument container call does not read its parameter alias",
            ));
        }

        let outer_ty = super::super::klib_types::semantic_type(
            self.arena,
            self.header.type_parameters(),
            outer.ty,
        )?;
        let parameter_ty = super::super::klib_types::semantic_type(
            self.arena,
            self.header.type_parameters(),
            parameter.ty,
        )?;
        if outer_ty != parameter_ty
            || self.ty(self.arena.expr(outer_initializer), "inline argument")? != outer_ty
            || self.ty(
                self.arena.expr(parameter_initializer),
                "inline parameter alias",
            )? != outer_ty
            || self.ty(returnable, "returnable inline block")? != ty
            || self.ty(inlined, "inlined function block")? != ty
            || self.ty(call, "inlined call")? != ty
            || self.ty(returned, "inline return")? != Ty::Nothing
        {
            return Err(mismatch(
                "an inline argument container has inconsistent types",
            ));
        }

        if self
            .compiler_aliases
            .insert(parameter.base.symbol.clone(), outer_initializer)
            .is_some()
        {
            return Err(mismatch("an inline parameter identity is already active"));
        }
        let lowered = self.operand(*value, ty);
        self.compiler_aliases.remove(&parameter.base.symbol);
        lowered
    }
}

fn immutable_alias(variable: &KlibIrVariable, expected_origin: &'static str) -> Lowered<()> {
    if variable.base.origin != expected_origin {
        return unsupported("an inline container variable of another origin");
    }
    if variable.base.flags != 0 || !variable.base.annotations.is_empty() {
        return Err(mismatch(
            "an inline container alias is not a plain immutable compiler temporary",
        ));
    }
    Ok(())
}

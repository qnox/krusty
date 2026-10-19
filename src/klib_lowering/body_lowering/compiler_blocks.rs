//! The blocks kotlinc introduces when it prepares an inline function's body.
//!
//! A prepared body has the inline calls it makes already inlined. Each inlined call evaluates its
//! arguments into compiler temporaries of an `INLINE_ARGS_CONTAINER` block, which then holds a
//! returnable block whose `return`s name it, around the inlined function's own block. Checked
//! common IR has none of these constructs. The module inliner represents an expansion whose only
//! `return` is its last statement as a block whose value is that `return`'s value, and so does this
//! lowering; a returnable block with another `return` declines. An immutable temporary initialized
//! with a constant or a read of an immutable value is that value, read where the temporary is; any other
//! temporary is an unnamed local.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrBindingStability, IrExpr};
use crate::metadata::klib_ir::tree::{
    KlibIrExprId, KlibIrExprKind, KlibIrStatement, KlibIrVariableId,
};
use crate::metadata::klib_ir::KlibIrSymbol;
use crate::types::Ty;

/// The origins of the temporaries a prepared body's inlined calls declare.
const TEMPORARIES: [&str; 2] = [
    "IR_TEMPORARY_VARIABLE",
    "IR_TEMPORARY_VARIABLE_FOR_INLINED_PARAMETER",
];

impl BodyLowering<'_> {
    /// An `INLINE_ARGS_CONTAINER` block typed `ty`: its statements, the last its value unless the
    /// block is typed `Unit`.
    pub(super) fn argument_container(
        &mut self,
        statements: &[KlibIrStatement],
        ty: Ty,
    ) -> Lowered<ExprId> {
        let Some((last, leading)) = statements.split_last() else {
            return Err(mismatch("an inline argument container is empty"));
        };
        let mut stmts = self.compiler_statements(leading)?;
        let value = match last {
            KlibIrStatement::Expression(last) if self.is_compiler_tail(*last) => {
                Some(self.compiler_value(*last, ty)?)
            }
            _ => {
                return unsupported("an inline argument container without its inlined call");
            }
        };
        stmts.extend(value.filter(|_| ty == Ty::Unit));
        let value = value.filter(|_| ty != Ty::Unit);
        let block = self.ir.add_expr(IrExpr::Block { stmts, value });
        Ok(self.typed(block, ty))
    }

    /// A returnable block typed `ty` whose only `return` is its last statement.
    pub(super) fn returnable_block(
        &mut self,
        symbol: &KlibIrSymbol,
        statements: &[KlibIrStatement],
        ty: Ty,
    ) -> Lowered<ExprId> {
        let returns = self.arena.returns_to(statements, symbol);
        let tail_return = self.tail_returns_to(statements, symbol);
        if returns != usize::from(tail_return) || (!tail_return && ty != Ty::Unit) {
            return unsupported("a returnable block with a `return` before its end");
        }
        let block = self.inlined_statements(statements, symbol, ty)?;
        Ok(self.typed(block, ty))
    }

    /// `statements` of a returnable block or of the inlined function block it holds, as a block
    /// whose value is the value the last statement returns to `symbol`.
    fn inlined_statements(
        &mut self,
        statements: &[KlibIrStatement],
        symbol: &KlibIrSymbol,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let Some((last, leading)) = statements.split_last() else {
            return Ok(self.ir.add_expr(IrExpr::Block {
                stmts: Vec::new(),
                value: None,
            }));
        };
        let mut stmts = self.compiler_statements(leading)?;
        let mut value = None;
        match last {
            KlibIrStatement::Expression(last) => match &self.arena.expr(*last).kind {
                KlibIrExprKind::Return {
                    target,
                    value: returned,
                } if target == symbol => {
                    value = Some(self.compiler_value(*returned, ty)?);
                }
                KlibIrExprKind::InlinedFunctionBlock {
                    statements,
                    origin: None,
                    ..
                } => {
                    let inner = self.inlined_statements(statements, symbol, ty)?;
                    value = Some(self.typed(inner, ty));
                }
                _ => stmts.push(self.discarded(*last)?),
            },
            statement => stmts.extend(self.compiler_statements(std::slice::from_ref(statement))?),
        }
        stmts.extend(value.filter(|_| ty == Ty::Unit));
        let value = value.filter(|_| ty != Ty::Unit);
        Ok(self.ir.add_expr(IrExpr::Block { stmts, value }))
    }

    /// Whether the last of `statements` returns to `symbol`, directly or as the last statement of
    /// the inlined function block it is.
    fn tail_returns_to(&self, statements: &[KlibIrStatement], symbol: &KlibIrSymbol) -> bool {
        let Some(KlibIrStatement::Expression(last)) = statements.last() else {
            return false;
        };
        match &self.arena.expr(*last).kind {
            KlibIrExprKind::Return { target, .. } => target == symbol,
            KlibIrExprKind::InlinedFunctionBlock {
                statements,
                origin: None,
                ..
            } => self.tail_returns_to(statements, symbol),
            _ => false,
        }
    }

    /// Whether `id` is the value a compiler block ends in: an expression rather than a jump.
    fn is_compiler_tail(&self, id: KlibIrExprId) -> bool {
        !matches!(
            self.arena.expr(id).kind,
            KlibIrExprKind::Return { .. }
                | KlibIrExprKind::Break { .. }
                | KlibIrExprKind::Continue { .. }
        )
    }

    /// `id` as the value of a compiler block typed `ty`, evaluated for its effect alone in a
    /// `Unit` one.
    fn compiler_value(&mut self, id: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if ty == Ty::Unit {
            self.discarded(id)
        } else {
            self.operand(id, ty)
        }
    }

    /// The statements of a compiler block, of which a temporary that only names another value is
    /// none.
    fn compiler_statements(&mut self, statements: &[KlibIrStatement]) -> Lowered<Vec<ExprId>> {
        let mut lowered = Vec::with_capacity(statements.len());
        for statement in statements {
            match statement {
                KlibIrStatement::Variable(variable)
                    if TEMPORARIES
                        .contains(&self.arena.variable(*variable).base.origin.as_str()) =>
                {
                    lowered.extend(self.temporary(*variable)?);
                }
                statement => lowered.push(self.statement(statement)?),
            }
        }
        Ok(lowered)
    }

    /// A compiler temporary: the value it names when its initializer reads an immutable value,
    /// otherwise an unnamed local holding its initializer's value.
    fn temporary(&mut self, id: KlibIrVariableId) -> Lowered<Option<ExprId>> {
        let variable = self.arena.variable(id);
        if variable.base.flags != 0 || !variable.base.annotations.is_empty() {
            return unsupported("a mutable or annotated compiler temporary");
        }
        let initializer = variable
            .initializer
            .ok_or_else(|| mismatch("a compiler temporary has no initializer"))?;
        let symbol = variable.base.symbol.clone();
        if self.reads_immutable_value(initializer) {
            if self.compiler_aliases.insert(symbol, initializer).is_some() {
                return Err(mismatch("a compiler temporary repeats another's identity"));
            }
            return Ok(None);
        }
        let ty = super::semantic_type(self.arena, self.header.type_parameters(), variable.ty)?;
        let init = self.operand(initializer, ty)?;
        let local = self.locals.temporary(&symbol, ty)?;
        Ok(Some(self.ir.add_expr(IrExpr::Variable {
            index: local.index,
            ty,
            init: Some(init),
            named: false,
        })))
    }

    /// Whether `id` is a value no statement can change: a constant, or a read of a parameter, a
    /// `val` or a temporary.
    fn reads_immutable_value(&self, id: KlibIrExprId) -> bool {
        let symbol = match &self.arena.expr(id).kind {
            KlibIrExprKind::Const(_) => return true,
            KlibIrExprKind::GetValue { symbol, .. } => symbol,
            _ => return false,
        };
        self.compiler_aliases.contains_key(symbol)
            || self.header.value(symbol).is_some()
            || self
                .locals
                .get(symbol)
                .is_some_and(|local| local.stability() == IrBindingStability::Stable)
    }
}

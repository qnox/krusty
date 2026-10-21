//! The blocks kotlinc introduces when it prepares an inline function's body.
//!
//! A prepared body has the inline calls it makes already inlined. Each inlined call evaluates its
//! arguments into compiler temporaries of an `INLINE_ARGS_CONTAINER` block, which then holds a
//! returnable block whose `return`s name it, around the inlined function's own block. Checked
//! common IR has none of these constructs. The module inliner represents an expansion whose only
//! `return` is its last statement as a block whose value is that `return`'s value, and so does this
//! lowering. Branch-local returns instead store into a result temporary and exit a labelled
//! single-iteration loop, the common IR representation of an inline return frame. An immutable
//! temporary initialized with a constant or a read of an immutable value is that value, read where
//! the temporary is; any other temporary is an unnamed local.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrBindingStability, IrConst, IrExpr};
use crate::metadata::klib_ir::tree::{
    KlibIrExprId, KlibIrExprKind, KlibIrStatement, KlibIrVariableId,
};
use crate::metadata::klib_ir::{KlibIrSymbol, KlibIrSymbolKind};
use crate::types::Ty;

/// The origins of the temporaries a prepared body's inlined calls declare.
const TEMPORARIES: [&str; 2] = [
    "IR_TEMPORARY_VARIABLE",
    "IR_TEMPORARY_VARIABLE_FOR_INLINED_PARAMETER",
];

/// One exact serialized return target and its structural common-IR exit.
#[derive(Clone)]
pub(super) struct ReturnableFrame {
    symbol: KlibIrSymbol,
    label: String,
    result: Option<u32>,
    ty: Ty,
}

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

    /// A returnable block typed `ty`, as a direct value for a sole tail return or a structural
    /// return frame when branches return to this exact block.
    pub(super) fn returnable_block(
        &mut self,
        symbol: &KlibIrSymbol,
        statements: &[KlibIrStatement],
        ty: Ty,
    ) -> Lowered<ExprId> {
        if symbol.kind != KlibIrSymbolKind::ReturnableBlock {
            return Err(mismatch(
                "a returnable block has no returnable-block symbol",
            ));
        }
        if self.returnable_targets.contains(symbol) {
            return Err(mismatch(
                "a returnable block repeats an enclosing target's identity",
            ));
        }
        let returns = self.arena.returns_to(statements, symbol);
        let tail_return = self.tail_returns_to(statements, symbol);
        self.returnable_targets.push(symbol.clone());
        let block = if returns != usize::from(tail_return) || (!tail_return && ty != Ty::Unit) {
            self.returnable_frame(symbol, statements, ty)
        } else {
            self.inlined_statements(statements, symbol, ty)
        };
        self.returnable_targets.pop();
        block.map(|block| self.typed(block, ty))
    }

    fn returnable_frame(
        &mut self,
        symbol: &KlibIrSymbol,
        statements: &[KlibIrStatement],
        ty: Ty,
    ) -> Lowered<ExprId> {
        if ty != Ty::Unit && !self.compiler_sequence_diverges(statements) {
            return unsupported("a value-returning block that may complete without a value");
        }
        let condition = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let label = format!("$klib_return_{condition}");
        let result = if ty != Ty::Unit && ty != Ty::Nothing {
            Some(self.locals.anonymous_slot())
        } else {
            None
        };
        self.returnable_frames.push(ReturnableFrame {
            symbol: symbol.clone(),
            label: label.clone(),
            result,
            ty,
        });
        let statements = self.compiler_statements(statements);
        self.returnable_frames.pop();
        let mut statements = statements?;
        statements.push(self.ir.add_expr(IrExpr::Break {
            label: Some(label.clone()),
        }));
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        });
        let mut stmts = Vec::new();
        if let Some(index) = result {
            let initial = self
                .ir
                .add_expr(IrExpr::Const(IrConst::zero_for_value_type(ty)));
            stmts.push(self.ir.add_expr(IrExpr::Variable {
                index,
                ty,
                init: Some(initial),
                named: false,
            }));
        }
        stmts.push(self.ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: None,
            post_test: false,
            label: Some(label),
        }));
        let value = result.map(|index| self.ir.add_expr(IrExpr::GetValue(index)));
        let block = self.ir.add_expr(IrExpr::Block { stmts, value });
        Ok(block)
    }

    /// A return to an enclosing exact block, distinct from a return to the template function.
    pub(super) fn returnable_frame_exit(
        &mut self,
        symbol: &KlibIrSymbol,
        value: KlibIrExprId,
    ) -> Lowered<Option<ExprId>> {
        let Some(frame) = self
            .returnable_frames
            .iter()
            .rev()
            .find(|frame| &frame.symbol == symbol)
            .cloned()
        else {
            return Ok(None);
        };
        let value = self.operand(value, frame.ty)?;
        let value = match frame.result {
            Some(var) => self.ir.add_expr(IrExpr::SetValue { var, value }),
            None => value,
        };
        let exit = self.ir.add_expr(IrExpr::Break {
            label: Some(frame.label),
        });
        Ok(Some(self.ir.add_expr(IrExpr::Block {
            stmts: vec![value, exit],
            value: None,
        })))
    }

    /// The final source statement must leave the block before a non-Unit frame result is read.
    fn compiler_sequence_diverges(&self, statements: &[KlibIrStatement]) -> bool {
        let Some(KlibIrStatement::Expression(last)) = statements.last() else {
            return false;
        };
        match &self.arena.expr(*last).kind {
            KlibIrExprKind::InlinedFunctionBlock {
                statements,
                origin: None,
                ..
            } => self.compiler_sequence_diverges(statements),
            _ => self.diverges(*last),
        }
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
                KlibIrStatement::Expression(expression) => {
                    match &self.arena.expr(*expression).kind {
                        KlibIrExprKind::InlinedFunctionBlock {
                            statements,
                            origin: None,
                            ..
                        } if !self.returnable_frames.is_empty() => {
                            lowered.extend(self.compiler_statements(statements)?);
                        }
                        _ => lowered.push(self.statement(statement)?),
                    }
                }
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
        self.unnamed_local(id).map(Some)
    }

    /// The compiler temporary `id` as an unnamed immutable local holding its initializer's value.
    pub(super) fn unnamed_local(&mut self, id: KlibIrVariableId) -> Lowered<ExprId> {
        let variable = self.arena.variable(id);
        if variable.base.flags != 0 || !variable.base.annotations.is_empty() {
            return unsupported("a mutable or annotated compiler temporary");
        }
        let initializer = variable
            .initializer
            .ok_or_else(|| mismatch("a compiler temporary has no initializer"))?;
        let symbol = variable.base.symbol.clone();
        let ty = super::semantic_type(self.arena, self.header.type_parameters(), variable.ty)?;
        let init = self.operand(initializer, ty)?;
        let local = self.locals.temporary(&symbol, ty)?;
        Ok(self.ir.add_expr(IrExpr::Variable {
            index: local.index,
            ty,
            init: Some(init),
            named: false,
        }))
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

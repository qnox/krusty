//! `while`, `do`-`while` and `for` loops and the `break`s and `continue`s that leave them.
//!
//! Checked FIR lowering gives every loop a label of its own and names that label from each jump,
//! so a jump names its loop rather than the innermost one. A KLIB names a jump's loop by the
//! loop's file-unique identity; the label lowering gives a loop is built from that identity and
//! the unit function, so it is unique in the unit and names nothing else.

use super::super::decline::KlibBodyDeclineReason;
use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{
    Callee, ExprId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrIntrinsic, IrProgressionSource,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrExprKind, KlibIrLoop, KlibIrStatement, KlibIrVariableId,
};
use crate::metadata::klib_ir::KlibIrSignature;
use crate::types::Ty;

impl BodyLowering<'_> {
    /// A source `while` (`post_test` false) or `do`-`while` loop, which records no type: it is a
    /// statement.
    pub(super) fn loop_statement(
        &mut self,
        serialized: &KlibIrLoop,
        post_test: bool,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if ty != Ty::Unit {
            return Err(mismatch("a loop is not typed `Unit`"));
        }
        let expected = if post_test {
            "DO_WHILE_LOOP"
        } else {
            "WHILE_LOOP"
        };
        if serialized.origin.as_deref() != Some(expected) {
            return unsupported("a loop the compiler introduced");
        }
        let Some(body) = serialized.body else {
            return unsupported("a loop without a body");
        };
        // A body written without braces is serialized as its one statement. A `do`-`while` body
        // is serialized as a composite, whose declarations its condition reads.
        let single = [KlibIrStatement::Expression(body)];
        let statements = match &self.arena.expr(body).kind {
            KlibIrExprKind::Block {
                statements,
                origin: None,
            } => statements.as_slice(),
            KlibIrExprKind::Composite {
                statements,
                origin: None,
            } if post_test => statements.as_slice(),
            KlibIrExprKind::Block { .. } => {
                return unsupported("a loop whose body is a block the compiler introduced")
            }
            _ => &single,
        };
        self.labeled_loop(serialized, post_test, statements, None)
    }

    /// A `for` loop, which a KLIB serializes as kotlinc desugars it and checked FIR lowering lowers
    /// an iterator-protocol loop: an unnamed local holding the iterable's iterator, then a `while`
    /// loop on its `hasNext()` whose body block declares the loop variable from its `next()` before
    /// the source body.
    pub(super) fn for_loop(&mut self, statements: &[KlibIrStatement], ty: Ty) -> Lowered<ExprId> {
        if ty != Ty::Unit {
            return Err(mismatch("a `for` loop is not typed `Unit`"));
        }
        if let Some(array_loop) = super::super::for_loops::array_for_loop(self.arena, statements) {
            return self.array_loop(array_loop);
        }
        if let Some(indices_loop) =
            super::super::for_loops::indices_for_loop(self.arena, statements)
        {
            return self.indices_loop(indices_loop);
        }
        if let Some(range_loop) = super::super::for_loops::range_for_loop(self.arena, statements) {
            if let Some(lowered) = self.range_loop(range_loop)? {
                return Ok(lowered);
            }
        }
        let [KlibIrStatement::Variable(iterator), KlibIrStatement::Expression(inner)] = statements
        else {
            return unsupported("a `for` loop of another shape than an iterator and its loop");
        };
        if self.arena.variable(*iterator).base.origin != "FOR_LOOP_ITERATOR" {
            return unsupported("a `for` loop of another shape than an iterator and its loop");
        }
        let KlibIrExprKind::While(serialized) = &self.arena.expr(*inner).kind else {
            return unsupported("a `for` loop of another shape than an iterator and its loop");
        };
        if serialized.origin.as_deref() != Some("FOR_LOOP_INNER_WHILE") {
            return unsupported("a `for` loop of another shape than an iterator and its loop");
        }
        let Some(body) = serialized.body else {
            return unsupported("a loop without a body");
        };
        let KlibIrExprKind::Block {
            statements: body,
            origin,
        } = &self.arena.expr(body).kind
        else {
            return unsupported("a loop whose body is not a block");
        };
        let Some(KlibIrStatement::Variable(binding)) = body.first() else {
            return unsupported("a `for` loop body that does not open with its variable");
        };
        if !matches!(origin.as_deref(), None | Some("FOR_LOOP_INNER_WHILE")) {
            return unsupported("a `for` loop body that does not open with its variable");
        }
        let temporary = match self.arena.variable(*binding).base.origin.as_str() {
            "FOR_LOOP_VARIABLE" => None,
            "IR_TEMPORARY_VARIABLE" if self.is_loop_element(*binding, *iterator) => Some(*binding),
            _ => return unsupported("a `for` loop body that does not open with its variable"),
        };
        let iterator = self.unnamed_local(*iterator)?;
        let lowered = self.labeled_loop(serialized, false, body, temporary)?;
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: vec![iterator, lowered],
            value: None,
        }))
    }

    /// A `for` loop over an array, by index as kotlinc's `ForLoopsLowering` and checked
    /// FIR lowering loop over one: the array is evaluated once, its size is read once, and each
    /// iteration binds the element at the index before the source body.
    fn array_loop(
        &mut self,
        array_loop: super::super::for_loops::ArrayForLoop<'_>,
    ) -> Lowered<ExprId> {
        let super::super::for_loops::ArrayForLoop {
            array,
            inner: serialized,
            body: statements,
            binding,
            ..
        } = array_loop;
        let destructured = self.arena.variable(binding).base.origin == "IR_TEMPORARY_VARIABLE";
        if self.loops.iter().any(|(id, _)| *id == serialized.id) {
            return Err(mismatch("a loop repeats an enclosing loop's identity"));
        }
        let array_value = self.expression(array)?;
        let array_ty = self.lowered_type(array_value);
        let element_ty = array_ty
            .non_null()
            .array_read_elem()
            .ok_or_else(|| mismatch("an iterated array has no element type"))?;
        let array_slot = self.locals.anonymous_slot();
        let array_declaration = self.ir.add_expr(IrExpr::Variable {
            index: array_slot,
            ty: array_ty,
            init: Some(array_value),
            named: false,
        });
        let index_slot = self.locals.anonymous_slot();
        let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let index_declaration = self.ir.add_expr(IrExpr::Variable {
            index: index_slot,
            ty: Ty::Int,
            init: Some(zero),
            named: false,
        });
        let array_read = self.ir.add_expr(IrExpr::GetValue(array_slot));
        let size = self.ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Intrinsic {
                operation: IrIntrinsic::ArraySize,
                ret: Ty::Int,
            }),
            dispatch_receiver: Some(array_read),
            args: Vec::new(),
        });
        let size = self.typed(size, Ty::Int);
        let size_slot = self.locals.anonymous_slot();
        let size_declaration = self.ir.add_expr(IrExpr::Variable {
            index: size_slot,
            ty: Ty::Int,
            init: Some(size),
            named: false,
        });
        let index_read = self.ir.add_expr(IrExpr::GetValue(index_slot));
        let size_read = self.ir.add_expr(IrExpr::GetValue(size_slot));
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Lt,
            lhs: index_read,
            rhs: size_read,
        });
        let condition = self.typed(condition, Ty::Boolean);
        let label = format!("$klib_loop_{}_{}", self.function.function, serialized.id);
        self.loops.push((serialized.id, label.clone()));
        let body = (|| {
            let array_read = self.ir.add_expr(IrExpr::GetValue(array_slot));
            let index_read = self.ir.add_expr(IrExpr::GetValue(index_slot));
            let element = self.ir.add_expr(IrExpr::Call {
                callee: Box::new(Callee::Intrinsic {
                    operation: IrIntrinsic::ArrayGet,
                    ret: element_ty,
                }),
                dispatch_receiver: Some(array_read),
                args: vec![index_read],
            });
            let element = self.typed(element, element_ty);
            let element_binding = self.element_binding(binding, element, destructured)?;
            self.block_with_prefix(vec![element_binding], &statements[1..])
        })();
        self.loops.pop();
        let body = body?;
        let index_read = self.ir.add_expr(IrExpr::GetValue(index_slot));
        let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let next_index = self
            .ir
            .add_arithmetic(IrBinOp::Add, index_read, one, Ty::Int);
        let update = self.ir.add_expr(IrExpr::SetValue {
            var: index_slot,
            value: next_index,
        });
        let lowered = self.ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: Some(update),
            post_test: false,
            label: Some(label),
        });
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: vec![
                array_declaration,
                index_declaration,
                size_declaration,
                lowered,
            ],
            value: None,
        }))
    }

    /// A `for` loop over `array.indices`, counted as kotlinc's `IndicesHandler` counts it: the
    /// receiver is evaluated once, and the loop variable takes each value from zero until its
    /// size. Only the array receiver is modelled; another receiver kind declines, because the
    /// prewalk folded the loop's protocol calls.
    fn indices_loop(
        &mut self,
        indices_loop: super::super::for_loops::IndicesForLoop<'_>,
    ) -> Lowered<ExprId> {
        let super::super::for_loops::IndicesForLoop {
            getter,
            receiver,
            reversed,
            inner: serialized,
            body: statements,
            binding,
        } = indices_loop;
        if let Some(reversed) = reversed {
            let KlibIrSignature::Public(callee) = &reversed.symbol.signature else {
                return Err(mismatch(
                    "a reversed indices loop names a non-public callee",
                ));
            };
            if !self.linker.reverses_progression(callee)? {
                return unsupported("a `for` loop over indices through another progression call");
            }
        }
        match self.linker.indices_receiver(getter)? {
            Some(crate::libraries::IndicesReceiver::Array) => {}
            Some(_) => return unsupported("a `for` loop over the indices of a non-array receiver"),
            None => {
                return unsupported(
                    "a `for` loop over an extension property that is not an array's indices",
                )
            }
        }
        let variable = self.arena.variable(binding);
        if variable.base.origin != "FOR_LOOP_VARIABLE" {
            return unsupported("a destructured `for` loop over indices");
        }
        if self.semantic(variable.ty)? != Ty::Int {
            return Err(mismatch(
                "an indices loop's variable is not typed as the range's element",
            ));
        }
        if self.loops.iter().any(|(id, _)| *id == serialized.id) {
            return Err(mismatch("a loop repeats an enclosing loop's identity"));
        }
        let array_value = self.expression(receiver)?;
        let array_ty = self.lowered_type(array_value);
        if !array_ty.non_null().is_array() {
            return Err(mismatch(
                "the receiver of an array indices loop is no array",
            ));
        }
        let array_slot = self.locals.anonymous_slot();
        let array_declaration = self.ir.add_expr(IrExpr::Variable {
            index: array_slot,
            ty: array_ty,
            init: Some(array_value),
            named: false,
        });
        let array_read = self.ir.add_expr(IrExpr::GetValue(array_slot));
        let size = self.ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Intrinsic {
                operation: IrIntrinsic::ArraySize,
                ret: Ty::Int,
            }),
            dispatch_receiver: Some(array_read),
            args: Vec::new(),
        });
        let size = self.typed(size, Ty::Int);
        // Counting down starts at the last index and ends at the first: `size - 1 downTo 0`.
        let (operation, start, end) = if reversed.is_some() {
            let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
            let last = self.ir.add_arithmetic(IrBinOp::Sub, size, one, Ty::Int);
            let first = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
            (crate::fir::FirRangeOperation::DownTo, last, first)
        } else {
            let first = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
            (crate::fir::FirRangeOperation::Until, first, size)
        };
        let symbol = variable.base.symbol.clone();
        let name = variable.name.clone();
        let local = self.locals.slot(variable, Ty::Int)?;
        self.locals.declare(&symbol, local)?;
        self.locals.record_name(&symbol, &name);
        let label = format!("$klib_loop_{}_{}", self.function.function, serialized.id);
        self.loops.push((serialized.id, label.clone()));
        let body = self.block_with_prefix(Vec::new(), &statements[1..]);
        self.loops.pop();
        let body = body?;
        let counted = self
            .ir
            .add_expr(IrExpr::Checked(Box::new(IrCheckedOperation::RangeLoop {
                variable: local.index,
                variable_name: Some(name.into()),
                counter: Ty::Int,
                source: IrProgressionSource::Literal {
                    operation,
                    start,
                    end,
                },
                unsigned_compare: None,
                body,
                label,
                with_index: None,
            })));
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: vec![array_declaration, counted],
            value: None,
        }))
    }

    /// A `for` loop over a range literal, counted as kotlinc's `ForLoopsLowering` and checked FIR
    /// lowering count one: the bounds are evaluated once, before the loop, and the loop variable
    /// takes each value from the start towards the end. `None` keeps the iterator protocol, when
    /// the linker keeps the call building the range.
    fn range_loop(
        &mut self,
        range_loop: super::super::for_loops::RangeForLoop<'_>,
    ) -> Lowered<Option<ExprId>> {
        let super::super::for_loops::RangeForLoop {
            range,
            start,
            end,
            inner: serialized,
            body: statements,
            binding,
        } = range_loop;
        let KlibIrSignature::Public(callee) = &range.symbol.signature else {
            return Err(mismatch(
                "a range loop's range call names a non-public declaration",
            ));
        };
        let Some(operation) = self.linker.range_literal(callee)? else {
            return Ok(None);
        };
        let variable = self.arena.variable(binding);
        if variable.base.origin != "FOR_LOOP_VARIABLE" {
            return unsupported("a destructured `for` loop over a range literal");
        }
        let bound_ty = |lowering: &Self, bound| {
            lowering
                .arena
                .expr(bound)
                .ty
                .ok_or_else(|| mismatch("a range literal bound has no type"))
                .and_then(|ty| lowering.semantic(ty))
        };
        let start_ty = bound_ty(self, start)?;
        let end_ty = bound_ty(self, end)?;
        // Mixed bounds widen, and an unsigned counter compares through a provider-selected
        // operation, which this form does not record.
        let counter = match Ty::range_counter_type_for(start_ty, end_ty) {
            Some(counter @ (Ty::Int | Ty::Long | Ty::Char))
                if start_ty == counter && end_ty == counter =>
            {
                counter
            }
            _ => return unsupported("a range literal loop over mixed or unsigned bounds"),
        };
        if self.semantic(variable.ty)? != counter {
            return Err(mismatch(
                "a range loop's variable is not typed as the range's element",
            ));
        }
        if self.loops.iter().any(|(id, _)| *id == serialized.id) {
            return Err(mismatch("a loop repeats an enclosing loop's identity"));
        }
        let start = self.operand(start, counter)?;
        let end = self.operand(end, counter)?;
        let symbol = variable.base.symbol.clone();
        let name = variable.name.clone();
        let local = self.locals.slot(variable, counter)?;
        self.locals.declare(&symbol, local)?;
        self.locals.record_name(&symbol, &name);
        let label = format!("$klib_loop_{}_{}", self.function.function, serialized.id);
        self.loops.push((serialized.id, label.clone()));
        let body = self.block_with_prefix(Vec::new(), &statements[1..]);
        self.loops.pop();
        let body = body?;
        Ok(Some(self.ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::RangeLoop {
                variable: local.index,
                variable_name: Some(name.into()),
                counter,
                source: IrProgressionSource::Literal {
                    operation,
                    start,
                    end,
                },
                unsigned_compare: None,
                body,
                label,
                with_index: None,
            },
        )))))
    }

    /// The loop variable `binding` (or a destructured loop's element temporary) declared from
    /// the element an array loop reads, in place of the iterator's `next()` it is serialized with.
    fn element_binding(
        &mut self,
        binding: KlibIrVariableId,
        element: ExprId,
        destructured: bool,
    ) -> Lowered<ExprId> {
        let variable = self.arena.variable(binding);
        let ty = self.semantic(variable.ty)?;
        let element_ty = self.lowered_type(element);
        let init = if super::is_value_of(element_ty, ty) {
            element
        } else {
            self.upcast(element, element_ty, ty)
                .ok_or(KlibBodyDeclineReason::ImplicitConversion)?
        };
        let symbol = variable.base.symbol.clone();
        if destructured {
            let local = self.locals.temporary(&symbol, ty)?;
            return Ok(self.ir.add_expr(IrExpr::Variable {
                index: local.index,
                ty,
                init: Some(init),
                named: false,
            }));
        }
        let name = variable.name.clone();
        let local = self.locals.slot(variable, ty)?;
        self.locals.declare(&symbol, local)?;
        self.locals.record_name(&symbol, &name);
        Ok(self.ir.add_expr(IrExpr::Variable {
            index: local.index,
            ty,
            init: Some(init),
            named: true,
        }))
    }

    /// A destructured loop binding is an immutable temporary of the iterator's next element.
    /// Its call still goes through ordinary selected-callee lowering; the serialized protocol
    /// origin and exact receiver identity establish only this temporary's place in the loop.
    fn is_loop_element(&self, binding: KlibIrVariableId, iterator: KlibIrVariableId) -> bool {
        let Some(initializer) = self.arena.variable(binding).initializer else {
            return false;
        };
        let KlibIrExprKind::Call {
            access,
            super_qualifier: None,
        } = &self.arena.expr(initializer).kind
        else {
            return false;
        };
        if access.origin.as_deref() != Some("FOR_LOOP_NEXT") {
            return false;
        }
        let receiver = match &access.arguments {
            KlibIrArguments::Flat(arguments) => match arguments.as_slice() {
                [Some(receiver)] => *receiver,
                _ => return false,
            },
            KlibIrArguments::Split {
                dispatch_receiver: Some(receiver),
                extension_receiver: None,
                values,
            } if values.is_empty() => *receiver,
            _ => return false,
        };
        matches!(
            &self.arena.expr(receiver).kind,
            KlibIrExprKind::GetValue { symbol, .. }
                if symbol == &self.arena.variable(iterator).base.symbol
        )
    }

    /// A loop labeled for the jumps that name `serialized`, whose body block holds `statements`.
    /// A loop's body block, opening with the element `temporary` binds when there is one.
    fn loop_body(
        &mut self,
        statements: &[KlibIrStatement],
        temporary: Option<KlibIrVariableId>,
    ) -> Lowered<ExprId> {
        let (prefix, statements) = match temporary {
            Some(binding) => (vec![self.unnamed_local(binding)?], &statements[1..]),
            None => (Vec::new(), statements),
        };
        self.block_with_prefix(prefix, statements)
    }

    fn labeled_loop(
        &mut self,
        serialized: &KlibIrLoop,
        post_test: bool,
        statements: &[KlibIrStatement],
        temporary: Option<KlibIrVariableId>,
    ) -> Lowered<ExprId> {
        if self.loops.iter().any(|(id, _)| *id == serialized.id) {
            return Err(mismatch("a loop repeats an enclosing loop's identity"));
        }
        let label = format!("$klib_loop_{}_{}", self.function.function, serialized.id);
        self.loops.push((serialized.id, label.clone()));
        // A `do`-`while` condition is in the scope of its body, so it may read a variable the
        // body declares: the body is lowered first and declares it.
        let lowered = if post_test {
            self.loop_body(statements, temporary)
                .and_then(|body| Ok((self.operand(serialized.condition, Ty::Boolean)?, body)))
        } else {
            self.operand(serialized.condition, Ty::Boolean)
                .and_then(|cond| Ok((cond, self.loop_body(statements, temporary)?)))
        };
        self.loops.pop();
        let (cond, body) = lowered?;
        Ok(self.ir.add_expr(IrExpr::While {
            cond,
            body,
            update: None,
            post_test,
            label: Some(label),
        }))
    }

    /// `break` (`resume` false) or `continue` (`resume` true) of the enclosing loop `loop_id`.
    pub(super) fn jump(&mut self, loop_id: u32, ty: Ty, resume: bool) -> Lowered<ExprId> {
        if ty != Ty::Nothing {
            return Err(mismatch("a `break` or `continue` is not typed `Nothing`"));
        }
        let Some(label) = self
            .loops
            .iter()
            .rev()
            .find(|(id, _)| *id == loop_id)
            .map(|(_, label)| label.clone())
        else {
            return Err(mismatch("a `break` or `continue` names no enclosing loop"));
        };
        if !resume {
            self.breakable_loops.insert(loop_id);
        }
        let label = Some(label);
        Ok(self.ir.add_expr(if resume {
            IrExpr::Continue { label }
        } else {
            IrExpr::Break { label }
        }))
    }
}

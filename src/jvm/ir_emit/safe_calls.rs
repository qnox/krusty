//! JVM layout for safe-call guards and chains.

use super::{discard, when, CodeBuilder, Emitter, IrBinOp, IrConst, IrExpr, IrTypeOp, Ty};

impl Emitter<'_> {
    /// Emit a discarded safe call with the discard inside its guard, as kotlinc writes it. The
    /// selector runs as a statement and the null path leaves nothing to pop.
    pub(super) fn emit_discarded_safe_call(
        &mut self,
        expression: u32,
        node: &IrExpr,
        code: &mut CodeBuilder,
    ) -> bool {
        if let IrExpr::Block {
            value: Some(value), ..
        } = node
        {
            let shared = matches!(self.safe_call_null_exits.get(value), Some((_, false)));
            if self.ir.null_guards.contains(value) && !shared {
                self.emit(expression, code);
                return true;
            }
        }
        if let IrExpr::When { branches } = node {
            if let [(Some(guard), null_result), (None, selector)] = branches.as_slice() {
                let shared = matches!(self.safe_call_null_exits.get(&expression), Some((_, false)));
                if self.ir.null_guards.contains(&expression) && !shared {
                    let end = code.new_label();
                    let entry_height = code.stack_height().max(0) as u16;
                    let elvis = self.ir.elvis_safe_call_guards.contains(&expression);
                    let result_ty = if elvis {
                        self.value_ty_of_when(branches)
                    } else {
                        Ty::Unit
                    };
                    self.emit_safe_call_guard(
                        expression,
                        (*guard, *null_result, *selector),
                        when::Emission::new(!elvis, result_ty, entry_height, end, None),
                        code,
                    );
                    if elvis && !self.diverges(expression) {
                        discard(result_ty, code);
                    }
                    return true;
                }
            }
        }
        false
    }

    pub(super) fn emit_safe_call_when(
        &mut self,
        expression: u32,
        branches: &[(Option<u32>, u32)],
        emission: when::Emission,
        code: &mut CodeBuilder,
    ) -> bool {
        if !self.ir.null_guards.contains(&expression) {
            return false;
        }
        let [(Some(guard), null_result), (None, selector)] = branches else {
            return false;
        };
        self.emit_safe_call_guard(
            expression,
            (*guard, *null_result, *selector),
            emission,
            code,
        );
        true
    }

    /// A safe call whose receiver is itself a safe call yielding `null` — `a?.b?.c` — has one null
    /// exit, as kotlinc's safe-call chain folding gives it: the inner guard jumps straight to the
    /// outer guard's null path, which the outer guard binds. `block` is a safe call's block; this
    /// links its guard with the guard initializing its receiver temporary.
    pub(super) fn link_safe_call_chain(&mut self, block: u32, code: &mut CodeBuilder) {
        let guard_of = |emitter: &Self, block: u32| match emitter.ir.expr(block) {
            IrExpr::Block {
                stmts,
                value: Some(value),
            } if emitter.ir.null_guards.contains(value) => Some((stmts.clone(), *value)),
            _ => None,
        };
        let Some((stmts, outer)) = guard_of(self, block) else {
            return;
        };
        let [variable] = stmts.as_slice() else {
            return;
        };
        let IrExpr::Variable {
            index: temporary,
            init: Some(init),
            ..
        } = *self.ir.expr(*variable)
        else {
            return;
        };
        let Some((_, inner)) = guard_of(self, init) else {
            return;
        };
        // Both guards must take the guard layout, or the shared exit would never be bound.
        let guard_shaped = |emitter: &Self, guard: u32| match emitter.ir.expr(guard) {
            IrExpr::When { branches } => {
                matches!(branches.as_slice(), [(Some(_), _), (None, _)]).then(|| branches.clone())
            }
            _ => None,
        };
        if guard_shaped(self, outer).is_none() {
            return;
        }
        let Some(branches) = guard_shaped(self, inner) else {
            return;
        };
        let [(Some(_), null_result), (None, selector)] = branches.as_slice() else {
            return;
        };
        if !matches!(self.ir.expr(*null_result), IrExpr::Const(IrConst::Null))
            || self.diverges(*selector)
        {
            return;
        }
        let label = match self.safe_call_null_exits.get(&outer) {
            Some(&(label, _)) => label,
            None => {
                let label = code.new_label();
                self.safe_call_null_exits.insert(outer, (label, true));
                label
            }
        };
        self.safe_call_null_exits.insert(inner, (label, false));
        // This block's temporary is stored after the inner guard's jump.
        self.safe_call_exit_temporaries
            .entry(label)
            .or_default()
            .push(temporary);
    }

    /// `dup; ifnull; <selector>; goto; pop` for a safe call whose receiver is one word and read
    /// once, as the call or property receiver. kotlinc never stores that receiver. A chain, a wide
    /// receiver, or any other use keeps the temporary the lowerer introduced.
    pub(super) fn try_emit_duplicated_safe_call(
        &mut self,
        block: u32,
        stmts: &[u32],
        value: Option<u32>,
        discarded: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some(plan) = duplicated_safe_call(self, stmts, value) else {
            return false;
        };
        self.mark_statement_line(plan.declaration, code);
        let source = self.emit_consumed_operand(plan.init, code);
        if self.diverges(plan.init) {
            return true;
        }
        let semantic = self
            .ir
            .logical_types
            .get(&plan.init)
            .copied()
            .unwrap_or(source);
        // `this?.close()` duplicates the inlined receiver. That receiver is stored as its erased
        // bound (`T : AutoCloseable?` is `AutoCloseable`). Narrowing the duplicate to the call-site
        // class and widening it again for the member leaves a pair of `checkcast`s kotlinc never
        // writes; the null check and the call both see the bound.
        let slot_ty = body_local_read_slot(self, plan.init)
            .filter(|slot| {
                plan.slot_ty.is_reference() && slot.is_reference() && *slot != plan.slot_ty
            })
            .unwrap_or(plan.slot_ty);
        let erased_receiver = slot_ty != plan.slot_ty;
        self.adapt_physical_operand(source, semantic, Some(plan.semantic_ty), slot_ty, code);
        code.dup();
        let null_path = code.new_label();
        let end = code.new_label();
        self.mark_statement_line(plan.guard, code);
        code.ifnull(null_path);
        // The selector's receiver read is this stack value. Publishing the erased slot type makes
        // that read report the bound, so the member call does not `checkcast` back to it.
        let previous_type = erased_receiver.then(|| self.var_types.insert(plan.temporary, slot_ty));
        self.stack_resident_value = Some(plan.temporary);
        let selector_diverges = if discarded {
            self.emit_discarding(plan.selector, code);
            self.discarding_diverges(plan.selector)
        } else {
            self.emit_value(plan.selector, code);
            self.diverges(plan.selector)
        };
        if let Some(previous_type) = previous_type {
            match previous_type {
                Some(previous) => {
                    self.var_types.insert(plan.temporary, previous);
                }
                None => {
                    self.var_types.remove(&plan.temporary);
                }
            }
        }
        if self.stack_resident_value.is_some() {
            self.run.set_emit_error(
                "safe-call receiver left on the stack was not consumed by its selector".to_string(),
            );
            self.stack_resident_value = None;
        }
        if !selector_diverges {
            code.goto(end);
        }
        self.bind(null_path, code);
        // An inlined selector forgets the line in effect (`markLineNumberAfterInlineIfNeeded`).
        // kotlinc's next mark is this null path, so the caller's line starts at the `pop` and the
        // shared `areturn` does not gain a second entry. The temporary and the guard are
        // generated and carry no statement line; the block does.
        mark_safe_call_line(self, block, code);
        code.pop();
        if !discarded {
            self.emit_value(plan.null_result, code);
        }
        self.bind(end, code);
        true
    }
}

/// Whether `expression` is the result block of a source safe call.
///
/// The generated null guard is the stable semantic marker. Its receiver may be statically non-null
/// (an unnecessary safe call), in which case JVM cleanup removes the guard but kotlinc still does
/// not add an implicit-return line after an inlined selector.
pub(super) fn is_safe_call_result(ir: &crate::ir::IrFile, mut expression: u32) -> bool {
    loop {
        match ir.expr(expression) {
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => expression = *arg,
            IrExpr::Block {
                value: Some(value), ..
            } => return ir.null_guards.contains(value),
            _ => return false,
        }
    }
}

/// The safe call's own source line, mapped the way every other caller expression is.
///
/// The duplicated temporary and its null guard are generated and have no statement line. The
/// block the lowerer built for the safe call does.
fn mark_safe_call_line(emitter: &mut Emitter<'_>, block: u32, code: &mut CodeBuilder) {
    let Some(line) = emitter
        .ir
        .expr_source_lines
        .get(&block)
        .copied()
        .or_else(|| emitter.ir.expr_lines.get(&block).copied())
        .filter(|line| *line != 0)
    else {
        return;
    };
    emitter.mark_expression_line(block, line, code);
}

/// The physical slot read by `expression` in the body currently owned by `emitter`.
///
/// Value indices restart for every inline body. `Emitter::var_types` is replaced when entering one,
/// so it is the ownership-safe source of a read's representation; scanning the file's declarations
/// by numeric index can bind an unrelated declaration from an adjacent or nested body.
fn body_local_read_slot(emitter: &Emitter<'_>, mut expression: u32) -> Option<Ty> {
    loop {
        match emitter.ir.expr(expression) {
            IrExpr::GetValue(value) => return emitter.var_types.get(value).copied(),
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => expression = *arg,
            _ => return None,
        }
    }
}

struct DuplicatedSafeCall {
    declaration: u32,
    temporary: u32,
    init: u32,
    semantic_ty: Ty,
    slot_ty: Ty,
    guard: u32,
    selector: u32,
    null_result: u32,
}

fn duplicated_safe_call(
    emitter: &Emitter<'_>,
    stmts: &[u32],
    value: Option<u32>,
) -> Option<DuplicatedSafeCall> {
    let [variable] = stmts else {
        return None;
    };
    let IrExpr::Variable {
        index: temporary,
        ty: semantic_ty,
        init: Some(init),
        named: false,
        ..
    } = emitter.ir.expr(*variable)
    else {
        return None;
    };
    let temporary = *temporary;
    let init = *init;
    let semantic_ty = *semantic_ty;
    let slot_ty =
        super::local_variable_representation::slot_type(emitter.ir, *variable, semantic_ty);
    // `ifnull` consumes a reference. A nullable primitive is unboxed before the
    // selector, so its one-word value is an `int` and keeps the temporary.
    if !duplicable_receiver_slot(slot_ty) {
        return None;
    }
    let guard = value?;
    if !emitter.ir.null_guards.contains(&guard) || emitter.safe_call_null_exits.contains_key(&guard)
    {
        return None;
    }
    let IrExpr::When { branches } = emitter.ir.expr(guard) else {
        return None;
    };
    let [(Some(condition), null_result), (None, selector)] = branches.as_slice() else {
        return None;
    };
    if !is_null_equality(emitter.ir, *condition, temporary) {
        return None;
    }
    // A suspension point opens its `beforeInlineCall` bracket before its receiver, and FixStack
    // saves every value already on the stack when the bracket opens. A receiver left there by
    // `dup` would be saved away from its own invoke, so the temporary stays; the bytecode
    // optimizer folds its store and load into kotlinc's `dup` after the coroutine transform.
    if selector_call(emitter.ir, *selector)
        .is_some_and(|call| emitter.transformed_result(call).is_some())
    {
        return None;
    }
    selector_reads_temporary_once_as_receiver(emitter.ir, *selector, temporary).then_some(
        DuplicatedSafeCall {
            declaration: *variable,
            temporary,
            init,
            semantic_ty,
            slot_ty,
            guard,
            selector: *selector,
            null_result: *null_result,
        },
    )
}

fn duplicable_receiver_slot(slot_ty: Ty) -> bool {
    super::slot_words(slot_ty) == 1 && !slot_ty.is_jvm_scalar()
}

fn is_null_equality(ir: &crate::ir::IrFile, condition: u32, temporary: u32) -> bool {
    let IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs,
        rhs,
    } = ir.expr(condition)
    else {
        return false;
    };
    let read = |expression: u32| matches!(ir.expr(expression), IrExpr::GetValue(value) if *value == temporary);
    let null = |expression: u32| matches!(ir.expr(expression), IrExpr::Const(IrConst::Null));
    (read(*lhs) && null(*rhs)) || (read(*rhs) && null(*lhs))
}

fn selector_reads_temporary_once_as_receiver(
    ir: &crate::ir::IrFile,
    selector: u32,
    temporary: u32,
) -> bool {
    if value_reads(ir, selector, temporary) != 1 {
        return false;
    }
    let mut expression = selector;
    loop {
        match ir.expr(expression) {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => expression = *arg,
            IrExpr::Call {
                dispatch_receiver,
                args,
                ..
            } => {
                let receiver = match dispatch_receiver {
                    Some(receiver) => Some(*receiver),
                    None => ir
                        .static_extension_receivers
                        .get(&expression)
                        .copied()
                        .and_then(|position| args.get(position as usize).copied()),
                };
                return receiver.is_some_and(|receiver| reads_temporary(ir, receiver, temporary));
            }
            IrExpr::PropertyRead {
                receiver: Some(receiver),
                ..
            } => return reads_temporary(ir, *receiver, temporary),
            _ => return false,
        }
    }
}

/// The call or property read a selector performs, under its implicit coercions.
fn selector_call(ir: &crate::ir::IrFile, mut selector: u32) -> Option<u32> {
    loop {
        match ir.expr(selector) {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => selector = *arg,
            IrExpr::Call { .. } | IrExpr::PropertyRead { .. } => return Some(selector),
            _ => return None,
        }
    }
}

fn reads_temporary(ir: &crate::ir::IrFile, expression: u32, temporary: u32) -> bool {
    let mut expression = expression;
    loop {
        match ir.expr(expression) {
            IrExpr::GetValue(value) => return *value == temporary,
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => expression = *arg,
            _ => return false,
        }
    }
}

fn value_reads(ir: &crate::ir::IrFile, root: u32, temporary: u32) -> usize {
    let mut count = usize::from(matches!(
        ir.expr(root),
        IrExpr::GetValue(value) if *value == temporary
    ));
    crate::ir::for_each_child(&ir.exprs, root, &mut |child| {
        count += value_reads(ir, child, temporary);
    });
    count
}

#[cfg(test)]
mod tests {
    use super::{duplicable_receiver_slot, selector_reads_temporary_once_as_receiver};
    use crate::ir::{Callee, IrExpr, IrFile};

    #[test]
    fn only_a_one_word_reference_receiver_can_stay_on_the_stack() {
        assert!(duplicable_receiver_slot(crate::types::Ty::obj("app/Host")));
        assert!(!duplicable_receiver_slot(crate::types::Ty::Int));
        assert!(!duplicable_receiver_slot(crate::types::Ty::Long));
    }

    #[test]
    fn a_selector_with_a_second_receiver_read_keeps_the_temporary() {
        let mut ir = IrFile::default();
        let receiver = ir.add_expr(IrExpr::GetValue(7));
        let argument = ir.add_expr(IrExpr::GetValue(7));
        let selector = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(0),
            dispatch_receiver: Some(receiver),
            args: vec![argument],
        });

        assert!(!selector_reads_temporary_once_as_receiver(&ir, selector, 7));
    }

    #[test]
    fn a_static_call_requires_the_recorded_extension_receiver_position() {
        let mut ir = IrFile::default();
        let receiver = ir.add_expr(IrExpr::GetValue(7));
        let selector = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(0),
            dispatch_receiver: None,
            args: vec![receiver],
        });

        assert!(!selector_reads_temporary_once_as_receiver(&ir, selector, 7));
        ir.static_extension_receivers.insert(selector, 0);
        assert!(selector_reads_temporary_once_as_receiver(&ir, selector, 7));
    }
}

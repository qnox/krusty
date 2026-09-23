//! JVM layout for safe-call guards and chains.

use super::{when, CodeBuilder, Emitter, IrConst, IrExpr, Ty};

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
            if self.ir.safe_call_guards.contains(value) && !shared {
                self.emit(expression, code);
                return true;
            }
        }
        if let IrExpr::When { branches } = node {
            if let [(Some(guard), null_result), (None, selector)] = branches.as_slice() {
                let shared = matches!(self.safe_call_null_exits.get(&expression), Some((_, false)));
                if self.ir.safe_call_guards.contains(&expression) && !shared {
                    let end = code.new_label();
                    let entry_height = code.stack_height().max(0) as u16;
                    self.emit_safe_call_guard(
                        expression,
                        (*guard, *null_result, *selector),
                        when::Emission::new(true, Ty::Unit, &[], entry_height, end, None),
                        code,
                    );
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
        emission: when::Emission<'_>,
        code: &mut CodeBuilder,
    ) -> bool {
        if !self.ir.safe_call_guards.contains(&expression) {
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
            } if emitter.ir.safe_call_guards.contains(value) => Some((stmts.clone(), *value)),
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
}

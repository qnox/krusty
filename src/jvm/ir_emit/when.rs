//! `when` expressions (and every `if` the frontend lowers to one): type joins, JVM integer
//! switches, and the chain of conditional branches.

use super::*;
use crate::types::TyKind;

mod java_hash_map;
mod switch;

/// Whether a discarded `when` still joins a value, as kotlinc's `visitWhen` has it: only a `when`
/// that is not exhaustive, or whose type is `Unit`, discards each branch's value in the branch
/// (outside a switch: see [`Emitter::is_unit_switch`]).
/// Any other materializes every branch at its type, and the statement discarding the `when` pops
/// the joined value once; `PopBackwardPropagation` then drops that `pop` where the branches'
/// pushes are cheap enough to drop with it. Whether a `when` or `if` ending in an `else if` chain
/// is exhaustive is recorded by common lowering, as fir2ir decides its type.
pub(super) fn keeps_discarded_value(exhaustive: bool, result_ty: Ty) -> bool {
    exhaustive && !matches!(result_ty, Ty::Unit | Ty::Nothing)
}

/// The `pop` of a discarded `when`'s joined value, when it keeps one and a branch reaches the join.
pub(super) fn discard_joined_value(keeps_value: bool, result_ty: Ty, code: &mut CodeBuilder) {
    if keeps_value && !code.is_dead() {
        discard(result_ty, code);
    }
}

/// Every live arrival at a value `when`'s end carries the entry operands and the joined value. The
/// linear counter shows that only when the last arm falls through: after a throwing last arm (the
/// implicit `NoWhenBranchMatchedException`, an `else throw`, a safe call's throwing null path) it
/// is back at the entry height, and a consumer that discards by the counter would leave the joined
/// value behind.
fn land_joined_value(is_stmt: bool, entry_height: u16, result_ty: Ty, code: &mut CodeBuilder) {
    if !is_stmt && !code.is_dead() {
        code.set_stack(entry_height + slot_words(result_ty));
    }
}

/// The merge contract shared by every emitted case body.
pub(super) struct Emission {
    is_stmt: bool,
    result_ty: Ty,
    entry_height: u16,
    end: Label,
    terminal_target: Option<Label>,
}

impl Emission {
    pub(super) fn new(
        is_stmt: bool,
        result_ty: Ty,
        entry_height: u16,
        end: Label,
        terminal_target: Option<Label>,
    ) -> Self {
        Self {
            is_stmt,
            result_ty,
            entry_height,
            end,
            terminal_target,
        }
    }
}

impl Emitter<'_> {
    /// The failure an exhaustive `when` without an `else` reaches when no branch matches.
    pub(super) fn emit_no_when_branch_matched(&mut self, code: &mut CodeBuilder) {
        let exception = self.cw.class_ref("kotlin/NoWhenBranchMatchedException");
        code.new_obj(exception);
        code.dup();
        let constructor = self
            .cw
            .methodref("kotlin/NoWhenBranchMatchedException", "<init>", "()V");
        code.invokespecial(constructor, 0, 0);
        code.athrow();
    }

    pub(super) fn value_ty_of_when(&self, branches: &[(Option<u32>, u32)]) -> Ty {
        if !branches.iter().any(|(condition, _)| condition.is_none()) {
            return Ty::Unit;
        }
        // A diverging branch contributes nothing to the merge type.
        let last = branches
            .iter()
            .rev()
            .find(|(_, body)| !self.diverges(*body))
            .map(|(_, body)| self.value_ty(*body))
            .unwrap_or(Ty::Unit);
        // `null` and `Nothing` have no concrete verifier type; use another live branch when present.
        if matches!(last, Ty::Null | Ty::Nothing | Ty::Error) {
            for (_, body) in branches {
                if self.diverges(*body) {
                    continue;
                }
                let ty = self.value_ty(*body);
                if !matches!(ty, Ty::Null | Ty::Nothing | Ty::Error) {
                    return ty;
                }
            }
        }
        // Distinct reference classes meet as Object in the StackMapTable.
        if last.is_reference() {
            let internal = |ty: &Ty| -> Option<String> {
                match ty.kind() {
                    TyKind::STRING => Some("java/lang/String".to_string()),
                    _ if ty.is_array() => Some(type_descriptor(*ty)),
                    TyKind::Obj(name, _) => Some(name.to_string()),
                    _ => None,
                }
            };
            let mut names = branches
                .iter()
                .filter(|(_, body)| !self.diverges(*body))
                .map(|(_, body)| self.value_ty(*body))
                .filter(|ty| !matches!(*ty, Ty::Null | Ty::Nothing | Ty::Error))
                .filter_map(|ty| internal(&ty));
            if let Some(first) = names.next() {
                if names.any(|name| name != first) {
                    return Ty::obj("kotlin/Any");
                }
            }
        }
        last
    }

    /// A safe call's guard laid out as kotlinc writes it: `ifnull` to the null path, the selector
    /// falling through — `aload t; ifnull N; <selector>; goto E; N: <null result>; E:`. A guard
    /// inside a chain (see `link_safe_call_chain`) jumps to the chain's null exit and leaves
    /// its selector's value for the next guard.
    pub(super) fn emit_safe_call_guard(
        &mut self,
        expression: u32,
        (guard, null_result, selector): (u32, u32, u32),
        emission: Emission,
        code: &mut CodeBuilder,
    ) {
        let exit = self.safe_call_null_exits.remove(&expression);
        let null_path = exit.map_or_else(|| code.new_label(), |(label, _)| label);
        // A guard folded to a constant `null` receiver jumps unconditionally: the selector is dead
        // and is not laid down, as `emit_when` treats a constant-false arm.
        let always_null = self.emit_cond_branch(guard, null_path, true, code);
        let emit_arm =
            |emitter: &mut Self, arm: u32, after_diverging: bool, code: &mut CodeBuilder| {
                if emission.is_stmt {
                    // A discarded value is never materialized: its implicit conversion (boxing into the
                    // safe call's nullable result) is not written, and a discarded constant is nothing
                    // at all, not a push and a `pop`.
                    let mut arm = arm;
                    while let IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg,
                        ..
                    } = emitter.ir.expr(arm)
                    {
                        arm = *arg;
                    }
                    match emitter.ir.expr(arm) {
                        // kotlinc still visits the constant, marking its line, and its optimizer leaves
                        // a `nop` where it removes the constant and its `pop`. The `nop` survives only
                        // where that line is new, after an inlined selector. After a selector that
                        // diverges, the label of that mark is dead code kotlinc removes: the null path
                        // keeps neither the line nor the `nop`.
                        IrExpr::Const(_) if after_diverging => {}
                        IrExpr::Const(_) => {
                            emitter.mark_expression_start(arm, code);
                            code.nop();
                        }
                        IrExpr::UnitInstance => {}
                        _ => emitter.emit(arm, code),
                    }
                    emitter.discarding_diverges(arm)
                } else {
                    emitter.emit_value(arm, code);
                    emitter.adapt_physical_operand_for(
                        arm,
                        emitter.value_ty(arm),
                        emission.result_ty,
                        code,
                    );
                    emitter.diverges(arm)
                }
            };
        let selector_boundary = super::safe_calls::safe_call_selector_boundary(self.ir, selector);
        let enclosing_safe_call_selector =
            self.active_safe_call_selector.replace(selector_boundary);
        let selector_diverges = always_null || emit_arm(self, selector, false, code);
        self.active_safe_call_selector = enclosing_safe_call_selector;
        if matches!(exit, Some((_, false))) {
            return;
        }
        if !selector_diverges {
            code.goto(emission.end);
        }
        self.bind(null_path, code);
        // A chain's inner guards jumped here before its later receiver temporaries were stored.
        if let Some(temporaries) = self.safe_call_exit_temporaries.remove(&null_path) {
            self.unassigned_values.extend(temporaries);
        }
        code.set_stack(emission.entry_height);
        let _ = emit_arm(self, null_result, selector_diverges, code);
        self.bind(emission.end, code);
        land_joined_value(
            emission.is_stmt,
            emission.entry_height,
            emission.result_ty,
            code,
        );
    }
}

impl Emitter<'_> {
    pub(super) fn emit_when(
        &mut self,
        expression: u32,
        branches: &[(Option<u32>, u32)],
        discarded: bool,
        code: &mut CodeBuilder,
    ) {
        self.emit_when_as(expression, branches, discarded, false, code);
    }

    /// Whether `expression` is an exhaustive `Unit` `when` that dispatches through a switch.
    /// kotlinc's `SwitchGenerator` materializes every arm of one, `Unit` included, so in statement
    /// position the joined `Unit` is popped (`PopBackwardPropagation` drops both where the method
    /// is small enough for it).
    pub(super) fn is_unit_switch(&self, expression: u32) -> bool {
        let IrExpr::When { branches } = self.ir.expr(expression) else {
            return false;
        };
        let has_else = branches.iter().any(|(c, _)| c.is_none());
        let exhaustive_result = self.ir.whens.exhaustive.get(&expression).copied();
        let result_ty = exhaustive_result
            .map(|result| ir_ty_to_jvm(&result))
            .unwrap_or_else(|| self.value_ty_of_when(branches));
        (has_else || exhaustive_result.is_some())
            && result_ty == Ty::Unit
            && !self.machine_suspensions.contains_key(&expression)
            && self.switch_plan(branches).is_some()
    }

    /// A statement [`Self::is_unit_switch`] `when` that leaves its joined `Unit` on the stack: the
    /// last statement of a block, whose `Unit` value kotlinc discards after the block's locals
    /// end.
    pub(super) fn emit_unit_switch_value(&mut self, expression: u32, code: &mut CodeBuilder) {
        let IrExpr::When { branches } = self.ir.expr(expression).clone() else {
            unreachable!("a unit switch is a `when`");
        };
        self.emit_when_as(expression, &branches, true, true, code);
    }

    fn emit_when_as(
        &mut self,
        expression: u32,
        branches: &[(Option<u32>, u32)],
        discarded: bool,
        keeps_unit: bool,
        code: &mut CodeBuilder,
    ) {
        let end = code.new_label();
        // Each branch starts at the pre-condition height, not the linear counter left by the prior
        // body. Resetting at `next` prevents a phantom operand in later branch bodies.
        let entry_height = code.stack_height().max(0) as u16;
        let has_else = branches.iter().any(|(c, _)| c.is_none());
        let exhaustive_result = self.ir.whens.exhaustive.get(&expression).copied();
        // A no-`else` or `Unit` `when` is a statement, so nothing reaches the stack at `end`.
        // Exhaustive results use their checked JVM erasure; other joins derive it from branch values.
        let result_ty = exhaustive_result
            .map(|result| ir_ty_to_jvm(&result))
            .unwrap_or_else(|| self.value_ty_of_when(branches));
        let is_stmt =
            (!has_else && exhaustive_result.is_none()) || result_ty == Ty::Unit || discarded;
        let enclosing_terminal_target = self.terminal_statement_target.take();
        if self.emit_safe_call_when(
            expression,
            branches,
            Emission::new(is_stmt, result_ty, entry_height, end, None),
            code,
        ) {
            return;
        }
        let exhaustive = has_else || exhaustive_result.is_some();
        let keeps_value = discarded && keeps_discarded_value(exhaustive, result_ty);
        let is_stmt = is_stmt && !keeps_value;
        let terminal_target = is_stmt.then_some(enclosing_terminal_target).flatten();
        // A `when` comparing ONE Int, Char or String local against constants is a JVM switch in kotlinc, not a chain
        // of comparisons. Everything above (the result type, the statement/value decision, the entry
        // height) applies unchanged; only the dispatch differs.
        if let Some(mut plan) = self.switch_plan(branches) {
            let (is_stmt, result_ty, keeps_value, terminal_target) =
                if discarded && exhaustive && result_ty == Ty::Unit {
                    (false, Ty::obj("kotlin/Unit"), !keeps_unit, None)
                } else {
                    (is_stmt, result_ty, keeps_value, terminal_target)
                };
            if !has_else && exhaustive_result.is_some() {
                plan.no_branch_matched = Some(self.ir.whens.source_lines.get(&expression).copied());
            }
            self.emit_switch(
                &plan,
                Emission::new(is_stmt, result_ty, entry_height, end, terminal_target),
                code,
            );
            if terminal_target.is_none() {
                land_joined_value(is_stmt, entry_height, result_ty, code);
            }
            discard_joined_value(keeps_value, result_ty, code);
            return;
        }
        // kotlinc's `visitWhen` marks a source `when`'s own line and writes a `nop` on it before
        // the first branch, so a debugger stops on the `when` line. The nop survives only where a
        // debug point needs it, as every nop does.
        // Every other `when`, an `if` included, still marks its own line there: a condition on a
        // later line marks that one at the same offset, and both entries stay.
        if let Some(&line) = self.ir.whens.source_lines.get(&expression) {
            code.mark_line(line);
            code.nop();
        } else {
            self.mark_kept_start_line(expression, code);
        }
        for (index, (cond, body)) in branches.iter().enumerate() {
            match cond {
                Some(c) => {
                    // Skip to the next branch when this condition is false (fused comparison branch).
                    let next = code.new_label();
                    // A constant-false condition emits `goto next`; do not lay down its unreachable,
                    // unframed body. Suspend flattening produces this shape for some do-while loops.
                    if self.emit_when_condition(*c, next, false, code) {
                        self.bind(next, code);
                        code.set_stack(entry_height);
                        continue;
                    }
                    if is_stmt {
                        // Statement emission handles both physical-void operations and explicit
                        // `Unit.INSTANCE`; semantic `Ty::Unit` alone cannot distinguish them.
                        self.emit(*body, code);
                    } else {
                        self.emit_value(*body, code);
                        self.adapt_physical_operand_for(
                            *body,
                            self.value_ty(*body),
                            result_ty,
                            code,
                        );
                    }
                    let body_diverges = if is_stmt {
                        self.discarding_diverges(*body)
                    } else {
                        self.diverges(*body)
                    };
                    if !body_diverges {
                        // Fall through only across empty ELSE branches. An empty conditional branch
                        // still evaluates its condition, which the selected arm must skip.
                        let nothing_follows = branches[index + 1..]
                            .iter()
                            .all(|(condition, rest)| {
                                condition.is_none()
                                    && matches!(self.ir.expr(*rest), IrExpr::Block { stmts, value } if stmts.is_empty() && value.is_none())
                            })
                            && (has_else || exhaustive_result.is_none());
                        let falls_into_end = is_stmt && nothing_follows;
                        if !falls_into_end {
                            code.goto(end);
                        }
                    }
                    self.bind(next, code);
                    // `next` is reached only via the conditional jump above, where the stack is back at the
                    // pre-branch baseline — reset the linear counter (the just-emitted branch body left its
                    // value on the counter, but not on this control path).
                    code.set_stack(entry_height);
                }
                None => {
                    if is_stmt {
                        self.emit(*body, code);
                    } else {
                        self.emit_value(*body, code);
                        self.adapt_physical_operand_for(
                            *body,
                            self.value_ty(*body),
                            result_ty,
                            code,
                        );
                    }
                    // The else is last — it falls through to `end` (no goto needed).
                }
            }
        }
        if !has_else && exhaustive_result.is_some() {
            // fir2ir builds the implicit `else -> throw NoWhenBranchMatchedException()` at the
            // `when`'s own offsets, and kotlinc's `visitThrow` marks that line.
            if let Some(&line) = self.ir.whens.source_lines.get(&expression) {
                code.mark_line(line);
            }
            self.emit_no_when_branch_matched(code);
        }
        self.bind(end, code);
        land_joined_value(is_stmt, entry_height, result_ty, code);
        discard_joined_value(keeps_value, result_ty, code);
    }
}

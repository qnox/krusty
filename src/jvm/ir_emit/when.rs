//! `when` expressions (and every `if` the frontend lowers to one): type joins, JVM integer
//! switches, and the chain of conditional branches.

use super::*;

/// Whether a discarded `when` still joins a value, as kotlinc's `visitWhen` has it: only a `when`
/// that is not exhaustive, or whose type is `Unit`, discards each branch's value in the branch.
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

/// A switch subject, constant cases in source order, and the optional final `else` body.
pub(super) struct IntSwitchPlan {
    subject: u32,
    cases: Vec<(i32, u32)>,
    default: Option<u32>,
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
                match *ty {
                    Ty::String => Some("java/lang/String".to_string()),
                    _ if ty.is_array() => Some(type_descriptor(*ty)),
                    Ty::Obj(name, _) => Some(name.to_string()),
                    _ => None,
                }
            };
            let mut names = branches
                .iter()
                .filter(|(_, body)| !self.diverges(*body))
                .map(|(_, body)| self.value_ty(*body))
                .filter(|ty| !matches!(ty, Ty::Null | Ty::Nothing | Ty::Error))
                .filter_map(|ty| internal(&ty));
            if let Some(first) = names.next() {
                if names.any(|name| name != first) {
                    return Ty::obj("kotlin/Any");
                }
            }
        }
        last
    }

    /// Recognize the shape kotlinc switches: every branch compares the same `Int` local against a
    /// distinct constant, and an `else`, if present, is last. One key remains a comparison.
    pub(super) fn int_switch_plan(&self, branches: &[(Option<u32>, u32)]) -> Option<IntSwitchPlan> {
        let mut subject = None;
        let mut cases: Vec<(i32, u32)> = Vec::new();
        let mut default = None;
        for (index, (cond, body)) in branches.iter().enumerate() {
            let Some(cond) = cond else {
                if index + 1 != branches.len() {
                    return None;
                }
                default = Some(*body);
                continue;
            };
            let IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs,
                rhs,
            } = self.ir.expr(*cond)
            else {
                return None;
            };
            let IrExpr::Const(IrConst::Int(key)) = self.ir.expr(*rhs) else {
                return None;
            };
            let IrExpr::GetValue(variable) = self.ir.expr(*lhs) else {
                return None;
            };
            if self.value_ty(*lhs) != Ty::Int {
                return None;
            }
            match subject {
                None => subject = Some((*variable, *lhs)),
                Some((already, _)) if already == *variable => {}
                Some(_) => return None,
            }
            if cases.iter().any(|(seen, _)| seen == key) {
                return None;
            }
            cases.push((*key, *body));
        }
        let (_, subject) = subject?;
        (cases.len() >= 2).then_some(IntSwitchPlan {
            subject,
            cases,
            default,
        })
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
        let emit_arm = |emitter: &mut Self, arm: u32, code: &mut CodeBuilder| {
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
                if !matches!(
                    emitter.ir.expr(arm),
                    IrExpr::Const(_) | IrExpr::UnitInstance
                ) {
                    emitter.emit(arm, code);
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
        let selector_diverges = always_null || emit_arm(self, selector, code);
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
        let _ = emit_arm(self, null_result, code);
        self.bind(emission.end, code);
    }

    /// Emit case bodies in source order with `else` last, using kotlinc's switch-density rule.
    pub(super) fn emit_int_switch(
        &mut self,
        plan: &IntSwitchPlan,
        emission: Emission,
        code: &mut CodeBuilder,
    ) {
        let default = code.new_label();
        let case_labels: Vec<Label> = plan.cases.iter().map(|_| code.new_label()).collect();
        // kotlinc's `SwitchGenerator` loads the `when`'s subject once, not a condition's read of
        // it, so the load carries none of the conditions' source lines.
        let subject = self.ir.expr(plan.subject).clone();
        self.emit_value_node(plan.subject, &subject, code);
        let low = plan.cases.iter().map(|(key, _)| *key).min().expect("keys");
        let high = plan.cases.iter().map(|(key, _)| *key).max().expect("keys");
        let span = i64::from(high) - i64::from(low) + 1;
        for &label in case_labels.iter().chain(std::iter::once(&default)) {
            self.record_label_assignment_state(label, code);
        }
        if span <= 2 * plan.cases.len() as i64 {
            let mut targets = vec![default; span as usize];
            for (index, (key, _)) in plan.cases.iter().enumerate() {
                targets[(i64::from(*key) - i64::from(low)) as usize] = case_labels[index];
            }
            code.tableswitch(low, high, default, &targets);
        } else {
            let mut pairs: Vec<(i32, Label)> = plan
                .cases
                .iter()
                .enumerate()
                .map(|(index, (key, _))| (*key, case_labels[index]))
                .collect();
            pairs.sort_by_key(|(key, _)| *key);
            code.lookupswitch(default, &pairs);
        }

        let merge = emission.terminal_target.unwrap_or(emission.end);
        for (index, (_, body)) in plan.cases.iter().enumerate() {
            self.bind(case_labels[index], code);
            code.set_stack(emission.entry_height);
            if self.emit_switch_body(*body, &emission, code) {
                code.goto(merge);
            }
        }
        self.bind(default, code);
        code.set_stack(emission.entry_height);
        match plan.default {
            Some(body) => {
                if self.emit_switch_body(body, &emission, code)
                    && emission.terminal_target.is_some()
                {
                    code.goto(merge);
                }
            }
            None => {
                if emission.terminal_target.is_some() {
                    code.goto(merge);
                }
            }
        }
        if emission.terminal_target.is_none() {
            self.bind(emission.end, code);
        }
    }

    /// Emit one case body and report whether it reaches the merge.
    fn emit_switch_body(&mut self, body: u32, emission: &Emission, code: &mut CodeBuilder) -> bool {
        if emission.is_stmt {
            self.emit(body, code);
            !self.discarding_diverges(body)
        } else {
            self.emit_value(body, code);
            self.adapt_physical_operand_for(body, self.value_ty(body), emission.result_ty, code);
            !self.diverges(body)
        }
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
        // A `when` comparing ONE Int local against constants is a JVM switch in kotlinc, not a chain
        // of comparisons. Everything above (the result type, the statement/value decision, the entry
        // height) applies unchanged; only the dispatch differs.
        if let Some(plan) = self.int_switch_plan(branches) {
            self.emit_int_switch(
                &plan,
                Emission::new(is_stmt, result_ty, entry_height, end, terminal_target),
                code,
            );
            discard_joined_value(keeps_value, result_ty, code);
            return;
        }
        // kotlinc's `visitWhen` marks a source `when`'s own line and writes a `nop` on it before
        // the first branch, so a debugger stops on the `when` line. The nop survives only where a
        // debug point needs it, as every nop does.
        if let Some(&line) = self.ir.whens.source_lines.get(&expression) {
            code.mark_line(line);
            code.nop();
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
            self.emit_no_when_branch_matched(code);
        }
        self.bind(end, code);
        discard_joined_value(keeps_value, result_ty, code);
    }
}

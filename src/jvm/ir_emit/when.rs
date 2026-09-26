//! Type joins and JVM integer-switch emission for `when` expressions.

use super::{
    discard, type_descriptor, CodeBuilder, Emitter, IrBinOp, IrConst, IrExpr, IrTypeOp, Label, Ty,
};

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
        self.emit_value(plan.subject, code);
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

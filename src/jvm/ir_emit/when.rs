//! Type joins and JVM integer-switch emission for `when` expressions.

use super::{
    type_descriptor, CodeBuilder, Emitter, IrBinOp, IrConst, IrExpr, Label, Ty, VerifType,
};

/// A switch subject, constant cases in source order, and the optional final `else` body.
pub(super) struct IntSwitchPlan {
    subject: u32,
    cases: Vec<(i32, u32)>,
    default: Option<u32>,
}

/// The merge contract shared by every emitted case body.
pub(super) struct Emission<'a> {
    is_stmt: bool,
    result_ty: Ty,
    result_stack: &'a [VerifType],
    entry_height: u16,
    end: Label,
}

impl<'a> Emission<'a> {
    pub(super) fn new(
        is_stmt: bool,
        result_ty: Ty,
        result_stack: &'a [VerifType],
        entry_height: u16,
        end: Label,
    ) -> Self {
        Self {
            is_stmt,
            result_ty,
            result_stack,
            entry_height,
            end,
        }
    }
}

impl Emitter<'_> {
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

    /// Emit case bodies in source order with `else` last, using kotlinc's switch-density rule.
    pub(super) fn emit_int_switch(
        &mut self,
        plan: &IntSwitchPlan,
        emission: Emission<'_>,
        code: &mut CodeBuilder,
    ) {
        let default = code.new_label();
        let case_labels: Vec<Label> = plan.cases.iter().map(|_| code.new_label()).collect();
        self.emit_value(plan.subject, code);
        let low = plan.cases.iter().map(|(key, _)| *key).min().expect("keys");
        let high = plan.cases.iter().map(|(key, _)| *key).max().expect("keys");
        let span = i64::from(high) - i64::from(low) + 1;
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

        let mut end_reachable = false;
        for (index, (_, body)) in plan.cases.iter().enumerate() {
            self.frame(case_labels[index], vec![], code);
            code.bind(case_labels[index]);
            code.set_stack(emission.entry_height);
            if self.emit_switch_body(*body, &emission, code) {
                self.frame(emission.end, emission.result_stack.to_vec(), code);
                code.goto(emission.end);
                end_reachable = true;
            }
        }
        self.frame(default, vec![], code);
        code.bind(default);
        code.set_stack(emission.entry_height);
        match plan.default {
            Some(body) => {
                if self.emit_switch_body(body, &emission, code) {
                    end_reachable = true;
                }
            }
            None => end_reachable = true,
        }
        if end_reachable {
            self.frame(emission.end, emission.result_stack.to_vec(), code);
        }
        code.bind(emission.end);
    }

    /// Emit one case body and report whether it reaches the merge.
    fn emit_switch_body(
        &mut self,
        body: u32,
        emission: &Emission<'_>,
        code: &mut CodeBuilder,
    ) -> bool {
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

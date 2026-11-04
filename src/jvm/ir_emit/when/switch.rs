//! kotlinc's `SwitchGenerator`: a `when` whose every clause compares one local against a constant
//! dispatches through a JVM switch instead of a chain of comparisons. An `Int` or `Char` local
//! switches on its value; a `String` local switches on its `hashCode()` and resolves each hash
//! bucket with `equals`.

use super::*;
use crate::kt_string::KtString;

/// A switch subject, its dispatch, the bodies of the branches a case still reaches, and the
/// optional final `else` body.
pub(in crate::jvm::ir_emit) struct SwitchPlan {
    subject: u32,
    dispatch: Dispatch,
    bodies: Vec<u32>,
    default: Option<u32>,
    /// An exhaustive `when` without `else` gets fir2ir's `else -> throw
    /// NoWhenBranchMatchedException()`, at the `when`'s line when it has one.
    pub(super) no_branch_matched: Option<Option<u32>>,
}

enum Dispatch {
    /// Each distinct key, in source order, with the branch it selects.
    Int(Vec<(i32, usize)>),
    String(StringDispatch),
}

/// `StringSwitch`: the hash buckets in the iteration order of kotlinc's `HashMap<Integer, …>`, each
/// holding its strings in source order as the constant to load and the branch it selects.
struct StringDispatch {
    nullable: bool,
    null_branch: Option<usize>,
    buckets: Vec<(i32, Vec<(u32, usize)>)>,
}

/// One clause's constant.
#[derive(PartialEq)]
enum Key {
    Int(i32),
    String(KtString),
    Null,
}

/// The kind of local a switch can dispatch on.
#[derive(Clone, Copy, PartialEq)]
enum SubjectKind {
    Int,
    Char,
    String,
}

impl Emitter<'_> {
    /// Every clause of every branch (`a, b ->` is an `||` chain of clauses) compares the same local
    /// against a constant, and an `else`, if present, is last. A key repeated later is unreachable
    /// and dropped, and so is a branch left without a key. kotlinc switches on two or more `Int` or
    /// `Char` keys, and on three or more string hash buckets; fewer remain comparisons.
    pub(in crate::jvm::ir_emit) fn switch_plan(
        &self,
        branches: &[(Option<u32>, u32)],
    ) -> Option<SwitchPlan> {
        let mut subject = None;
        let mut cases: Vec<(Key, u32, usize)> = Vec::new();
        let mut bodies = Vec::new();
        let mut default = None;
        for (index, (cond, body)) in branches.iter().enumerate() {
            let Some(cond) = cond else {
                if index + 1 != branches.len() {
                    return None;
                }
                default = Some(*body);
                continue;
            };
            let mut clauses = Vec::new();
            self.switch_clauses(*cond, &mut clauses);
            let branch = bodies.len();
            let mut reachable = false;
            for clause in clauses {
                let (variable, read, constant) = self.switch_clause(clause)?;
                match subject {
                    None => subject = Some((variable, read)),
                    Some((already, _)) if already == variable => {}
                    Some(_) => return None,
                }
                let key = self.switch_key(constant)?;
                if cases.iter().all(|(seen, _, _)| *seen != key) {
                    cases.push((key, constant, branch));
                    reachable = true;
                }
            }
            if reachable {
                bodies.push(*body);
            }
        }
        let (_, subject) = subject?;
        // The local's physical type selects the switch; a `String` local's checked type says
        // whether it may be `null`, and a platform `String!` may, as in kotlinc's IR.
        let kind = match self.value_ty(subject) {
            Ty::Int => SubjectKind::Int,
            Ty::Char => SubjectKind::Char,
            Ty::String => SubjectKind::String,
            _ => return None,
        };
        let dispatch = match kind {
            SubjectKind::Int | SubjectKind::Char => Dispatch::Int(
                cases
                    .into_iter()
                    .map(|(key, constant, branch)| match key {
                        Key::Int(key) if self.int_key_kind(constant) == Some(kind) => {
                            Some((key, branch))
                        }
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()?,
            ),
            SubjectKind::String => Dispatch::String(StringDispatch::new(
                self.ir.logical_types.get(&subject)?.admits_null(),
                cases,
            )?),
        };
        let enough = match &dispatch {
            Dispatch::Int(cases) => cases.len() > 1,
            Dispatch::String(strings) => strings.buckets.len() > 2,
        };
        enough.then_some(SwitchPlan {
            subject,
            dispatch,
            bodies,
            default,
            no_branch_matched: None,
        })
    }

    /// The clauses of a branch condition: the leaves of an `||` chain, or the condition itself.
    fn switch_clauses(&self, condition: u32, clauses: &mut Vec<u32>) {
        if self.ir.short_circuits.get(&condition) == Some(&crate::ir::IrShortCircuitKind::Or) {
            if let IrExpr::When { branches } = self.ir.expr(condition) {
                if let [(Some(first), _), (None, rest)] = branches.as_slice() {
                    self.switch_clauses(*first, clauses);
                    self.switch_clauses(*rest, clauses);
                    return;
                }
            }
        }
        clauses.push(condition);
    }

    /// `subject == constant` (`EQEQ`) with a local subject: the local, its read, and the constant.
    fn switch_clause(&self, clause: u32) -> Option<(u32, u32, u32)> {
        let (lhs, rhs) = match self.ir.expr(clause) {
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs,
                rhs,
            } => (*lhs, *rhs),
            IrExpr::Equality {
                op: IrBinOp::Eq,
                mode: crate::ir::EqualityMode::Primitive | crate::ir::EqualityMode::Structural,
                lhs,
                rhs,
            } => (*lhs, *rhs),
            _ => return None,
        };
        let IrExpr::GetValue(variable) = self.ir.expr(lhs) else {
            return None;
        };
        Some((*variable, lhs, rhs))
    }

    fn switch_key(&self, constant: u32) -> Option<Key> {
        Some(match self.ir.expr(constant) {
            IrExpr::Const(IrConst::Int(key)) => Key::Int(*key),
            IrExpr::Const(IrConst::Char(key)) => Key::Int(i32::from(*key)),
            IrExpr::Const(IrConst::String(key)) => Key::String(key.clone()),
            IrExpr::Const(IrConst::Null) => Key::Null,
            IrExpr::EnumSwitchKey { classifier, name } => {
                Key::Int(self.enum_switch_key(*classifier, name))
            }
            _ => return None,
        })
    }

    /// Which subject an integral constant can be a key of.
    fn int_key_kind(&self, constant: u32) -> Option<SubjectKind> {
        match self.ir.expr(constant) {
            IrExpr::Const(IrConst::Int(_)) | IrExpr::EnumSwitchKey { .. } => Some(SubjectKind::Int),
            IrExpr::Const(IrConst::Char(_)) => Some(SubjectKind::Char),
            _ => None,
        }
    }

    /// Emit the dispatch, then the reachable branch bodies in source order with `else` last.
    pub(in crate::jvm::ir_emit) fn emit_switch(
        &mut self,
        plan: &SwitchPlan,
        emission: Emission,
        code: &mut CodeBuilder,
    ) {
        let default = code.new_label();
        let branch_labels: Vec<Label> = plan.bodies.iter().map(|_| code.new_label()).collect();
        for &label in branch_labels.iter().chain(std::iter::once(&default)) {
            self.record_label_assignment_state(label, code);
        }
        match &plan.dispatch {
            Dispatch::Int(cases) => {
                // kotlinc loads the `when`'s subject once, not a condition's read of it, so the
                // load carries none of the conditions' source lines.
                self.emit_switch_subject(plan.subject, code);
                let cases: Vec<(i32, Label)> = cases
                    .iter()
                    .map(|(key, branch)| (*key, branch_labels[*branch]))
                    .collect();
                emit_int_switch(&cases, default, code);
            }
            Dispatch::String(strings) => {
                self.emit_string_dispatch(plan.subject, strings, &branch_labels, default, code)
            }
        }

        let merge = emission.terminal_target.unwrap_or(emission.end);
        for (label, body) in branch_labels.iter().zip(&plan.bodies) {
            self.bind(*label, code);
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
            None => match plan.no_branch_matched {
                Some(line) => {
                    if let Some(line) = line {
                        code.mark_line(line);
                    }
                    self.emit_no_when_branch_matched(code);
                }
                None => {
                    if emission.terminal_target.is_some() {
                        code.goto(merge);
                    }
                }
            },
        }
        if emission.terminal_target.is_none() {
            self.bind(emission.end, code);
        }
    }

    /// Load a subject read or a key constant without the source line its node carries.
    fn emit_switch_subject(&mut self, operand: u32, code: &mut CodeBuilder) {
        let node = self.ir.expr(operand).clone();
        self.emit_value_node(operand, &node, code);
    }

    /// `StringSwitch.genSwitch`: a `null` subject goes to the `null` case or the default, any
    /// other switches on its hash, and each bucket tests its strings with `equals` in source
    /// order before falling to the default.
    fn emit_string_dispatch(
        &mut self,
        subject: u32,
        strings: &StringDispatch,
        branch_labels: &[Label],
        default: Label,
        code: &mut CodeBuilder,
    ) {
        if strings.nullable {
            self.emit_switch_subject(subject, code);
            code.ifnull(
                strings
                    .null_branch
                    .map_or(default, |branch| branch_labels[branch]),
            );
        }
        self.emit_switch_subject(subject, code);
        let hash_code = self.cw.methodref("java/lang/String", "hashCode", "()I");
        code.invokevirtual(hash_code, 0, 1);
        let bucket_labels: Vec<Label> = strings.buckets.iter().map(|_| code.new_label()).collect();
        for &label in &bucket_labels {
            self.record_label_assignment_state(label, code);
        }
        let cases: Vec<(i32, Label)> = strings
            .buckets
            .iter()
            .zip(&bucket_labels)
            .map(|((hash, _), label)| (*hash, *label))
            .collect();
        emit_int_switch(&cases, default, code);
        for ((_, entries), label) in strings.buckets.iter().zip(bucket_labels) {
            self.bind(label, code);
            let height = code.stack_height().max(0) as u16;
            for &(constant, branch) in entries {
                // Neither read carries a source line: the bucket belongs to the `when`'s line.
                self.emit_switch_subject(subject, code);
                self.emit_switch_subject(constant, code);
                let equals =
                    self.cw
                        .methodref("java/lang/String", "equals", "(Ljava/lang/Object;)Z");
                code.invokevirtual(equals, 1, 1);
                code.ifne(branch_labels[branch]);
            }
            code.goto(default);
            code.set_stack(height);
        }
    }

    /// Emit one case body and report whether it reaches the merge.
    fn emit_switch_body(&mut self, body: u32, emission: &Emission, code: &mut CodeBuilder) -> bool {
        if emission.is_stmt {
            self.emit(body, code);
            !self.discarding_diverges(body)
        } else if matches!(self.ir.expr(body), IrExpr::Block { stmts, value } if stmts.is_empty() && value.is_none())
        {
            // An empty arm of a `Unit` switch has no statement to carry a line: kotlinc
            // materializes its `Unit` on the line before it.
            let unit = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
            code.getstatic(unit, 1);
            true
        } else if emission.result_ty == Ty::obj("kotlin/Unit") && !self.yields_unit(body) {
            // A `when` ending in an open `else -> if` is `Unit` without changing its other arms'
            // types: kotlinc discards such an arm's value and materializes the join's `Unit`.
            self.emit(body, code);
            let completes = !self.discarding_diverges(body);
            if completes {
                let unit = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                code.getstatic(unit, 1);
            }
            completes
        } else {
            self.emit_value(body, code);
            self.adapt_physical_operand_for(body, self.value_ty(body), emission.result_ty, code);
            !self.diverges(body)
        }
    }

    /// Whether `body`'s own value is `Unit` (or never completes), so it materializes the `Unit` a
    /// switch arm yields itself.
    fn yields_unit(&self, body: u32) -> bool {
        let ty = self.value_ty(body);
        ty == Ty::Unit || ty == Ty::obj("kotlin/Unit") || ty == Ty::Nothing
    }
}

impl StringDispatch {
    fn new(nullable: bool, cases: Vec<(Key, u32, usize)>) -> Option<Self> {
        let mut null_branch = None;
        let mut buckets: Vec<(i32, Vec<(u32, usize)>)> = Vec::new();
        for (key, constant, branch) in cases {
            match key {
                // `null` never reaches the hash switch.
                Key::Null => null_branch = Some(branch),
                Key::String(string) => {
                    let hash = java_string_hash(&string);
                    match buckets.iter_mut().find(|(seen, _)| *seen == hash) {
                        Some((_, entries)) => entries.push((constant, branch)),
                        None => buckets.push((hash, vec![(constant, branch)])),
                    }
                }
                Key::Int(_) => return None,
            }
        }
        java_hash_map_order(&mut buckets);
        Some(Self {
            nullable,
            null_branch,
            buckets,
        })
    }
}

/// `tableswitch` when the key range is at most twice the number of keys, else `lookupswitch`.
fn emit_int_switch(cases: &[(i32, Label)], default: Label, code: &mut CodeBuilder) {
    let low = cases.iter().map(|(key, _)| *key).min().expect("keys");
    let high = cases.iter().map(|(key, _)| *key).max().expect("keys");
    let span = i64::from(high) - i64::from(low) + 1;
    if span <= 2 * cases.len() as i64 {
        let mut targets = vec![default; span as usize];
        for (key, label) in cases {
            targets[(i64::from(*key) - i64::from(low)) as usize] = *label;
        }
        code.tableswitch(low, high, default, &targets);
    } else {
        let mut pairs = cases.to_vec();
        pairs.sort_by_key(|(key, _)| *key);
        code.lookupswitch(default, &pairs);
    }
}

/// `java.lang.String.hashCode()`: `s[0]*31^(n-1) + … + s[n-1]` over UTF-16 units, wrapping.
fn java_string_hash(string: &KtString) -> i32 {
    string.units().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

/// Reorder hash buckets, given in insertion order, as kotlinc's `HashMap<Integer, …>` iterates
/// them.
fn java_hash_map_order<T>(buckets: &mut Vec<(i32, T)>) {
    let keys: Vec<i32> = buckets.iter().map(|(hash, _)| *hash).collect();
    let mut slots: Vec<Option<(i32, T)>> = std::mem::take(buckets).into_iter().map(Some).collect();
    *buckets = super::java_hash_map::iteration_order(&keys)
        .into_iter()
        .map(|index| slots[index].take().expect("each key iterates once"))
        .collect();
}

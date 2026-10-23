//! Definite assignment of deferred locals: `val x: Int` or `var x: Int` declared without an
//! initializer and assigned later.
//!
//! kotlinc's variable-initialization analysis runs over the control-flow graph of the whole body.
//! For each deferred local it tracks whether the local is definitely and whether it may already be
//! initialized:
//! - a read where the local is not definitely initialized is `variable 'x' must be initialized.`;
//! - a write to a `val` that may already be initialized is `'val' cannot be reassigned.`;
//! - a write to an enclosing `val` from a lambda or function that is not called in place is
//!   `captured values cannot be initialized because of possible reassignments.`
//!
//! Local and anonymous class bodies read and write their captures through class storage, which the
//! analysis does not follow yet: their reads and writes of enclosing deferred locals are not checked.
//!
//! A lambda argument runs in place when the selected callee's contract declares `callsInPlace` for
//! it, or when the callee inlines it (an inline parameter without a contract runs in place an
//! unknown number of times). Its invocation kind decides the flow around it: `EXACTLY_ONCE` runs
//! the body once, `AT_MOST_ONCE` may skip it, `AT_LEAST_ONCE` repeats it, and `UNKNOWN` may skip
//! or repeat it. A repeated body and a loop body are analyzed until the state at their entry is
//! stable; only the final pass reports.
//!
//! Like kotlinc, an edge that cannot be taken (after `return`, `throw`, `break`, an expression of
//! type `Nothing`, the exit of `while (true)`, the back edge of `do … while (false)`) still carries its
//! state: a join ignores dead inputs when a live one exists, and code reached only through dead
//! edges is analyzed with their state.

use std::collections::BTreeSet;

use super::lambda_returns::ReturnTarget;
use super::scope::PathRoot;
use super::{Checker, CheckerScope, Local, ReceiverFnValueOrigin};
use crate::ast::{BinOp, Decl, Expr, ExprId, Stmt, StmtId};
use crate::contracts::{Effect, InvocationKind};
use crate::diag::Span;
use crate::fx_hash::{HashMap, HashSet};
use crate::types::Ty;

/// The checker records the analysis inputs that are not in the AST: which statements declare deferred
/// locals and which statements write a local, both by the flow identity of the local.
#[derive(Default)]
pub(super) struct InitializationRecords {
    declarations: HashMap<StmtId, u32>,
    writes: HashMap<StmtId, u32>,
}

impl InitializationRecords {
    pub(super) fn declare_deferred(&mut self, statement: StmtId, flow_identity: u32) {
        self.declarations.insert(statement, flow_identity);
    }

    /// Records that `statement` (an assignment or a statement-position `++`/`--`) writes the
    /// lexical local with `flow_identity`.
    fn write(&mut self, statement: StmtId, flow_identity: u32) {
        self.writes.insert(statement, flow_identity);
    }
}

/// What is known about the deferred locals at one point of the body.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Flow {
    /// False on a path that cannot be taken.
    live: bool,
    definite: BTreeSet<u32>,
    possible: BTreeSet<u32>,
}

impl Flow {
    fn dead(mut self) -> Self {
        self.live = false;
        self
    }

    fn dead_if(self, dead: bool) -> Self {
        if dead {
            self.dead()
        } else {
            self
        }
    }

    fn join(self, other: Self) -> Self {
        match (self.live, other.live) {
            (true, false) => self,
            (false, true) => other,
            _ => Self {
                live: self.live,
                definite: self
                    .definite
                    .intersection(&other.definite)
                    .copied()
                    .collect(),
                possible: self.possible.union(&other.possible).copied().collect(),
            },
        }
    }
}

fn join(first: Option<Flow>, second: Option<Flow>) -> Option<Flow> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.join(second)),
        (first, second) => first.or(second),
    }
}

struct Tracked {
    name: String,
    is_val: bool,
    /// The detached body (lambda or function not called in place) the local is declared in.
    frame: usize,
}

struct LoopFrame {
    label: Option<String>,
    breaks: Option<Flow>,
    continues: Option<Flow>,
}

struct LambdaFrame {
    lambda: ExprId,
    exits: Option<Flow>,
}

struct Analysis<'c, 'a> {
    checker: &'c Checker<'a>,
    flow: Flow,
    tracked: HashMap<u32, Tracked>,
    frame: usize,
    loops: Vec<LoopFrame>,
    lambdas: Vec<LambdaFrame>,
    /// Locals written inside each enclosing `try` body (or its catches, for a `finally`), which
    /// an exception may interrupt anywhere.
    try_writes: Vec<BTreeSet<u32>>,
    report: bool,
    reported: HashSet<(Span, String)>,
    diagnostics: Vec<(Span, String)>,
}

impl Checker<'_> {
    /// Records that `statement` writes the lexical local `target` resolved to, when it is one.
    pub(super) fn record_local_write(&mut self, statement: StmtId, target: Option<Local>) {
        self.record_class_storage_write(statement, target);
        if let Some(local) = target.filter(|local| local.origin == ReceiverFnValueOrigin::Local) {
            self.local_initialization
                .write(statement, local.flow_identity);
        }
    }

    /// Type-check a block-shaped root not owned by a [`crate::ast::FunDecl`] (an accessor,
    /// constructor, or `init` block), then complete all body-check phases for that root.
    pub(super) fn check_executable_body(&mut self, scope: &CheckerScope<'_>, body: ExprId) {
        let _ = self.expr_statement(scope, body);
        self.finish_executable_root(body);
    }

    /// Type-check an `init` block, or a script statement, which kotlinc compiles to one. Neither has
    /// a function to return from, so a `return` in it is prohibited.
    pub(super) fn check_initializer_block(&mut self, scope: &CheckerScope<'_>, body: ExprId) {
        self.with_ret_allowed(Ty::Unit, false, |checker| {
            checker.check_executable_body(scope, body);
        });
    }

    /// Completes the body-check phase for one independently evaluated expression root.
    ///
    /// Type checking records resolved local reads, declarations, writes, and calls-in-place facts;
    /// definite assignment consumes those facts only after the whole root has been checked. Keep
    /// that phase boundary shared by function/accessor bodies, declaration initializers, delegates,
    /// and parameter defaults so adding a new kind of executable declaration root cannot silently
    /// leave it with types but without control-flow diagnostics.
    pub(super) fn finish_executable_root(&mut self, body: ExprId) {
        // An ordinary signature-only traversal recreates lexical scope for a selected nested
        // default and must not diagnose the enclosing expression it temporarily walks. The default
        // itself is a complete executable root while its depth is nonzero and is checked here.
        if self.signature_defaults_only && self.signature_default_expression_depth == 0 {
            return;
        }
        if self.local_initialization.declarations.is_empty() {
            return;
        }
        let mut analysis = Analysis {
            checker: self,
            flow: Flow {
                live: true,
                definite: BTreeSet::new(),
                possible: BTreeSet::new(),
            },
            tracked: HashMap::default(),
            frame: 0,
            loops: Vec::new(),
            lambdas: Vec::new(),
            try_writes: Vec::new(),
            report: true,
            reported: HashSet::default(),
            diagnostics: Vec::new(),
        };
        analysis.expr(body);
        let diagnostics = analysis.diagnostics;
        for (span, message) in diagnostics {
            self.diags.error(span, message);
        }
    }
}

impl Analysis<'_, '_> {
    fn emit(&mut self, span: Span, message: String) {
        if self.report && self.reported.insert((span, message.clone())) {
            self.diagnostics.push((span, message));
        }
    }

    fn read(&mut self, expression: ExprId) {
        let Some(PathRoot::Value(identity)) = self.checker.read_flow_roots.get(&expression) else {
            return;
        };
        self.read_local(*identity, self.checker.span(expression));
    }

    fn read_local(&mut self, identity: u32, span: Span) {
        let Some(local) = self.tracked.get(&identity) else {
            return;
        };
        if !self.flow.definite.contains(&identity) {
            let message = format!("variable '{}' must be initialized.", local.name);
            self.emit(span, message);
        }
    }

    fn write(&mut self, identity: u32, span: Span) {
        let Some(local) = self.tracked.get(&identity) else {
            return;
        };
        if local.is_val {
            if local.frame < self.frame {
                self.emit(
                    span,
                    "captured values cannot be initialized because of possible reassignments."
                        .to_string(),
                );
            } else if self.flow.possible.contains(&identity) {
                self.emit(span, "'val' cannot be reassigned.".to_string());
            }
        }
        self.flow.definite.insert(identity);
        self.flow.possible.insert(identity);
        for writes in &mut self.try_writes {
            writes.insert(identity);
        }
    }

    /// Runs `pass` from `entry` until the state it feeds back to its own entry is stable, then
    /// once more with reporting on. `pass` returns its result and the state of its back edge.
    fn repeat(
        &mut self,
        entry: Flow,
        mut pass: impl FnMut(&mut Self, Flow) -> (Flow, Option<Flow>),
    ) -> Flow {
        let report = std::mem::replace(&mut self.report, false);
        let mut input = entry.clone();
        loop {
            let (_, back) = pass(self, input.clone());
            let next = join(Some(entry.clone()), back).expect("the entry is a path");
            if next == input {
                break;
            }
            input = next;
        }
        self.report = report;
        pass(self, input).0
    }

    /// Analyzes a body that runs at an unknown later time (a lambda or function not called in
    /// place): it sees the state where it is declared, and leaves it as is.
    fn detached(&mut self, body: impl FnOnce(&mut Self)) {
        let flow = self.flow.clone();
        let loops = std::mem::take(&mut self.loops);
        let lambdas = std::mem::take(&mut self.lambdas);
        let try_writes = std::mem::take(&mut self.try_writes);
        self.frame += 1;
        body(self);
        self.frame -= 1;
        self.try_writes = try_writes;
        self.lambdas = lambdas;
        self.loops = loops;
        self.flow = flow;
    }

    fn detached_lambda(&mut self, lambda: ExprId) {
        self.detached(|analysis| {
            analysis.lambda_body(lambda, analysis.flow.clone());
        });
    }

    /// One run of a lambda's body from `input`; the result joins its normal end with every
    /// `return@label` that leaves it.
    fn lambda_body(&mut self, lambda: ExprId, input: Flow) -> Flow {
        let Expr::Lambda { body, .. } = self.checker.file.expr(lambda) else {
            return input;
        };
        self.flow = input;
        self.lambdas.push(LambdaFrame {
            lambda,
            exits: None,
        });
        self.expr(*body);
        let frame = self.lambdas.pop().expect("pushed above");
        join(Some(self.flow.clone()), frame.exits).expect("the end is a path")
    }

    fn in_place_lambda(&mut self, lambda: ExprId, kind: InvocationKind) {
        let entry = self.flow.clone();
        self.flow = match kind {
            InvocationKind::ExactlyOnce => self.lambda_body(lambda, entry),
            InvocationKind::AtMostOnce => {
                let ran = self.lambda_body(lambda, entry.clone());
                entry.join(ran)
            }
            InvocationKind::AtLeastOnce => self.repeat(entry, |analysis, input| {
                let ran = analysis.lambda_body(lambda, input);
                (ran.clone(), Some(ran))
            }),
            InvocationKind::Unknown => self.repeat(entry, |analysis, input| {
                let ran = analysis.lambda_body(lambda, input.clone());
                (input.join(ran.clone()), Some(ran))
            }),
        };
    }

    /// How the callee of `call` invokes its lambda argument `lambda`, when it runs it in place.
    fn invocation(&self, call: ExprId, lambda: ExprId) -> Option<InvocationKind> {
        let declared = self.checker.contract_for_call(call).and_then(|contract| {
            contract.effects.iter().find_map(|effect| match effect {
                Effect::CallsInPlace { param, kind }
                    if self.checker.contract_arg_expr(call, *param) == Some(lambda) =>
                {
                    Some(*kind)
                }
                _ => None,
            })
        });
        declared.or_else(|| {
            (self.checker.argument_lambda_inlining.get(&lambda) == Some(&true))
                .then_some(InvocationKind::Unknown)
        })
    }

    /// The arguments are evaluated in source order, and the callee runs its in-place lambdas only
    /// after all of them: `invoke({ x = 1 }, x)` reads `x` before the lambda assigns it.
    fn arguments(&mut self, call: ExprId, arguments: &[ExprId]) {
        let mut in_place = Vec::new();
        for &argument in arguments {
            if matches!(self.checker.file.expr(argument), Expr::Lambda { .. }) {
                match self.invocation(call, argument) {
                    Some(kind) => in_place.push((argument, kind)),
                    None => self.detached_lambda(argument),
                }
            } else {
                self.expr(argument);
            }
        }
        for (lambda, kind) in in_place {
            self.in_place_lambda(lambda, kind);
        }
    }

    fn jump_to_loop(&mut self, label: Option<&String>, continues: bool) {
        let flow = self.flow.clone();
        if let Some(target) = self
            .loops
            .iter_mut()
            .rev()
            .find(|frame| label.is_none() || frame.label.as_ref() == label)
        {
            let edge = if continues {
                &mut target.continues
            } else {
                &mut target.breaks
            };
            *edge = join(edge.take(), Some(flow));
        }
        self.flow.live = false;
    }

    fn jump_out(&mut self, target: Option<ReturnTarget>) {
        if let Some(ReturnTarget::Lambda(lambda)) = target {
            let flow = self.flow.clone();
            if let Some(frame) = self
                .lambdas
                .iter_mut()
                .rev()
                .find(|frame| frame.lambda == lambda)
            {
                frame.exits = join(frame.exits.take(), Some(flow));
            }
        }
        self.flow.live = false;
    }

    /// Evaluates `optional` on only some paths: the result joins the paths that skip it.
    fn conditionally(&mut self, optional: impl FnOnce(&mut Self)) {
        let skipped = self.flow.clone();
        optional(self);
        self.flow = skipped.join(self.flow.clone());
    }

    fn boolean_literal(&self, expression: ExprId) -> Option<bool> {
        match self.checker.file.expr(expression) {
            Expr::BoolLit(value) => Some(*value),
            _ => None,
        }
    }

    fn expr(&mut self, expression: ExprId) {
        self.evaluate(expression);
        // An expression of type `Nothing` (a call, a property read, a `throw`) never completes.
        if self.checker.expr_types.get(expression.0 as usize) == Some(&Ty::Nothing) {
            self.flow.live = false;
        }
    }

    fn evaluate(&mut self, expression: ExprId) {
        let file = self.checker.file;
        match file.expr(expression) {
            Expr::Name(_) => self.read(expression),
            Expr::Lambda { .. } => self.detached_lambda(expression),
            Expr::Throw { operand } => {
                self.expr(*operand);
                self.flow.live = false;
            }
            Expr::Return { value, .. } => {
                if let Some(value) = value {
                    self.expr(*value);
                }
                let target = self.checker.expr_return_targets.get(&expression).copied();
                self.jump_out(target);
            }
            Expr::Break { label } => self.jump_to_loop(label.as_ref(), false),
            Expr::Continue { label } => self.jump_to_loop(label.as_ref(), true),
            Expr::Elvis { lhs, rhs } => {
                self.expr(*lhs);
                self.conditionally(|analysis| analysis.expr(*rhs));
            }
            Expr::Binary {
                op: BinOp::And | BinOp::Or,
                lhs,
                rhs,
                ..
            } => {
                self.expr(*lhs);
                self.conditionally(|analysis| analysis.expr(*rhs));
            }
            Expr::IncDec { target, .. } => {
                if let Some(&PathRoot::Value(identity)) = self.checker.read_flow_roots.get(target) {
                    let span = self.checker.span(*target);
                    self.read_local(identity, span);
                    self.write(identity, span);
                } else {
                    self.expr(*target);
                }
            }
            Expr::Call { callee, args } => {
                self.expr(*callee);
                self.arguments(expression, args);
                // An object expression evaluates its super-constructor arguments where it is
                // written. Its body sees its captures through class storage, which this analysis
                // does not follow yet.
                if let Some(&declaration) = file.anonymous_object_classes.get(&expression) {
                    if let Decl::Class(class) = file.decl(declaration) {
                        for &argument in &class.base_args {
                            self.expr(argument);
                        }
                        for delegation in &class.interface_delegations {
                            self.expr(delegation.value);
                        }
                    }
                }
            }
            Expr::SafeCall { receiver, args, .. } => {
                self.expr(*receiver);
                if let Some(args) = args {
                    self.conditionally(|analysis| analysis.arguments(expression, args));
                }
            }
            Expr::SafeIndex {
                receiver,
                access,
                element,
                indices,
            }
            | Expr::SafeIndexIncDec {
                receiver,
                access,
                element,
                indices,
                ..
            } => {
                self.expr(*receiver);
                self.conditionally(|analysis| {
                    analysis.expr(*access);
                    for &index in indices {
                        analysis.expr(index);
                    }
                    analysis.expr(*element);
                });
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.expr(*cond);
                let condition = self.flow.clone();
                self.expr(*then_branch);
                let then_flow = std::mem::replace(&mut self.flow, condition);
                if let Some(else_branch) = else_branch {
                    self.expr(*else_branch);
                }
                self.flow = then_flow.join(self.flow.clone());
            }
            Expr::When { subject, arms } => self.when(expression, *subject, arms),
            Expr::Block { stmts, trailing } => {
                for &statement in stmts {
                    self.stmt(statement);
                }
                if let Some(trailing) = trailing {
                    self.expr(*trailing);
                }
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => self.try_expression(
                *body,
                &catches.iter().map(|clause| clause.body).collect::<Vec<_>>(),
                *finally,
            ),
            _ => {
                for child in child_expressions(file, expression) {
                    self.expr(child);
                }
            }
        }
    }

    fn when(&mut self, when: ExprId, subject: Option<ExprId>, arms: &[crate::ast::WhenArm]) {
        if let Some(subject) = subject {
            self.expr(subject);
        }
        let mut unmatched = self.flow.clone();
        let mut out: Option<Flow> = None;
        let mut has_else = false;
        for arm in arms {
            self.flow = unmatched.clone();
            if arm.conditions.is_empty() {
                has_else = true;
                self.expr(arm.body);
                out = join(out, Some(self.flow.clone()));
                unmatched = self.flow.clone().dead();
                continue;
            }
            let mut matched: Option<Flow> = None;
            for condition in &arm.conditions {
                self.expr(condition.expression());
                matched = join(matched, Some(self.flow.clone()));
            }
            unmatched = self.flow.clone();
            self.flow = matched.expect("an arm has a condition");
            if let Some(guard) = arm.guard {
                self.expr(guard);
                unmatched = unmatched.join(self.flow.clone());
            }
            self.expr(arm.body);
            out = join(out, Some(self.flow.clone()));
        }
        let exhaustive = has_else || self.checker.exhaustive_whens.contains(&when);
        self.flow = join(out, Some(unmatched.dead_if(exhaustive))).expect("a path");
    }

    fn try_expression(&mut self, body: ExprId, catches: &[ExprId], finally: Option<ExprId>) {
        let entry = self.flow.clone();
        // An exception may leave the body before or after any of its writes.
        let interrupted = |writes: &BTreeSet<u32>| {
            let mut flow = entry.clone();
            flow.possible.extend(writes.iter().copied());
            flow
        };
        self.try_writes.push(BTreeSet::new());
        self.expr(body);
        let body_writes = self.try_writes.pop().expect("pushed above");
        let mut out = self.flow.clone();
        self.try_writes.push(body_writes.clone());
        for &catch in catches {
            self.flow = interrupted(&body_writes);
            self.expr(catch);
            out = out.join(self.flow.clone());
        }
        let all_writes = self.try_writes.pop().expect("pushed above");
        let Some(finally) = finally else {
            self.flow = out;
            return;
        };
        // The `finally` runs after a normal completion and after an exception anywhere; it reports
        // on both together, and the code after it continues from the normal completion only.
        self.flow = out.clone().join(interrupted(&all_writes));
        self.expr(finally);
        let report = std::mem::replace(&mut self.report, false);
        self.flow = out;
        self.expr(finally);
        self.report = report;
    }

    fn loop_body(&mut self, label: Option<&String>, body: ExprId) -> LoopFrame {
        self.loops.push(LoopFrame {
            label: label.cloned(),
            breaks: None,
            continues: None,
        });
        self.expr(body);
        self.loops.pop().expect("pushed above")
    }

    fn stmt(&mut self, statement: StmtId) {
        let file = self.checker.file;
        match file.stmt(statement) {
            Stmt::Local { init, name, .. } => {
                match self
                    .checker
                    .local_initialization
                    .declarations
                    .get(&statement)
                {
                    Some(&identity) => {
                        self.tracked.insert(
                            identity,
                            Tracked {
                                name: name.clone(),
                                is_val: file.deferred_val_declarations.contains(&statement),
                                frame: self.frame,
                            },
                        );
                        // A declaration in a loop starts each iteration uninitialized.
                        self.flow.definite.remove(&identity);
                        self.flow.possible.remove(&identity);
                    }
                    None => self.expr(*init),
                }
            }
            Stmt::Assign { value, .. } => {
                let target = self.checker.assignment_target_span(statement);
                match file.expr(*value) {
                    // `x op= v` is parsed as `x = x op v` whose read of `x` stands at the
                    // operator; kotlinc reports that read at the target.
                    Expr::Binary {
                        lhs,
                        rhs,
                        operator_span,
                        ..
                    } if self.checker.span(*lhs) == *operator_span => {
                        match self.checker.read_flow_roots.get(lhs) {
                            Some(&PathRoot::Value(identity)) => self.read_local(identity, target),
                            _ => self.expr(*lhs),
                        }
                        self.expr(*rhs);
                    }
                    _ => self.expr(*value),
                }
                if let Some(&identity) = self.checker.local_initialization.writes.get(&statement) {
                    self.write(identity, target);
                }
            }
            Stmt::IncDec { .. } => {
                if let Some(&identity) = self.checker.local_initialization.writes.get(&statement) {
                    let span = self.checker.assignment_target_span(statement);
                    self.read_local(identity, span);
                    self.write(identity, span);
                }
            }
            Stmt::Return(value, _) => {
                if let Some(value) = value {
                    self.expr(*value);
                }
                let target = self.checker.stmt_return_targets.get(&statement).copied();
                self.jump_out(target);
            }
            Stmt::Break(label) => self.jump_to_loop(label.as_ref(), false),
            Stmt::Continue(label) => self.jump_to_loop(label.as_ref(), true),
            Stmt::While { cond, body, label } => {
                let (cond, body) = (*cond, *body);
                let literal = self.boolean_literal(cond);
                let entry = self.flow.clone();
                self.flow = self.repeat(entry, |analysis, input| {
                    analysis.flow = input;
                    analysis.expr(cond);
                    let condition = analysis.flow.clone();
                    analysis.flow = condition.clone().dead_if(literal == Some(false));
                    let frame = analysis.loop_body(label.as_ref(), body);
                    let back = join(Some(analysis.flow.clone()), frame.continues);
                    let exit = join(Some(condition.dead_if(literal == Some(true))), frame.breaks);
                    (exit.expect("a path"), back)
                });
            }
            Stmt::DoWhile { body, cond, label } => {
                let (cond, body) = (*cond, *body);
                let literal = self.boolean_literal(cond);
                let entry = self.flow.clone();
                self.flow = self.repeat(entry, |analysis, input| {
                    analysis.flow = input;
                    let frame = analysis.loop_body(label.as_ref(), body);
                    analysis.flow =
                        join(Some(analysis.flow.clone()), frame.continues).expect("a path");
                    analysis.expr(cond);
                    let condition = analysis.flow.clone();
                    let back = condition.clone().dead_if(literal == Some(false));
                    let exit = join(Some(condition.dead_if(literal == Some(true))), frame.breaks);
                    (exit.expect("a path"), Some(back))
                });
            }
            Stmt::For {
                range, body, label, ..
            } => {
                self.expr(range.start);
                self.expr(range.end);
                self.iteration(label.as_ref(), *body);
            }
            Stmt::ForEach {
                iterable,
                body,
                label,
                ..
            } => {
                self.expr(*iterable);
                self.iteration(label.as_ref(), *body);
            }
            Stmt::LocalFun(function) => {
                if let crate::ast::FunBody::Expr(body) | crate::ast::FunBody::Block(body) =
                    &function.body
                {
                    let body = *body;
                    self.detached(|analysis| analysis.expr(body));
                }
            }
            _ => {
                let mut children = Vec::new();
                file.any_child_stmt(statement, &mut |child| {
                    children.push(child);
                    false
                });
                for child in children {
                    self.expr(child);
                }
            }
        }
    }

    /// A `for` loop: the header decides on each iteration whether the body runs again.
    fn iteration(&mut self, label: Option<&String>, body: ExprId) {
        let label = label.cloned();
        let entry = self.flow.clone();
        self.flow = self.repeat(entry, |analysis, input| {
            analysis.flow = input.clone();
            let frame = analysis.loop_body(label.as_ref(), body);
            let back = join(Some(analysis.flow.clone()), frame.continues);
            (join(Some(input), frame.breaks).expect("a path"), back)
        });
    }
}

/// The direct child expressions of an expression evaluated in order, for the forms whose flow is
/// plain sequencing.
fn child_expressions(file: &crate::ast::File, expression: ExprId) -> Vec<ExprId> {
    let mut children = Vec::new();
    let mut statements = Vec::new();
    file.any_child_expr(
        expression,
        &mut |child| {
            children.push(child);
            false
        },
        &mut |statement| {
            statements.push(statement);
            false
        },
    );
    debug_assert!(statements.is_empty(), "only a block holds statements");
    children
}

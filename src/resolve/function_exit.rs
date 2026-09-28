//! Whether a checked expression leaves the enclosing function.
//!
//! The missing-return check reads this. A `do`/`while` runs its body before the condition, so a body
//! that leaves the function on every path leaves the function too. A `break` or `continue` does that
//! only when a `finally` returns or throws over it; a transfer that reaches the loop exits the loop
//! instead.

use super::lambda_returns::ReturnTarget;
use super::Checker;
use crate::ast::{Expr, ExprId, Stmt, StmtId};
use crate::types::Ty;

impl Checker<'_> {
    /// True if evaluating `e` always transfers control away (a `return`, or a block/if whose every
    /// exit does). Used to detect early-return guards for smart-casting the rest of a block.
    pub(super) fn expr_diverges(&self, e: ExprId) -> bool {
        match self.file.expr(e) {
            Expr::Throw { .. }
            | Expr::Return { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => true,
            Expr::Block { stmts, trailing } => {
                if let Some(te) = trailing {
                    self.expr_diverges(*te)
                } else if let Some(&last) = stmts.last() {
                    matches!(
                        self.file.stmt(last),
                        Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_)
                    )
                } else {
                    false
                }
            }
            Expr::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => self.expr_diverges(*then_branch) && self.expr_diverges(*eb),
            Expr::Call { callee, args } => {
                self.expr_diverges(*callee)
                    || args.iter().any(|argument| self.expr_diverges(*argument))
            }
            _ => false,
        }
    }

    /// Whether statement `s` always transfers control (so subsequent statements in its block are dead).
    pub(super) fn stmt_diverges(&self, s: StmtId) -> bool {
        match self.file.stmt(s) {
            Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_) => true,
            Stmt::Expr(e) => self.expr_diverges(*e),
            _ => false,
        }
    }

    /// Whether the block-body expression `e` transfers control out of the function on every path — a
    /// `return`/`throw`, a `Nothing`-typed call (`error(…)`/`TODO()`/any `Nothing`-returning fn), an
    /// `if` whose both branches do, a `when` whose every arm does, a `try` whose paths all do, or an
    /// infinite `while (true)`. Drives the block-body missing-return check.
    ///
    /// Deliberately conservative TOWARD `true`: a spurious `true` only fails to flag a genuine
    /// missing return (harmless), whereas a spurious `false` would reject a function that DOES return
    /// (a false error). So a subject-exhaustive `when` needs no explicit `else` here, and a
    /// `while (true)` is treated as non-terminating-fallthrough regardless of an inner `break`.
    pub(super) fn body_terminates(&self, e: ExprId) -> bool {
        // A `Nothing`-typed expression never completes normally (the checker already resolved the
        // result type — no hardcoded intrinsic name list).
        if self.expr_types[e.0 as usize] == Ty::Nothing {
            return true;
        }
        match self.file.expr(e) {
            Expr::Return { .. } | Expr::Throw { .. } => true,
            Expr::Block { stmts, trailing } => {
                if stmts.iter().any(|s| self.stmt_terminates(*s)) {
                    return true;
                }
                trailing.is_some_and(|t| self.body_terminates(t))
            }
            Expr::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => self.body_terminates(*then_branch) && self.body_terminates(*eb),
            Expr::When { arms, .. } => {
                !arms.is_empty() && arms.iter().all(|a| self.body_terminates(a.body))
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                if finally.is_some_and(|f| self.body_terminates(f)) {
                    return true;
                }
                self.body_terminates(*body) && catches.iter().all(|c| self.body_terminates(c.body))
            }
            Expr::Call { callee, args } => {
                self.expression_terminates_function(*callee)
                    || args
                        .iter()
                        .any(|argument| self.expression_terminates_function(*argument))
                    || self.call_contract_terminates_function(e)
            }
            _ => false,
        }
    }

    /// Whether a selected call necessarily invokes a lambda that necessarily exits the enclosing
    /// function. Both facts are semantic: invocation comes from the selected callable's contract,
    /// and the return owner comes from the checker's recorded return target.
    fn call_contract_terminates_function(&self, call: ExprId) -> bool {
        let Some(contract) = self.contract_for_call(call) else {
            return false;
        };
        contract.effects.iter().any(|effect| {
            let crate::contracts::Effect::CallsInPlace { param, kind } = effect else {
                return false;
            };
            if !matches!(
                kind,
                crate::contracts::InvocationKind::ExactlyOnce
                    | crate::contracts::InvocationKind::AtLeastOnce
            ) {
                return false;
            }
            self.contract_arg_expr(call, *param)
                .is_some_and(|argument| self.lambda_terminates_function(argument))
        })
    }

    fn lambda_terminates_function(&self, lambda: ExprId) -> bool {
        let Expr::Lambda { body, .. } = self.file.expr(lambda) else {
            return false;
        };
        self.expression_terminates_function(*body)
    }

    pub(super) fn expression_terminates_function(&self, expression: ExprId) -> bool {
        match self.file.expr(expression) {
            Expr::Return { .. } => {
                self.expr_return_targets.get(&expression) == Some(&ReturnTarget::Function)
            }
            Expr::Throw { .. } => true,
            Expr::Break { .. } | Expr::Continue { .. } => false,
            Expr::Block { stmts, trailing } => {
                stmts
                    .iter()
                    .any(|statement| self.statement_terminates_function(*statement))
                    || trailing
                        .is_some_and(|trailing| self.expression_terminates_function(trailing))
            }
            Expr::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => {
                self.expression_terminates_function(*then_branch)
                    && self.expression_terminates_function(*else_branch)
            }
            Expr::When { arms, .. } => {
                !arms.is_empty()
                    && arms
                        .iter()
                        .all(|arm| self.expression_terminates_function(arm.body))
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                finally.is_some_and(|finally| self.expression_terminates_function(finally))
                    || (self.expression_terminates_function(*body)
                        && catches
                            .iter()
                            .all(|catch| self.expression_terminates_function(catch.body)))
            }
            Expr::Call { callee, args } => {
                self.expression_terminates_function(*callee)
                    || args
                        .iter()
                        .any(|argument| self.expression_terminates_function(*argument))
                    || self.call_contract_terminates_function(expression)
            }
            _ => self.expr_types[expression.0 as usize] == Ty::Nothing,
        }
    }

    fn statement_terminates_function(&self, statement: StmtId) -> bool {
        match self.file.stmt(statement) {
            Stmt::Return(..) => {
                self.stmt_return_targets.get(&statement) == Some(&ReturnTarget::Function)
            }
            Stmt::Expr(expression) => self.expression_terminates_function(*expression),
            _ => false,
        }
    }

    /// Whether statement `s` guarantees the function returns/throws (for the missing-return check).
    fn stmt_terminates(&self, s: StmtId) -> bool {
        match self.file.stmt(s) {
            Stmt::Return(..) => true,
            Stmt::Expr(e) => self.body_terminates(*e),
            // Initializers are evaluated as part of the declaration statement. If the selected
            // expression has type `Nothing` (or otherwise exits the function), the declaration
            // cannot complete and the enclosing block cannot fall through. This matters for an
            // inline generic call whose lambda returns non-locally:
            // `val ignored = action { return value }` specializes the call result to `Nothing`.
            Stmt::Local { init, .. } | Stmt::Destructure { init, .. } => {
                self.body_terminates(*init)
            }
            Stmt::LocalDelegate { delegate, .. } => self.body_terminates(*delegate),
            Stmt::Assign { value, .. } => self.body_terminates(*value),
            Stmt::AssignMember {
                receiver, value, ..
            } => self.body_terminates(*receiver) || self.body_terminates(*value),
            Stmt::AssignIndex {
                array,
                indices,
                value,
            } => {
                self.body_terminates(*array)
                    || indices.iter().any(|index| self.body_terminates(*index))
                    || self.body_terminates(*value)
            }
            Stmt::CompoundAssign { target, value, .. } => {
                self.body_terminates(*target) || self.body_terminates(*value)
            }
            // `while (true)` never falls through (an inner `break` only under-reports here, which
            // is safe — it can't cause a false missing-return error). A `do`/`while (true)` is the
            // same. A `do`/`while` also leaves the function when its body does, because the body
            // runs before the condition.
            Stmt::While { cond, .. } => {
                self.expression_terminates_function(*cond)
                    || matches!(self.file.expr(*cond), Expr::BoolLit(true))
            }
            Stmt::DoWhile { body, cond, .. } => {
                self.do_while_body_exits_function(*body)
                    || self.expression_terminates_function(*cond)
                    || matches!(self.file.expr(*cond), Expr::BoolLit(true))
            }
            // Loop headers are evaluated before the loop can fall through. A non-local return or
            // throw in a range bound/iterable therefore terminates the enclosing function even
            // when the iteration space itself could be empty. `break`/`continue` are explicitly
            // excluded by `expression_terminates_function`: they target the loop, not the function.
            Stmt::For { range, .. } => {
                self.expression_terminates_function(range.start)
                    || self.expression_terminates_function(range.end)
            }
            Stmt::ForEach { iterable, .. } => self.expression_terminates_function(*iterable),
            _ => false,
        }
    }

    /// The `do`/`while` body leaves the function when every path does, and no `break`/`continue`
    /// reaches this loop. A transfer inside a `try` whose `finally` itself leaves the function is
    /// covered by that `finally` and does not reach the loop.
    fn do_while_body_exits_function(&self, body: ExprId) -> bool {
        let Expr::Block { stmts, trailing } = self.file.expr(body) else {
            return !self.expression_escapes_loop(body, &[]) && self.body_terminates(body);
        };
        for statement in stmts {
            if self.statement_escapes_loop(*statement, &[]) {
                return false;
            }
            if self.stmt_terminates(*statement) {
                return true;
            }
        }
        trailing.is_some_and(|trailing| {
            !self.expression_escapes_loop(trailing, &[]) && self.body_terminates(trailing)
        })
    }

    /// `nested` lists loops inside the `do`/`while` being asked about, innermost last. A transfer
    /// leaves that `do`/`while` when it targets the `do`/`while` itself or a loop further out.
    fn statement_escapes_loop(&self, statement: StmtId, nested: &[Option<String>]) -> bool {
        match self.file.stmt(statement) {
            Stmt::Break(label) | Stmt::Continue(label) => transfer_leaves_loop(label, nested),
            Stmt::While { cond, body, label } => {
                let deeper = push_loop(nested, label);
                // A `while` condition is outside that loop: `while (break)` leaves the enclosing one.
                self.expression_escapes_loop(*cond, nested)
                    || self.expression_escapes_loop(*body, &deeper)
            }
            Stmt::DoWhile { body, cond, label } => {
                let deeper = push_loop(nested, label);
                // A `do`/`while` condition is inside that loop: `do { … } while (break)` leaves it.
                self.expression_escapes_loop(*body, &deeper)
                    || self.expression_escapes_loop(*cond, &deeper)
            }
            Stmt::For {
                range, body, label, ..
            } => {
                let deeper = push_loop(nested, label);
                self.expression_escapes_loop(range.start, nested)
                    || self.expression_escapes_loop(range.end, nested)
                    || self.expression_escapes_loop(*body, &deeper)
            }
            Stmt::ForEach {
                iterable,
                body,
                label,
                ..
            } => {
                let deeper = push_loop(nested, label);
                self.expression_escapes_loop(*iterable, nested)
                    || self.expression_escapes_loop(*body, &deeper)
            }
            // A local function or class is its own break scope.
            Stmt::LocalFun(_) | Stmt::LocalClass(_) | Stmt::LocalTypeAlias(_) => false,
            _ => self.file.any_child_stmt(statement, &mut |child| {
                self.expression_escapes_loop(child, nested)
            }),
        }
    }

    fn expression_escapes_loop(&self, expression: ExprId, nested: &[Option<String>]) -> bool {
        match self.file.expr(expression) {
            Expr::Break { label } | Expr::Continue { label } => transfer_leaves_loop(label, nested),
            // `break` is not allowed in a lambda; a non-local `return` there is a function exit,
            // which the call's contract records. The lambda body is not this loop's break scope.
            Expr::Lambda { .. } => false,
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                if let Some(finally) = *finally {
                    let escapes = self.expression_escapes_loop(finally, nested);
                    if escapes {
                        return true;
                    }
                    if self.body_terminates(finally) {
                        return false;
                    }
                }
                self.expression_escapes_loop(*body, nested)
                    || catches
                        .iter()
                        .any(|catch| self.expression_escapes_loop(catch.body, nested))
            }
            _ => self.file.any_child_expr(
                expression,
                &mut |child| self.expression_escapes_loop(child, nested),
                &mut |child| self.statement_escapes_loop(child, nested),
            ),
        }
    }
}

/// An unlabeled transfer targets the innermost loop. With `nested` empty, that is the `do`/`while`
/// under analysis. A label that names one of `nested` stays inside; any other label leaves.
fn transfer_leaves_loop(label: &Option<String>, nested: &[Option<String>]) -> bool {
    match label {
        None => nested.is_empty(),
        Some(name) => !nested
            .iter()
            .any(|nested_label| nested_label.as_deref() == Some(name.as_str())),
    }
}

fn push_loop(nested: &[Option<String>], label: &Option<String>) -> Vec<Option<String>> {
    let mut deeper = Vec::with_capacity(nested.len() + 1);
    deeper.extend_from_slice(nested);
    deeper.push(label.clone());
    deeper
}

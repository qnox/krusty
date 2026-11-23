//! Recursive calls of a `tailrec` function: kotlinc's `FirTailrecFunctionChecker`.
//!
//! A `tailrec` function promises that its self-calls run as a loop. kotlinc keeps the declaration
//! when the promise cannot be kept and reports it instead:
//!
//! - `NON_TAIL_RECURSIVE_CALL` at a self-call that something still runs after, or that dispatches
//!   on another receiver than the function's own instance, or that leaves out an argument of an
//!   overriding function;
//! - `TAIL_RECURSION_IN_TRY_IS_NOT_SUPPORTED` at a self-call written anywhere in a `try`, its
//!   handlers and its `finally` included;
//! - `NO_TAIL_CALLS_FOUND` at the `tailrec` modifier when no self-call is a tail call.
//!
//! A self-call is a tail call when no instruction follows it but the ones that leave the function:
//! it is the value of a `return` of the function, the last statement of a tail block, a branch of a
//! tail `if`/`when`, the right operand of a tail `&&`, `||` or `?:`, or a statement a bare `return`
//! of the function directly follows. A call inside an inline lambda is a tail call only through a
//! non-local `return`. Code no path reaches is never a call of the function.
//!
//! The analysis runs once the body is checked, over the calls the checker selected. A call is the
//! function's own when it selected this declaration, so no name is compared.

use super::*;
use crate::fir::DeclarationFlags;
use crate::kotlin_version::{Quirk, Quirks};

/// How a `tailrec` function is identified by the calls that select it.
#[derive(Clone, Copy)]
pub(super) enum TailrecTarget {
    /// A top-level, member or extension function, by its stable declaration.
    Declaration(crate::fir::DeclarationId),
    /// A function declared in a body, by its declaring statement.
    Local(StmtId),
}

/// Kotlinc orders these from least to most severe: a call keeps the most severe thing wrong with
/// it, and only a call with nothing wrong counts as a tail call.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CallKind {
    Valid,
    FollowingInstructions,
    InsideTry,
    WrongReceiver,
    DefaultArguments,
}

struct RecursiveCall {
    span: Span,
    kind: CallKind,
}

/// The class a member belongs to: calls on its own instance are the function's own.
#[derive(Clone, Copy)]
struct OwnerClass {
    declaration: crate::fir::DeclarationId,
    singleton: bool,
}

/// What the walk knows about the position it is visiting.
#[derive(Clone, Copy)]
struct Position {
    /// Nothing runs after the expression but what leaves the function.
    tail: bool,
    /// Inside a `try`, a handler or a `finally`.
    in_try: bool,
    /// No path reaches the expression.
    dead: bool,
    /// An unlabeled `return` here leaves the function: the body, or a lambda written in it. A
    /// nested function's returns leave that function instead.
    returns_from_function: bool,
}

impl Position {
    fn operand(self) -> Self {
        Self {
            tail: false,
            ..self
        }
    }
}

struct Walk<'a, 'c> {
    checker: &'a Checker<'c>,
    function: &'a FunDecl,
    target: TailrecTarget,
    owner: Option<OwnerClass>,
    quirks: Quirks,
    calls: Vec<RecursiveCall>,
}

impl Checker<'_> {
    /// Report the recursive calls of `function`, which was declared `tailrec`.
    pub(super) fn check_tailrec_calls(&mut self, function: &FunDecl, target: TailrecTarget) {
        if !function.is_tailrec()
            || self.signature_defaults_only
            || self.inference_only
            || self.discover_anonymous_captures
        {
            return;
        }
        let (FunBody::Expr(body) | FunBody::Block(body)) = function.body else {
            return;
        };
        let Some(modifier) = function.tailrec_span else {
            return;
        };
        let owner = match target {
            TailrecTarget::Declaration(declaration) => self.tailrec_owner(declaration),
            TailrecTarget::Local(_) => None,
        };
        self.report_tailrec_calls(function, target, owner, body, modifier);
    }

    fn tailrec_owner(&self, declaration: crate::fir::DeclarationId) -> Option<OwnerClass> {
        let index = self.resolved_index?;
        let owner = index.enclosing_owner_classifier(declaration)?;
        Some(OwnerClass {
            declaration: owner.declaration,
            singleton: index
                .declaration_header(owner.declaration)?
                .flags
                .has(DeclarationFlags::SINGLETON | DeclarationFlags::COMPANION),
        })
    }

    fn report_tailrec_calls(
        &mut self,
        function: &FunDecl,
        target: TailrecTarget,
        owner: Option<OwnerClass>,
        body: ExprId,
        modifier: Span,
    ) {
        let mut walk = Walk {
            checker: self,
            function,
            target,
            owner,
            quirks: Quirks::target(),
            calls: Vec::new(),
        };
        walk.expression(
            body,
            Position {
                tail: true,
                in_try: false,
                dead: false,
                returns_from_function: true,
            },
        );
        let mut calls = walk.calls;
        calls.sort_by_key(|call| call.span.lo);
        let tail_calls = calls
            .iter()
            .filter(|call| call.kind == CallKind::Valid)
            .count();
        if tail_calls == 0 {
            self.diags.warning_named(
                modifier,
                crate::diag::names::NO_TAIL_CALLS_FOUND,
                "a function is marked as tail-recursive but no tail calls are found.".to_string(),
            );
        }
        for call in calls {
            match call.kind {
                CallKind::Valid => {}
                CallKind::InsideTry => self.diags.warning_named(
                    call.span,
                    crate::diag::names::TAIL_RECURSION_IN_TRY_IS_NOT_SUPPORTED,
                    "tail recursion optimization inside try/catch/finally is not supported."
                        .to_string(),
                ),
                CallKind::FollowingInstructions
                | CallKind::WrongReceiver
                | CallKind::DefaultArguments => self.diags.warning_named(
                    call.span,
                    crate::diag::names::NON_TAIL_RECURSIVE_CALL,
                    "recursive call is not a tail call.".to_string(),
                ),
            }
        }
    }
}

impl Walk<'_, '_> {
    fn expression(&mut self, expression: ExprId, position: Position) {
        let file = self.checker.file;
        match file.expr(expression) {
            Expr::Return { value, label } => {
                let leaves = self.leaves_function(label.as_deref(), position);
                if let Some(value) = *value {
                    self.expression(
                        value,
                        Position {
                            tail: leaves,
                            ..position
                        },
                    );
                }
                return;
            }
            Expr::Lambda { body, .. } => {
                self.expression(*body, position.operand());
                return;
            }
            Expr::Block { stmts, trailing } => {
                self.sequence(stmts, *trailing, position);
                return;
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.expression(*cond, position.operand());
                self.expression(*then_branch, position);
                if let Some(else_branch) = else_branch {
                    self.expression(*else_branch, position);
                }
                return;
            }
            Expr::When { subject, arms } => {
                if let Some(subject) = subject {
                    self.expression(*subject, position.operand());
                }
                for arm in arms {
                    for condition in &arm.conditions {
                        self.expression(condition.expression(), position.operand());
                    }
                    if let Some(guard) = arm.guard {
                        self.expression(guard, position.operand());
                    }
                    self.expression(arm.body, position);
                }
                return;
            }
            Expr::Elvis { lhs, rhs } => {
                let left_is_tail = position.tail
                    && self.quirks.has(Quirk::TailrecElvisLeftOperandIsTail)
                    && !self.checker.ret_ty.is_nullable();
                self.expression(
                    *lhs,
                    Position {
                        tail: left_is_tail,
                        ..position
                    },
                );
                self.expression(*rhs, position);
                return;
            }
            Expr::Binary {
                op: BinOp::And | BinOp::Or,
                lhs,
                rhs,
                ..
            } => {
                self.expression(*lhs, position.operand());
                self.expression(*rhs, position);
                return;
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                let inside = Position {
                    tail: false,
                    in_try: true,
                    ..position
                };
                self.expression(*body, inside);
                for handler in catches {
                    self.expression(handler.body, inside);
                }
                if let Some(finally) = finally {
                    self.expression(*finally, inside);
                }
                return;
            }
            Expr::Call { .. } | Expr::SafeCall { .. } | Expr::Binary { .. } => {
                self.record_if_recursive(expression, position);
                if let Some(&object) = file.anonymous_object_classes.get(&expression) {
                    self.declaration(object, position);
                }
            }
            _ => {}
        }
        let operand = position.operand();
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
        for child in children {
            self.expression(child, operand);
        }
        for statement in statements {
            self.statement(statement, operand);
        }
    }

    /// The bodies of a class declared in the function, which run on their own instance and so never
    /// as a tail of the function.
    fn declaration(&mut self, declaration: DeclId, position: Position) {
        let inside = Position {
            tail: false,
            returns_from_function: false,
            ..position
        };
        let mut roots = Vec::new();
        self.checker.file.any_decl_expr(declaration, &mut |root| {
            roots.push(root);
            false
        });
        for root in roots {
            self.expression(root, inside);
        }
    }

    /// A block's statements in order. A statement is a tail when the next one is a bare `return` of
    /// the function, or when it is the last and the block is.
    fn sequence(&mut self, statements: &[StmtId], trailing: Option<ExprId>, position: Position) {
        let mut dead = position.dead;
        for (index, &statement) in statements.iter().enumerate() {
            let next_leaves = match statements.get(index + 1) {
                Some(&next) => self.statement_is_bare_return(next, position),
                None => {
                    trailing.is_some_and(|value| self.expression_is_bare_return(value, position))
                }
            };
            let last = index + 1 == statements.len() && trailing.is_none();
            self.statement(
                statement,
                Position {
                    tail: next_leaves || (last && position.tail),
                    dead,
                    ..position
                },
            );
            dead = dead || self.checker.stmt_terminates(statement);
        }
        if let Some(value) = trailing {
            self.expression(value, Position { dead, ..position });
        }
    }

    fn statement(&mut self, statement: StmtId, position: Position) {
        let file = self.checker.file;
        match file.stmt(statement) {
            Stmt::Expr(expression) => self.expression(*expression, position),
            Stmt::Return(value, label) => {
                let leaves = self.leaves_function(label.as_deref(), position);
                if let Some(value) = *value {
                    self.expression(
                        value,
                        Position {
                            tail: leaves,
                            ..position
                        },
                    );
                }
            }
            Stmt::LocalClass(_) => {
                if let Some(&class) = file.local_class_decls.get(&statement) {
                    self.declaration(class, position);
                }
            }
            Stmt::LocalFun(nested) => {
                if let FunBody::Expr(body) | FunBody::Block(body) = &nested.body {
                    self.expression(
                        *body,
                        Position {
                            tail: false,
                            returns_from_function: false,
                            ..position
                        },
                    );
                }
            }
            _ => {
                let operand = position.operand();
                file.any_child_stmt(statement, &mut |child| {
                    self.expression(child, operand);
                    false
                });
            }
        }
    }

    /// Whether a `return` written with `label` leaves the function being checked.
    fn leaves_function(&self, label: Option<&str>, position: Position) -> bool {
        position.returns_from_function
            && match label {
                None => true,
                Some(label) => label == self.function.name,
            }
    }

    /// A statement that leaves the function with no value, or with `Unit`, which kotlinc reads
    /// without an instruction of its own.
    fn statement_is_bare_return(&self, statement: StmtId, position: Position) -> bool {
        match self.checker.file.stmt(statement) {
            Stmt::Return(value, label) => {
                self.leaves_function(label.as_deref(), position)
                    && value.is_none_or(|value| self.is_bare_unit(value))
            }
            Stmt::Expr(expression) => self.expression_is_bare_return(*expression, position),
            _ => false,
        }
    }

    fn expression_is_bare_return(&self, expression: ExprId, position: Position) -> bool {
        match self.checker.file.expr(expression) {
            Expr::Return { value, label } => {
                self.leaves_function(label.as_deref(), position)
                    && value.is_none_or(|value| self.is_bare_unit(value))
            }
            _ => false,
        }
    }

    /// `return Unit` reads like a bare `return` where kotlinc builds no instruction for the object.
    fn is_bare_unit(&self, expression: ExprId) -> bool {
        !self.quirks.has(Quirk::TailrecUnitObjectReturnFollowsCall)
            && self.is_unit_object(expression)
    }

    /// The `Unit` object: a selected classifier value, not a read of a variable.
    fn is_unit_object(&self, expression: ExprId) -> bool {
        matches!(self.checker.file.expr(expression), Expr::Name(_))
            && self.checker.expr_types[expression.0 as usize] == Ty::Unit
            && !self.checker.read_flow_roots.contains_key(&expression)
    }

    fn record_if_recursive(&mut self, call: ExprId, position: Position) {
        let Some(selected) = self.selected_call(call) else {
            return;
        };
        let is_own = match (self.target, selected) {
            (TailrecTarget::Local(statement), ResolvedCall::LocalFunction(local)) => {
                local.stmt_id == statement
            }
            (TailrecTarget::Declaration(declaration), selected) => {
                selected.stable_declaration() == Some(declaration)
            }
            _ => false,
        };
        if !is_own {
            return;
        }
        let kind = if self.omits_arguments(call) {
            CallKind::DefaultArguments
        } else if !self.same_receiver(call, selected) {
            CallKind::WrongReceiver
        } else if position.dead {
            // A call no path reaches has no instructions after it and is not a tail call either.
            return;
        } else if position.in_try {
            CallKind::InsideTry
        } else if position.tail {
            CallKind::Valid
        } else {
            CallKind::FollowingInstructions
        };
        self.calls.push(RecursiveCall {
            span: self.reported_span(call),
            kind,
        });
    }

    /// The callable `call` selected: the call itself, or for an operator expression the convention
    /// the operator denotes.
    fn selected_call(&self, call: ExprId) -> Option<&ResolvedCall> {
        let checker = self.checker;
        let Expr::Binary { op, .. } = checker.file.expr(call) else {
            return checker.resolved_calls.get(&call);
        };
        let name = op.arith_operator_name().or(match op {
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Some("compareTo"),
            _ => None,
        })?;
        let key = SyntheticOperatorCall::from_name(name)?;
        checker.resolved_operator_calls.get(&(call, key))
    }

    /// Where kotlinc reports a call: the written name of a dotted or plain call, and for a call
    /// written without a dot — an infix call or an operator — the start of the whole expression,
    /// as wide as the name.
    fn reported_span(&self, call: ExprId) -> Span {
        let file = self.checker.file;
        let dotless =
            matches!(file.expr(call), Expr::Binary { .. }) || file.infix_calls.contains(&call.0);
        if !dotless || self.quirks.has(Quirk::TailrecDotlessCallReportedAtOperator) {
            return self.checker.call_callee_name_span(call);
        }
        let leading = match file.expr(call) {
            Expr::Binary { lhs, .. } => Some(*lhs),
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Member { receiver, .. } => Some(*receiver),
                _ => None,
            },
            _ => None,
        };
        // Parentheses leave no node, but the expression kotlinc reports starts at the first one.
        let start = leading
            .and_then(|operand| file.parenthesized_expression_spans.get(&operand.0))
            .map_or(self.checker.span(call).lo, |parenthesized| parenthesized.lo);
        Span::new(start, start + self.function.name.len() as u32)
    }

    /// An overriding function's call that leaves out an argument: its defaults belong to the
    /// overridden declaration, which the loop would not reach.
    fn omits_arguments(&self, call: ExprId) -> bool {
        if !self.function.is_override() || self.function.params.iter().any(|p| p.is_vararg) {
            return false;
        }
        let supplied = match self.checker.file.expr(call) {
            Expr::Call { args, .. } => args.len(),
            Expr::SafeCall { args, .. } => args.as_ref().map_or(0, Vec::len),
            Expr::Binary { .. } => 1,
            _ => return false,
        };
        supplied != self.function.params.len() - self.function.context_count
    }

    /// Whether the call dispatches on the instance the function runs on, which a loop reuses.
    fn same_receiver(&self, call: ExprId, selected: &ResolvedCall) -> bool {
        let Some(owner) = self.owner else {
            return true;
        };
        if owner.singleton {
            return true;
        }
        let file = self.checker.file;
        let dispatch = match selected {
            ResolvedCall::MemberExtension {
                dispatch_receiver, ..
            } => Some(dispatch_receiver),
            ResolvedCall::Member(_) | ResolvedCall::Companion(_) => {
                let selections = &self.checker.implicit_receiver_selections;
                match file.expr(call) {
                    Expr::Call { callee, .. } => match file.expr(*callee) {
                        Expr::Member { receiver, .. } => selections.get(receiver),
                        _ => selections.get(&call).or_else(|| selections.get(callee)),
                    },
                    Expr::SafeCall { receiver, .. } => selections.get(receiver),
                    Expr::Binary { lhs, .. } => selections.get(lhs),
                    _ => return true,
                }
            }
            _ => return true,
        };
        dispatch.is_some_and(|selection| {
            selection.singleton.is_some() || selection.classifier == Some(owner.declaration)
        })
    }
}

//! Unused return values: kotlinc's `FirUnusedReturnValueChecker`.
//!
//! An expression whose value is discarded (a statement, or the result of a block whose value
//! nobody reads) that calls a callable or reads a property whose return value must be used reports
//! `Unused return value of '<name>'.` at the written name. What must be used is the status the
//! selected declaration carries: a dependency's recorded status, or for this compilation's own
//! declaration the one resolved from its annotations, its enclosing scopes and the checker mode
//! ([`super::return_value_status`]).
//!
//! A discarded value of a type nobody can use (`Unit`, `Nothing`, an erroneous type, a non-null
//! `java.lang.Void`) is never reported. The value of `!!`, of a safe call, of an elvis expression
//! and of a cast is discarded with the operand it wraps, so the callable inside is checked.

use super::*;
use crate::features::ReturnValueCheckerMode;
use crate::types::ReturnValueStatus;

/// The callable or property a discarded expression selected, with the status it carries.
struct Discarded {
    name: String,
    status: ReturnValueStatus,
    span: Span,
}

impl Checker<'_> {
    /// Called once `expression` is typed, under the policies its own annotations opened: remember
    /// that it suppresses the report, and report it when its value is discarded.
    pub(super) fn finish_return_value_check(&mut self, expression: ExprId, ty: Ty) {
        if self.file.return_value_checker == ReturnValueCheckerMode::Disabled {
            return;
        }
        if self.suppresses_diagnostic("RETURN_VALUE_NOT_USED") {
            self.return_value_suppressed.insert(expression);
        }
        if self.discarded_exprs.contains(&expression) {
            self.check_unused_return_value(expression, ty);
        }
    }

    /// Report the unused return values of the discarded expression `expression`, whose type is
    /// `ty`. An operand of a wrapper keeps the suppression its own annotations opened, which is
    /// no longer active once its wrapper is checked.
    fn check_unused_return_value(&mut self, expression: ExprId, ty: Ty) {
        if self.discover_anonymous_captures || self.return_value_suppressed.contains(&expression) {
            return;
        }
        match self.file.expr(expression).clone() {
            Expr::NotNull { operand } | Expr::As { operand, .. } => {
                let ty = self.expr_types[operand.0 as usize];
                self.check_unused_return_value(operand, ty);
            }
            Expr::Elvis { lhs, rhs } => {
                for operand in [lhs, rhs] {
                    let ty = self.expr_types[operand.0 as usize];
                    self.check_unused_return_value(operand, ty);
                }
            }
            _ => {
                if ignorable_type(ty) {
                    return;
                }
                let Some(discarded) = self.discarded_callee(expression) else {
                    return;
                };
                if discarded.status == ReturnValueStatus::MustUse {
                    self.diags.warning_named(
                        discarded.span,
                        crate::diag::names::RETURN_VALUE_NOT_USED,
                        format!("Unused return value of '{}'.", discarded.name),
                    );
                }
            }
        }
    }

    /// The callable `expression` calls, or the property it reads, with its status and the span of
    /// the name that spells it.
    fn discarded_callee(&self, expression: ExprId) -> Option<Discarded> {
        let index = self.resolved_index?;
        match self.file.expr(expression) {
            Expr::Call { callee, .. } => {
                let call = self.resolved_calls.get(&expression)?;
                let (name, status) = self.call_return_value_status(index, call)?;
                Some(Discarded {
                    name,
                    status,
                    span: self.callee_name_span(*callee),
                })
            }
            Expr::SafeCall {
                name,
                args: Some(_),
                ..
            } => {
                let call = self.resolved_calls.get(&expression)?;
                let (name_of_callable, status) = self.call_return_value_status(index, call)?;
                Some(Discarded {
                    name: name_of_callable,
                    status,
                    span: self.member_name_span(expression, name),
                })
            }
            Expr::Name(_) | Expr::Member { .. } | Expr::SafeCall { args: None, .. } => {
                let (name, status) = self.property_return_value_status(index, expression)?;
                let span = match self.file.expr(expression) {
                    Expr::Member { name, .. } | Expr::SafeCall { name, .. } => {
                        self.member_name_span(expression, name)
                    }
                    _ => self.span(expression),
                };
                Some(Discarded { name, status, span })
            }
            _ => None,
        }
    }

    /// The name and return value status of the callable `call` selected.
    fn call_return_value_status(
        &self,
        index: &crate::fir::ResolvedModuleIndex,
        call: &ResolvedCall,
    ) -> Option<(String, ReturnValueStatus)> {
        if let Some(declaration) = super::deprecation_usage::call_declaration(call) {
            let callable = index.callable_for_declaration(declaration)?;
            return Some((
                index.declaration_name(declaration)?.to_string(),
                index.callable_inherited_status(callable.id).return_value,
            ));
        }
        let (name, status) = match call {
            ResolvedCall::Member(selected) => {
                (&selected.member.name, selected.member.return_value_status)
            }
            ResolvedCall::Companion(member) => (&member.name, member.return_value_status),
            ResolvedCall::TopLevel(call) => {
                (&call.callable.name, call.callable.return_value_status)
            }
            ResolvedCall::Extension(call) => {
                (&call.callable.name, call.callable.return_value_status)
            }
            ResolvedCall::MemberExtension { .. } | ResolvedCall::LocalFunction(_) => return None,
        };
        Some((name.clone(), status?))
    }

    /// The name and return value status of the property `expression` reads.
    fn property_return_value_status(
        &self,
        index: &crate::fir::ResolvedModuleIndex,
        expression: ExprId,
    ) -> Option<(String, ReturnValueStatus)> {
        let lowering = self.expr_lowers.get(&expression)?;
        let (declaration, getter, name) = match lowering {
            ExprLowering::MemberPropertyRead {
                stable_declaration,
                accessor,
                name,
                ..
            } => (
                *stable_declaration,
                accessor
                    .as_deref()
                    .and_then(|accessor| accessor.return_value_status),
                name,
            ),
            ExprLowering::TopLevelPropertyGet(access) => (
                access.property.stable_declaration,
                access.property.return_value_status,
                &access.property.name,
            ),
            _ => return None,
        };
        match declaration {
            Some(declaration) => {
                let property = index.property_for_declaration(declaration)?;
                Some((
                    index.declaration_name(declaration)?.to_string(),
                    index.property_return_value_status(property),
                ))
            }
            None => Some((name.clone(), getter?)),
        }
    }
}

/// Whether a value of `ty` is of a type nobody uses: `Unit`, `Nothing` or an erroneous type, or a
/// non-null `java.lang.Void`. A nullable `Unit?` or `Void?` carries information and is not.
fn ignorable_type(ty: Ty) -> bool {
    ty == Ty::Error
        || ty == Ty::Nothing
        || ty == Ty::Unit
        || (!ty.is_nullable() && ty.obj_internal() == Some(crate::types::wk::java_void()))
}

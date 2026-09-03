//! Checked assignment convention calls.

use super::*;
use crate::resolve::CompoundAssignmentTarget;

impl BodyFirChecker<'_> {
    pub(super) fn compound_assignment_statement(
        &mut self,
        statement: StmtId,
        target: CompoundAssignmentTarget,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        let (receiver_source, argument_source) = match self.file.stmt(statement) {
            Stmt::Assign { value, .. }
            | Stmt::AssignMember { value, .. }
            | Stmt::AssignIndex { value, .. } => {
                let Expr::Binary { lhs, rhs, .. } = self.file.expr(*value) else {
                    return Err(self.failure(
                        self.file.stmt_spans.get(statement.0 as usize).copied(),
                        BodyCheckFailureKind::UnsupportedStatement(StatementForm::CompoundAssign),
                    ));
                };
                (*lhs, *rhs)
            }
            Stmt::CompoundAssign { target, value, .. } => (*target, *value),
            Stmt::Local { .. }
            | Stmt::LocalLateinit { .. }
            | Stmt::LocalDelegate { .. }
            | Stmt::Destructure { .. }
            | Stmt::IncDec { .. }
            | Stmt::Return(_, _)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::While { .. }
            | Stmt::DoWhile { .. }
            | Stmt::For { .. }
            | Stmt::ForEach { .. }
            | Stmt::Expr(_)
            | Stmt::LocalFun(_)
            | Stmt::LocalClass(_)
            | Stmt::LocalTypeAlias(_) => {
                return Err(self.failure(
                    self.file.stmt_spans.get(statement.0 as usize).copied(),
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::CompoundAssign),
                ));
            }
        };
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let receiver = self.expression(receiver_source)?;
        if let crate::resolve::ResolvedCall::LocalFunction(selected) = target.call.as_ref() {
            let call = self.local_operator_call_on_value(
                span,
                origin,
                selected,
                receiver,
                std::slice::from_ref(&argument_source),
            )?;
            let call = self.body.add_expr(FirExpr {
                origin,
                ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                kind: call,
            });
            return Ok(self.body.add_statement(FirStatement {
                origin,
                kind: FirStatementKind::Expression(call),
            }));
        }
        // Provider origin is linkage only: the selected `plusAssign`/`plus` may be declared in this
        // module or on the classpath (`MutableList.plusAssign`), and both must produce a checked
        // call. Routing through the shared operator-target mapping is what keeps a dependency
        // operator from being reported as a missing STABLE target — it never had one.
        let selected = self.selected_call_target(span, Some(target.call.as_ref()))?;
        if selected.vararg_index.is_some() || selected.value_parameters.len() != 1 {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        }
        let parameter_types = selected.parameter_types();
        let cause = self.statement_origin(statement)?;
        let mut arguments = selected
            .context_arguments
            .iter()
            .enumerate()
            .map(|(parameter, argument)| {
                let receiver = self.materialize_context_argument(
                    receiver_source,
                    cause,
                    argument.as_ref().ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?,
                )?;
                Ok(FirCallArgument::Expression {
                    parameter: u32::try_from(parameter).map_err(|_| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?,
                    value: receiver.value,
                    conversion: receiver.conversion,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        let argument = self.expression(argument_source)?;
        let conversion =
            self.selected_value_conversion(argument_source, selected.value_parameters[0], cause)?;
        arguments.push(FirCallArgument::Expression {
            parameter: u32::try_from(selected.context_arguments.len())
                .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?,
            value: argument,
            conversion,
        });
        let bound = FirReceiver {
            value: receiver,
            conversion: None,
        };
        let call = self.body.add_expr(FirExpr {
            origin,
            ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
            kind: FirExprKind::Call(FirCall {
                target: selected.target,
                dispatch_receiver: (!selected.extension).then_some(bound),
                extension_receiver: selected.extension.then_some(bound),
                parameter_types,
                arguments: arguments.into_boxed_slice(),
                substitutions: Box::new([]),
            }),
        });
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(call),
        }))
    }
}

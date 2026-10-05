//! Checked assignment convention calls.

use super::*;
use crate::resolve::CompoundAssignmentTarget;

/// How `receiver.name op= value` evaluates its receiver once.
pub(super) enum CompoundMemberReceiver {
    /// The receiver is saved in a local declared by this statement.
    Snapshot(FirStatementId),
    /// The receiver is a read of an immutable binding, repeated for the get and the set.
    StableRead,
}

impl BodyFirChecker<'_> {
    pub(super) fn compound_assignment_statement(
        &mut self,
        statement: StmtId,
        target: CompoundAssignmentTarget,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        let safe_receiver = match self.file.stmt(statement) {
            Stmt::AssignSafeIndex { receiver, .. } => Some(*receiver),
            Stmt::AssignMember {
                receiver,
                safe: true,
                ..
            } => Some(*receiver),
            _ => None,
        };
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
            Stmt::AssignSafeIndex { value, .. } => {
                let Expr::Binary { lhs, rhs, .. } = self.file.expr(*value) else {
                    return Err(self.failure(
                        self.file.stmt_spans.get(statement.0 as usize).copied(),
                        BodyCheckFailureKind::UnsupportedStatement(StatementForm::CompoundAssign),
                    ));
                };
                (*lhs, *rhs)
            }
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
        let call = if let Some(receiver) = safe_receiver {
            let kind = self.guard_safe_index(
                receiver,
                origin,
                ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                |checker| {
                    checker.in_place_assignment_expression(
                        statement,
                        receiver_source,
                        argument_source,
                        target,
                        origin,
                    )
                },
            )?;
            self.body.add_expr(FirExpr {
                origin,
                ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                kind,
            })
        } else {
            self.in_place_assignment_expression(
                statement,
                receiver_source,
                argument_source,
                target,
                origin,
            )?
        };
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(call),
        }))
    }

    pub(super) fn in_place_assignment_expression(
        &mut self,
        statement: StmtId,
        receiver_source: ExprId,
        argument_source: ExprId,
        target: CompoundAssignmentTarget,
        origin: OriginId,
    ) -> Result<FirExprId, BodyCheckFailure> {
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
            return Ok(self.body.add_expr(FirExpr {
                origin,
                ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                kind: call,
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
        let conversion = self.selected_value_conversion(
            argument_source,
            argument,
            selected.value_parameters[0],
            cause,
        )?;
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
        Ok(self.body.add_expr(FirExpr {
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
        }))
    }

    /// Bind the receiver of `receiver.name op= value` once, before the read inside `value`.
    ///
    /// An enclosing access increment has already substituted that receiver for every access in
    /// the block, including a prefix re-read after this write. A fresh local whose substitution
    /// is dropped at the end of the write would evaluate the receiver again on that re-read.
    ///
    /// A read of an immutable binding is already its own snapshot: kotlinc's assignment desugaring
    /// reuses an immutable `IrGetValue` receiver for both the get and the set instead of saving it
    /// in a temporary, so the checked read itself becomes the substitution.
    pub(super) fn bind_compound_member_receiver(
        &mut self,
        receiver: ExprId,
        name: &str,
        value: ExprId,
        origin: OriginId,
    ) -> Result<Option<CompoundMemberReceiver>, BodyCheckFailure> {
        if self.expression_substitutions.contains_key(&receiver)
            || !self.is_compound_member_assignment(receiver, name, value)
        {
            return Ok(None);
        }
        let initializer = self.expression(receiver)?;
        if self.is_immutable_binding_read(initializer) {
            self.expression_substitutions.insert(receiver, initializer);
            return Ok(Some(CompoundMemberReceiver::StableRead));
        }
        let ty = self.expression_type(receiver)?;
        let target = self.allocate_local();
        let declaration = self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Local {
                target,
                ty,
                mutable: false,
                lateinit: false,
                deferred: false,
                initializer: Some(initializer),
                conversion: None,
            },
        });
        let replacement = self.body.add_expr(FirExpr {
            origin,
            ty,
            kind: FirExprKind::ValueRead(target),
        });
        self.expression_substitutions.insert(receiver, replacement);
        Ok(Some(CompoundMemberReceiver::Snapshot(declaration)))
    }

    /// Whether `expression` is a plain read of a lexical `val`, a parameter, or a `this` receiver
    /// of this body or of an enclosing one: a value no evaluation between two reads can change.
    ///
    /// A `val` a local classifier or a suspend lambda captured is not one: kotlinc's temporary
    /// survives once local declaration lowering has turned that read into a capture-field read,
    /// while a receiver is never saved in a temporary at all.
    fn is_immutable_binding_read(&self, expression: FirExprId) -> bool {
        let immutable = |binding: &LocalBinding| !binding.mutable && !binding.lateinit;
        match self
            .body
            .expr(expression)
            .map(|expression| &expression.kind)
        {
            Some(
                FirExprKind::ImplicitReceiver { .. }
                | FirExprKind::EnclosingReceiver { .. }
                | FirExprKind::CapturedImplicitReceiver { .. },
            ) => true,
            // A local classifier's captured `this@Outer` is still that receiver, not a value.
            Some(
                FirExprKind::ClassStorageRead { owner, field }
                | FirExprKind::EnclosingClassStorageRead {
                    owner,
                    field,
                    shared_cell: false,
                    ..
                },
            ) => self
                .class_receivers
                .iter()
                .any(|binding| binding.owner == *owner && binding.field == *field),
            Some(FirExprKind::ValueRead(value)) => self
                .scopes
                .iter()
                .flat_map(|scope| scope.values())
                .any(|binding| binding.value == *value && immutable(binding)),
            // A callable lifted to a method receives its captures as parameters; a suspend lambda
            // is a class of its own and reads them from capture fields instead.
            Some(FirExprKind::CapturedValueRead {
                enclosing_depth,
                source,
            }) => {
                self.body.lifting_site().is_none_or(|site| site.lifted)
                    && self.outer_values.values().any(|(depth, binding)| {
                        depth == enclosing_depth && binding.value == *source && immutable(binding)
                    })
            }
            _ => false,
        }
    }

    /// The parser represents `receiver.name op= rhs` as a write whose value contains the matching
    /// read and deliberately reuses the same receiver expression identity.
    fn is_compound_member_assignment(&self, receiver: ExprId, name: &str, value: ExprId) -> bool {
        let Expr::Binary { lhs, .. } = self.file.expr(value) else {
            return false;
        };
        matches!(
            self.file.expr(*lhs),
            Expr::Member {
                receiver: read_receiver,
                name: read_name,
            } if *read_receiver == receiver && read_name == name
        )
    }
}

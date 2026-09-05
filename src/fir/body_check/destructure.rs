//! Checked FIR for local destructuring declarations.

use super::*;
use crate::resolve::ResolvedCall;

impl BodyFirChecker<'_> {
    pub(super) fn destructure_statement(
        &mut self,
        statement: StmtId,
        entries: &[crate::ast::DestructureEntry],
        initializer_source: ExprId,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        let initializer = self.expression(initializer_source)?;
        let mut checked = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            if entry.ignored {
                checked.push(FirDestructureEntry::Ignored { origin });
                continue;
            }
            let name = &entry.name;

            let target = self
                .info
                .resolved_destructure_component(statement, index)
                .cloned()
                .ok_or_else(|| {
                    self.failure(
                        self.file.stmt_spans.get(statement.0 as usize).copied(),
                        BodyCheckFailureKind::UnsupportedStatement(StatementForm::Destructure),
                    )
                })?;
            let component_ty = self.resolved_type(
                self.file
                    .stmt_spans
                    .get(statement.0 as usize)
                    .copied()
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
                target.ret(),
            )?;
            let component_kind = self.destructure_component_call(
                statement,
                initializer_source,
                initializer,
                *target,
                origin,
            )?;
            let component = self.body.add_expr(FirExpr {
                origin,
                ty: component_ty,
                kind: component_kind,
            });
            let binding_ty = self
                .file
                .destructure_entry_types
                .get(&statement.0)
                .and_then(|types| types.get(index))
                .and_then(Option::as_ref)
                .map(|annotation| {
                    let ty = self.info.resolved_type(annotation).ok_or_else(|| {
                        self.failure(
                            Some(annotation.span),
                            BodyCheckFailureKind::UnresolvedTypeSyntax,
                        )
                    })?;
                    self.resolved_type(annotation.span, ty)
                })
                .transpose()?
                .unwrap_or(component_ty);
            let conversion = self.selected_type_conversion(component_ty, binding_ty, origin);
            let target = self.bind_local(name, binding_ty);
            checked.push(FirDestructureEntry::Binding {
                origin,
                target,
                ty: binding_ty,
                mutable: entry.mutable,
                component,
                conversion,
            });
        }

        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Destructure {
                initializer,
                entries: checked.into_boxed_slice(),
            },
        }))
    }

    fn destructure_component_call(
        &mut self,
        statement: StmtId,
        receiver_source: ExprId,
        receiver: FirExprId,
        selected: ResolvedCall,
        origin: OriginId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let resolved = |ty| {
            ResolvedTy::new(ty).map_err(|error| BodyCheckFailure {
                span,
                kind: BodyCheckFailureKind::UnpublishableType(error),
            })
        };
        crate::trace_compiler!(
            "fir",
            "checked destructure component statement={statement:?} selected={selected:?}",
        );
        let (target, dispatch_receiver, extension_receiver) = match selected {
            ResolvedCall::Member(member) => {
                let target = if let Some(declaration) = member.member.stable_declaration {
                    if let Some(property) = self.index.property_for_declaration(declaration) {
                        return Ok(FirExprKind::PropertyRead {
                            target: FirPropertyTarget::Module(property),
                            dispatch_receiver: Some(self.destructure_receiver(
                                receiver_source,
                                receiver,
                                member.receiver,
                                origin,
                            )?),
                            extension_receiver: None,
                            context_arguments: Box::new([]),
                            substitutions: Box::new([]),
                        });
                    }
                    self.index
                        .callable_for_declaration(declaration)
                        .map(|callable| FirCallTarget::Module(callable.id))
                        .ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?
                } else {
                    let declaration = member.member.external_identity.ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                    })?;
                    FirCallTarget::External {
                        declaration,
                        receiver: Some(resolved(member.receiver)?),
                        declared_receiver: None,
                        parameters: member
                            .member
                            .params
                            .iter()
                            .copied()
                            .map(resolved)
                            .collect::<Result<Vec<_>, _>>()?
                            .into_boxed_slice(),
                        result: resolved(member.ret)?,
                        declared_result: member.member.declared_ret.map(resolved).transpose()?,
                        suspend: member.member.suspend(),
                        can_inline: member.member.inline.can_inline(),
                        inline_plan: super::calls::fir_inline_body_plan(
                            member.member.inline_body_plan.as_deref(),
                            None,
                        ),
                        extension_receiver_parameter: None,
                    }
                };
                (
                    target,
                    Some(self.destructure_receiver(
                        receiver_source,
                        receiver,
                        member.receiver,
                        origin,
                    )?),
                    None,
                )
            }
            ResolvedCall::Extension(extension)
                if extension.context_args.is_empty() && extension.vararg_index.is_none() =>
            {
                let target = if let Some(declaration) = extension.stable_declaration {
                    self.index
                        .callable_for_declaration(declaration)
                        .map(|callable| FirCallTarget::Module(callable.id))
                        .ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?
                } else {
                    let callable = &extension.callable;
                    FirCallTarget::External {
                        declaration: callable.external_identity.ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?,
                        receiver: Some(resolved(extension.receiver)?),
                        declared_receiver: callable.source_receiver.map(resolved).transpose()?,
                        parameters: extension
                            .params
                            .iter()
                            .copied()
                            .map(resolved)
                            .collect::<Result<Vec<_>, _>>()?
                            .into_boxed_slice(),
                        result: resolved(callable.ret)?,
                        declared_result: callable.declared_ret.map(resolved).transpose()?,
                        suspend: callable.suspend,
                        can_inline: callable.inline.can_inline(),
                        inline_plan: super::calls::fir_inline_body_plan(
                            callable.inline_body_plan.as_deref(),
                            Some(0),
                        ),
                        extension_receiver_parameter: None,
                    }
                };
                (
                    target,
                    None,
                    Some(self.destructure_receiver(
                        receiver_source,
                        receiver,
                        extension.receiver,
                        origin,
                    )?),
                )
            }
            ResolvedCall::MemberExtension {
                stable_declaration,
                external_identity,
                dispatch_receiver,
                extension_receiver: declared_extension_receiver,
                params,
                context_args,
                ret,
                inline,
                inline_body_plan,
                suspend,
                declared_ret,
                vararg_index,
                ..
            } if context_args.is_empty() && vararg_index.is_none() => {
                let origin = self.statement_origin(statement)?;
                let dispatch_ty = dispatch_receiver.ty;
                let dispatch_receiver = self
                    .materialize_implicit_receiver(origin, span, &dispatch_receiver)?
                    .ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?;
                let target = if let Some(declaration) = stable_declaration {
                    self.index
                        .callable_for_declaration(declaration)
                        .map(|callable| FirCallTarget::Module(callable.id))
                        .ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?
                } else {
                    let declaration = external_identity.ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                    })?;
                    let mut parameters = params.clone();
                    parameters.insert(0, declared_extension_receiver);
                    FirCallTarget::External {
                        declaration,
                        receiver: Some(resolved(dispatch_ty)?),
                        declared_receiver: None,
                        parameters: parameters
                            .into_iter()
                            .map(resolved)
                            .collect::<Result<Vec<_>, _>>()?
                            .into_boxed_slice(),
                        result: resolved(ret)?,
                        declared_result: declared_ret.map(resolved).transpose()?,
                        suspend,
                        can_inline: inline.can_inline(),
                        inline_plan: super::calls::fir_inline_body_plan(
                            inline_body_plan.as_deref(),
                            None,
                        ),
                        extension_receiver_parameter: Some(0),
                    }
                };
                (
                    target,
                    Some(dispatch_receiver),
                    Some(self.destructure_receiver(
                        receiver_source,
                        receiver,
                        declared_extension_receiver,
                        origin,
                    )?),
                )
            }
            ResolvedCall::LocalFunction(local)
                if local.context_args.is_empty()
                    && local.sig.vararg_index.is_none()
                    && local
                        .sig
                        .params
                        .get(local.sig.context_count.min(local.sig.params.len())..)
                        .is_some_and(<[Ty]>::is_empty) =>
            {
                let target = self
                    .local_callable_ref(local.stmt_id, origin)?
                    .ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                    })?;
                let expected_receiver = local.sig.source_receiver.ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                return Ok(FirExprKind::LocalCall {
                    target,
                    extension_receiver: Some(self.destructure_receiver(
                        receiver_source,
                        receiver,
                        expected_receiver,
                        origin,
                    )?),
                    arguments: Box::new([]),
                });
            }
            ResolvedCall::TopLevel(_)
            | ResolvedCall::Companion(_)
            | ResolvedCall::LocalFunction(_)
            | ResolvedCall::Extension(_)
            | ResolvedCall::MemberExtension { .. } => {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            }
        };
        Ok(FirExprKind::Call(FirCall {
            target,
            dispatch_receiver,
            extension_receiver,
            parameter_types: Box::new([]),
            arguments: Box::new([]),
            substitutions: Box::new([]),
        }))
    }

    /// Publish the exact receiver view selected by semantic destructuring resolution. A flow
    /// intersection has no synthetic runtime classifier: each component call projects the shared
    /// initializer onto the constituent that owns that operator, and FIR must retain that cast so
    /// lowering never has to rediscover the intersection or member owner.
    fn destructure_receiver(
        &self,
        source: ExprId,
        value: FirExprId,
        target: Ty,
        origin: OriginId,
    ) -> Result<FirReceiver, BodyCheckFailure> {
        let actual = self.expression_type(source)?;
        let target = ResolvedTy::new(target).map_err(|error| {
            self.failure(
                self.file.expr_span(source),
                BodyCheckFailureKind::UnpublishableType(error),
            )
        })?;
        let conversion = self
            .selected_type_conversion(actual, target, origin)
            .or_else(|| {
                (actual != target).then_some(FirConversion {
                    origin,
                    kind: FirConversionKind::SmartCast { to: target },
                })
            });
        Ok(FirReceiver { value, conversion })
    }
}

//! Translation of checker-selected source property accesses into stable checked FIR nodes.

use super::*;
use crate::fir::PropertyId;

struct ExternalPropertyTarget {
    property: crate::fir::ExternalPropertyId,
    receiver: Option<Ty>,
    parameters: Vec<Ty>,
    result: Ty,
    extension_receiver_parameter: Option<u32>,
}

impl BodyFirChecker<'_> {
    /// Preserve an already-selected dependency property reached through `super` as a PROPERTY
    /// operation. The provider identity remains opaque; only the non-virtual dispatch fact crosses
    /// FIR/common IR. Source-module properties retain their existing stable accessor-call path
    /// until their property declaration identity is available on the selected accessor.
    pub(super) fn selected_super_property_read(
        &mut self,
        expression: ExprId,
        target: &crate::resolve::ResolvedSuperCall,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let Some(property) = target.external_property else {
            return self.selected_super_call(expression, &[], target);
        };
        let span = self.file.expr_span(expression);
        let cause = self.expression_origin(expression)?;
        if !target.params.is_empty() {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        }
        let dispatch_receiver = self
            .materialize_implicit_receiver(cause, span, &target.receiver)?
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        Ok(FirExprKind::PropertyRead {
            target: FirPropertyTarget::External {
                property,
                receiver: Some(resolved(target.receiver.ty)?),
                parameters: Box::new([]),
                result: resolved(target.ret)?,
                extension_receiver_parameter: None,
                dispatch: crate::fir::FirPropertyDispatch::Super {
                    owner: target.owner,
                    interface: target.interface,
                },
            },
            dispatch_receiver: Some(dispatch_receiver),
            extension_receiver: None,
            context_arguments: Box::new([]),
            substitutions: Box::new([]),
        })
    }

    /// Write counterpart of [`Self::selected_super_property_read`].
    pub(super) fn selected_super_property_write(
        &mut self,
        span: Option<Span>,
        cause: OriginId,
        value: ExprId,
        target: &crate::resolve::ResolvedSuperCall,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let Some(property) = target.external_property else {
            return self.selected_super_call_at(
                span,
                cause,
                None,
                std::slice::from_ref(&value),
                target,
            );
        };
        let [value_type] = target.params.as_slice() else {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        };
        let dispatch_receiver = self
            .materialize_implicit_receiver(cause, span, &target.receiver)?
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        let value_type = resolved(*value_type)?;
        Ok(FirExprKind::PropertyWrite {
            target: FirPropertyTarget::External {
                property,
                receiver: Some(resolved(target.receiver.ty)?),
                parameters: Box::new([value_type]),
                result: resolved(target.ret)?,
                extension_receiver_parameter: None,
                dispatch: crate::fir::FirPropertyDispatch::Super {
                    owner: target.owner,
                    interface: target.interface,
                },
            },
            dispatch_receiver: Some(dispatch_receiver),
            extension_receiver: None,
            context_arguments: Box::new([]),
            value: self.expression(value)?,
            conversion: self.selected_value_conversion(value, value_type, cause)?,
            substitutions: Box::new([]),
        })
    }

    fn external_property_getter(
        property: &crate::libraries::PropertyInfo,
        extension: bool,
    ) -> Option<ExternalPropertyTarget> {
        let getter = &property.getter;
        let identity = getter.external_property_identity?;
        let mut parameters = getter.params.clone();
        let (receiver, extension_receiver_parameter) = if extension {
            let receiver_position = property.context_count.min(parameters.len());
            if receiver_position >= parameters.len() {
                return None;
            }
            if let Some(singleton) = getter.singleton_dispatch.as_deref() {
                (
                    Some(singleton.ty()),
                    Some(u32::try_from(receiver_position).ok()?),
                )
            } else {
                parameters.remove(receiver_position);
                (
                    (!property.is_companion_extension())
                        .then_some(property.receiver)
                        .flatten(),
                    None,
                )
            }
        } else {
            (None, None)
        };
        Some(ExternalPropertyTarget {
            property: identity,
            receiver,
            parameters,
            result: property.ty,
            extension_receiver_parameter,
        })
    }

    fn external_member_getter(
        getter: &crate::symbol_resolver::ResolvedMember,
    ) -> Option<ExternalPropertyTarget> {
        Some(ExternalPropertyTarget {
            property: getter.member.external_property_identity?,
            receiver: Some(getter.receiver),
            parameters: getter.member.params.clone(),
            result: getter.ret,
            extension_receiver_parameter: None,
        })
    }

    fn external_member_setter(
        receiver: Ty,
        setter: &crate::libraries::LibraryCallable,
    ) -> Option<ExternalPropertyTarget> {
        Some(ExternalPropertyTarget {
            property: setter.external_property_identity?,
            receiver: Some(receiver),
            parameters: setter.params.clone(),
            result: setter.ret,
            extension_receiver_parameter: None,
        })
    }

    fn external_property_setter(
        property: &crate::libraries::PropertyInfo,
        extension: bool,
    ) -> Option<ExternalPropertyTarget> {
        let setter = property.setter.as_ref()?;
        let identity = setter.external_property_identity?;
        let mut parameters = setter.params.clone();
        let (receiver, extension_receiver_parameter) = if extension {
            let receiver_position = property.context_count.min(parameters.len());
            if receiver_position >= parameters.len() {
                return None;
            }
            if let Some(singleton) = setter.singleton_dispatch.as_deref() {
                (
                    Some(singleton.ty()),
                    Some(u32::try_from(receiver_position).ok()?),
                )
            } else {
                parameters.remove(receiver_position);
                (
                    (!property.is_companion_extension())
                        .then_some(property.receiver)
                        .flatten(),
                    None,
                )
            }
        } else {
            (None, None)
        };
        Some(ExternalPropertyTarget {
            property: identity,
            receiver,
            parameters,
            result: setter.ret,
            extension_receiver_parameter,
        })
    }

    pub(super) fn enclosing_property_id(&self) -> Option<PropertyId> {
        if let Some(property) = self.enclosing_property {
            return Some(property);
        }
        let mut declaration = DeclarationId::from_raw(self.body.owner().raw());
        loop {
            if let Some(property) = self.index.property_for_declaration(declaration) {
                return Some(property);
            }
            declaration = self.index.declaration_anchor(declaration)?.owner?;
        }
    }

    pub(super) fn enclosing_property(
        &self,
        expression: ExprId,
    ) -> Result<PropertyId, BodyCheckFailure> {
        self.enclosing_property_id().ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStablePropertyTarget,
            )
        })
    }

    pub(super) fn enclosing_property_for_statement(
        &self,
        statement: StmtId,
    ) -> Result<PropertyId, BodyCheckFailure> {
        self.enclosing_property_id().ok_or_else(|| {
            self.failure(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                BodyCheckFailureKind::MissingStablePropertyTarget,
            )
        })
    }

    /// The stable property of the ENCLOSING classifier with this source name. An `init` block that
    /// assigns a deferred `val` is not inside that property's own body, so the enclosing-property
    /// coordinate does not apply: the target has to be looked up by name on the owner.
    pub(super) fn classifier_property_named(&self, name: &str) -> Option<PropertyId> {
        let owner = self
            .index
            .declaration_anchor(crate::fir::DeclarationId::from_raw(self.body.owner().raw()))
            .and_then(|anchor| anchor.owner)?;
        let header = self.index.declaration_header(owner)?;
        if header.kind != crate::fir::DeclarationKind::Classifier {
            return None;
        }
        (0..self.index.declaration_count()).find_map(|raw| {
            let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
            let candidate = self.index.declaration_header(declaration)?;
            (candidate.owner == Some(owner)
                && candidate.kind == crate::fir::DeclarationKind::Property
                && self.index.declaration_name(declaration) == Some(name))
            .then(|| self.index.property_for_declaration(declaration))?
        })
    }

    /// The checker has already selected an enum-entry property by its owner-local structural
    /// ordinal. Convert that transient coordinate to the stable module property identity before
    /// publishing FIR; neither common lowering nor the backend sees the ordinal or source name.
    pub(super) fn enum_entry_property(&self, sibling: u32) -> Option<PropertyId> {
        let mut owner = crate::fir::DeclarationId::from_raw(self.body.owner().raw());
        loop {
            let anchor = self.index.declaration_anchor(owner)?;
            if anchor.kind == crate::fir::DeclarationKind::EnumEntry {
                break;
            }
            owner = anchor.owner?;
        }
        let declaration =
            self.index
                .owned_declaration(owner, crate::fir::DeclarationKind::Property, sibling)?;
        self.index.property_for_declaration(declaration)
    }

    pub(super) fn implicit_receiver(
        &mut self,
        cause_expression: ExprId,
    ) -> Result<Option<FirReceiver>, BodyCheckFailure> {
        let Some(selected) = self
            .info
            .implicit_receiver_selections
            .get(&cause_expression)
            .cloned()
        else {
            return Ok(None);
        };
        let cause = self.expression_origin(cause_expression)?;
        let mut receiver = self.materialize_implicit_receiver(
            cause,
            self.file.expr_span(cause_expression),
            &selected,
        )?;
        if let (Some(receiver), Some(owner)) = (
            receiver.as_mut(),
            self.info
                .narrowed_this_member
                .get(&cause_expression)
                .copied(),
        ) {
            let to = ResolvedTy::new(Ty::obj_name(owner)).map_err(|error| {
                self.failure(
                    self.file.expr_span(cause_expression),
                    BodyCheckFailureKind::UnpublishableType(error),
                )
            })?;
            receiver.conversion = Some(FirConversion {
                origin: cause,
                kind: FirConversionKind::SmartCast { to },
            });
        }
        Ok(receiver)
    }

    pub(super) fn materialize_implicit_receiver(
        &mut self,
        cause: OriginId,
        span: Option<Span>,
        selected: &crate::resolve::ImplicitReceiverSelection,
    ) -> Result<Option<FirReceiver>, BodyCheckFailure> {
        let origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::ImplicitReceiver);
        crate::trace_compiler!(
            "fir",
            "materialize implicit receiver body={:?} origin={origin:?} selection={selected:?} owned_receivers={}",
            self.body.owner(),
            self.owned_receiver_count,
        );
        let selected_ty = ResolvedTy::new(selected.ty)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        if let Some((owner, field)) = self.class_context_storage(selected) {
            let value = self.body.add_expr(FirExpr {
                origin,
                ty: selected_ty,
                kind: if self.constructor_prefix_capture_access {
                    FirExprKind::ConstructorContextRead {
                        owner,
                        parameter: field,
                    }
                } else {
                    FirExprKind::ClassStorageRead { owner, field }
                },
            });
            return Ok(Some(FirReceiver {
                value,
                conversion: None,
            }));
        }
        if let Some((name, shadow_depth)) = selected
            .context_binding
            .as_ref()
            .filter(|(name, _)| name != "this")
        {
            let (enclosing_depth, binding) = self
                .binding_source_at_shadow_depth(name, *shadow_depth)
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnknownLocal))?;
            let kind = if let Some(enclosing_depth) = enclosing_depth {
                self.body.add_capture(FirCapture {
                    origin,
                    enclosing_depth,
                    source: binding.value,
                    ty: binding.ty,
                    shared_cell: false,
                });
                FirExprKind::CapturedValueRead {
                    enclosing_depth,
                    source: binding.value,
                }
            } else {
                FirExprKind::ValueRead(binding.value)
            };
            let value = self.body.add_expr(FirExpr {
                origin,
                ty: binding.ty,
                kind,
            });
            return Ok(Some(FirReceiver {
                value,
                conversion: None,
            }));
        }
        if let Some(singleton) = selected.singleton.as_ref() {
            let ty = ResolvedTy::new(selected.ty).map_err(|error| {
                self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
            })?;
            let value = self.body.add_expr(FirExpr {
                origin,
                ty,
                kind: FirExprKind::SingletonValue {
                    classifier: singleton.classifier,
                },
            });
            return Ok(Some(FirReceiver {
                value,
                conversion: None,
            }));
        }
        if !selected.current {
            if let Some(receiver) = self.local_class_enclosing_receiver(origin, selected)? {
                return Ok(Some(receiver));
            }
        }
        let depth = u32::try_from(selected.receiver_depth)
            .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        if self.body.local_callable().is_some()
            && selected.receiver_depth >= self.owned_receiver_count as usize
        {
            let (enclosing_depth, captured_depth, path) = self
                .enclosing_receiver_capture(selected.receiver_depth)
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
            self.body
                .add_implicit_receiver_capture(FirImplicitReceiverCapture {
                    origin,
                    enclosing_depth,
                    current: captured_depth == 0,
                    depth: captured_depth,
                    path: path.clone(),
                    ty: selected_ty,
                });
            let value = self.body.add_expr(FirExpr {
                origin,
                ty: selected_ty,
                kind: FirExprKind::CapturedImplicitReceiver {
                    enclosing_depth,
                    current: captured_depth == 0,
                    depth: captured_depth,
                    path,
                },
            });
            return Ok(Some(FirReceiver {
                value,
                conversion: None,
            }));
        }
        if let Some(path) = self.enclosing_receiver_path(selected) {
            let value = self.body.add_expr(FirExpr {
                origin,
                ty: selected_ty,
                kind: FirExprKind::EnclosingReceiver { path },
            });
            return Ok(Some(FirReceiver {
                value,
                conversion: None,
            }));
        }
        let ty = ResolvedTy::new(selected.ty)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        let value = self.body.add_expr(FirExpr {
            origin,
            ty,
            kind: FirExprKind::ImplicitReceiver {
                current: selected.current,
                depth,
            },
        });
        Ok(Some(FirReceiver {
            value,
            conversion: None,
        }))
    }

    fn class_context_storage(
        &self,
        selected: &crate::resolve::ImplicitReceiverSelection,
    ) -> Option<(DeclarationId, u32)> {
        let owner = self.current_storage_owner()?;
        let classifier = self.index.classifier_header(owner)?;
        if classifier.context_parameters.is_empty() {
            return None;
        }
        let declaration = DeclarationId::from_raw(self.body.owner().raw());
        let direct_receivers = usize::from(self.index.enclosing_classifier(declaration).is_some())
            .checked_add(self.body.context_receiver_types().len())?
            .checked_add(usize::from(self.body.receiver_type().is_some()))?;
        let reverse_ordinal = selected.receiver_depth.checked_sub(direct_receivers)?;
        let ordinal = classifier
            .context_parameters
            .len()
            .checked_sub(reverse_ordinal.checked_add(1)?)?;
        let parameter = classifier.context_parameters.get(ordinal)?;
        (parameter.ty.get() == selected.ty).then(|| {
            (
                owner,
                u32::try_from(ordinal).expect("too many classifier context parameters"),
            )
        })
    }

    pub(super) fn source_property_read(
        &mut self,
        expression: ExprId,
        receiver: Option<ExprId>,
    ) -> Result<Option<FirExprKind>, BodyCheckFailure> {
        let selected = self.info.expr_lowers.get(&expression).cloned();
        let resolved_member = self.info.resolved_member(expression).cloned();
        self.source_property_read_selected(expression, receiver, selected, resolved_member)
    }

    pub(super) fn source_property_read_selected(
        &mut self,
        expression: ExprId,
        receiver: Option<ExprId>,
        selected: Option<ExprLowering>,
        resolved_member: Option<crate::symbol_resolver::ResolvedMember>,
    ) -> Result<Option<FirExprKind>, BodyCheckFailure> {
        if let Some(ExprLowering::ClassifierPropertyRead { owner, property }) = &selected {
            let property = match property.operation {
                crate::libraries::ImplicitClassifierProperty::EnumEntries => {
                    FirClassifierProperty::EnumEntries
                }
            };
            return Ok(Some(FirExprKind::ClassifierPropertyRead {
                owner: *owner,
                property,
            }));
        }
        if let Some(ExprLowering::EnumEntryPropertyRead { sibling }) = selected.as_ref() {
            let target = self.enum_entry_property(*sibling).ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::MissingStablePropertyTarget,
                )
            })?;
            let dispatch_receiver = self.implicit_receiver(expression)?.ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
            return Ok(Some(FirExprKind::PropertyRead {
                target: FirPropertyTarget::Module(target),
                dispatch_receiver: Some(dispatch_receiver),
                extension_receiver: None,
                context_arguments: Box::new([]),
                substitutions: Box::new([]),
            }));
        }
        let (declaration, external, dispatch_receiver, mut extension_receiver, context_args) =
            match selected {
                Some(ExprLowering::TopLevelPropertyGet(access)) => {
                    if access.property.getter.compiler_intrinsic
                        == Some(crate::libraries::CompilerIntrinsic::CoroutineContext)
                    {
                        let result = ResolvedTy::new(access.property.ty).map_err(|error| {
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnpublishableType(error),
                            )
                        })?;
                        return Ok(Some(FirExprKind::Call(FirCall {
                            target: FirCallTarget::Intrinsic {
                                operation: FirIntrinsic::CoroutineContext,
                                receiver: None,
                                parameters: Box::new([]),
                                result,
                            },
                            dispatch_receiver: None,
                            extension_receiver: None,
                            parameter_types: Box::new([]),
                            arguments: Box::new([]),
                            substitutions: Box::new([]),
                        })));
                    }
                    let external =
                        access
                            .property
                            .getter
                            .external_property_identity
                            .map(|property| ExternalPropertyTarget {
                                property,
                                receiver: None,
                                parameters: access.property.getter.params.clone(),
                                result: access.property.ty,
                                extension_receiver_parameter: None,
                            });
                    (
                        access.property.stable_declaration,
                        external,
                        None,
                        None,
                        access.context_args,
                    )
                }
                Some(ExprLowering::MemberPropertyRead {
                    mut stable_declaration,
                    source_member,
                    accessor,
                    declaration_ty,
                    context_access,
                    compiler_intrinsic,
                    ..
                }) => {
                    // A classifier-qualified companion property has a syntactic qualifier but no
                    // runtime value receiver there. Resolution records the exact companion
                    // singleton as an implicit receiver on the whole access; consume that semantic
                    // selection before considering the source receiver expression.
                    let selected_implicit = self
                        .info
                        .implicit_receiver_selections
                        .contains_key(&expression);
                    let dispatch_receiver = if selected_implicit {
                        self.implicit_receiver(expression)?
                    } else {
                        receiver
                            .map(|receiver| {
                                self.expression(receiver).map(|value| FirReceiver {
                                    value,
                                    conversion: None,
                                })
                            })
                            .transpose()?
                    };
                    let Some(dispatch_receiver) = dispatch_receiver else {
                        return Ok(None);
                    };
                    let intrinsic = match compiler_intrinsic {
                        Some(crate::libraries::CompilerIntrinsic::ArraySize) => {
                            Some(FirIntrinsic::ArraySize)
                        }
                        Some(crate::libraries::CompilerIntrinsic::StringLength) => {
                            Some(FirIntrinsic::StringLength)
                        }
                        Some(_) | None => None,
                    };
                    if let Some(operation) = intrinsic {
                        let receiver_ty = resolved_member
                            .as_ref()
                            .map(|selected| selected.receiver)
                            .or_else(|| {
                                self.info
                                    .implicit_receiver_selections
                                    .get(&expression)
                                    .map(|receiver| receiver.ty)
                            })
                            .or_else(|| receiver.map(|receiver| self.info.semantic_ty(receiver)))
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.expr_span(expression),
                                    BodyCheckFailureKind::MissingStablePropertyTarget,
                                )
                            })?;
                        return Ok(Some(FirExprKind::Call(FirCall {
                            target: FirCallTarget::Intrinsic {
                                operation,
                                receiver: Some(ResolvedTy::new(receiver_ty).map_err(|error| {
                                    self.failure(
                                        self.file.expr_span(expression),
                                        BodyCheckFailureKind::UnpublishableType(error),
                                    )
                                })?),
                                parameters: Box::new([]),
                                result: ResolvedTy::new(declaration_ty).map_err(|error| {
                                    self.failure(
                                        self.file.expr_span(expression),
                                        BodyCheckFailureKind::UnpublishableType(error),
                                    )
                                })?,
                            },
                            dispatch_receiver: Some(dispatch_receiver),
                            extension_receiver: None,
                            parameter_types: Box::new([]),
                            arguments: Box::new([]),
                            substitutions: Box::new([]),
                        })));
                    }
                    if stable_declaration.is_none() {
                        stable_declaration = source_member.and_then(|source| {
                            self.session
                                .active_source
                                .as_ref()?
                                .source_member_declaration(self.file, self.index, source)
                        });
                    }
                    let external = resolved_member
                        .as_ref()
                        .and_then(|selected| {
                            selected.member.external_property_identity.map(|property| {
                                ExternalPropertyTarget {
                                    property,
                                    receiver: Some(selected.receiver),
                                    parameters: selected.member.params.clone(),
                                    result: selected.ret,
                                    extension_receiver_parameter: None,
                                }
                            })
                        })
                        .or_else(|| {
                            let accessor = accessor.as_ref()?;
                            let identity = accessor.external_property_identity?;
                            let receiver_ty = self
                                .info
                                .implicit_receiver_selections
                                .get(&expression)
                                .map(|receiver| receiver.ty)
                                .or_else(|| {
                                    receiver.map(|receiver| self.info.semantic_ty(receiver))
                                })?;
                            Some(ExternalPropertyTarget {
                                property: identity,
                                receiver: Some(receiver_ty),
                                parameters: accessor.params.clone(),
                                result: declaration_ty,
                                extension_receiver_parameter: None,
                            })
                        });
                    if stable_declaration.is_none() && external.is_none() {
                        let mut getter = resolved_member.clone().ok_or_else(|| {
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::MissingStableCallTarget,
                            )
                        })?;
                        getter.context_args = context_access
                            .as_ref()
                            .map(|access| {
                                access
                                    .context_args
                                    .iter()
                                    .cloned()
                                    .map(Some)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        return self
                            .selected_member_call_with_receiver(
                                expression,
                                &[],
                                &getter,
                                dispatch_receiver,
                            )
                            .map(Some);
                    }
                    (
                        stable_declaration,
                        external,
                        Some(dispatch_receiver),
                        None,
                        context_access.map_or_else(Vec::new, |access| access.context_args),
                    )
                }
                Some(ExprLowering::ExtensionPropertyGet { access }) => {
                    let companion_extension = access
                        .property
                        .stable_declaration
                        .and_then(|declaration| self.index.declaration_header(declaration))
                        .is_some_and(|header| {
                            header.flags.has(crate::fir::DeclarationFlags::COMPANION)
                        });
                    let extension_receiver = if companion_extension {
                        None
                    } else {
                        Some(match receiver {
                            Some(receiver) => FirReceiver {
                                value: self.expression(receiver)?,
                                conversion: None,
                            },
                            None => {
                                let Some(receiver) = self.implicit_receiver(expression)? else {
                                    return Ok(None);
                                };
                                receiver
                            }
                        })
                    };
                    if access.property.getter.compiler_intrinsic
                        == Some(crate::libraries::CompilerIntrinsic::CharCode)
                    {
                        let extension_receiver = extension_receiver
                            .expect("the Char.code intrinsic has a runtime extension receiver");
                        let cause = self.expression_origin(expression)?;
                        let target = ResolvedTy::new(access.property.ty).map_err(|error| {
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnpublishableType(error),
                            )
                        })?;
                        return Ok(Some(FirExprKind::ImplicitConversion {
                            value: extension_receiver.value,
                            conversion: FirConversion {
                                origin: cause,
                                kind: FirConversionKind::NumericConversion { to: target },
                            },
                        }));
                    }
                    let context_count = access
                        .property
                        .context_count
                        .min(access.property.getter.params.len());
                    let singleton_dispatch = access.property.getter.singleton_dispatch.as_deref();
                    let mut parameters = access.property.getter.params.clone();
                    let extension_receiver_parameter =
                        singleton_dispatch.and_then(|_| u32::try_from(context_count).ok());
                    if singleton_dispatch.is_none() && context_count < parameters.len() {
                        parameters.remove(context_count);
                    }
                    let external =
                        access
                            .property
                            .getter
                            .external_property_identity
                            .map(|property| ExternalPropertyTarget {
                                property,
                                receiver: singleton_dispatch.map_or_else(
                                    || {
                                        (!companion_extension)
                                            .then_some(access.property.receiver)
                                            .flatten()
                                    },
                                    |singleton| Some(singleton.ty()),
                                ),
                                parameters,
                                result: access.property.ty,
                                extension_receiver_parameter,
                            });
                    // An extension imported from an object is still a MEMBER extension: the
                    // selected getter carries its exact singleton dispatch declaration. Preserve
                    // that receiver in FIR just as selected extension calls do. Companion-marked
                    // source extensions are intentionally realized as receiverless declarations.
                    let dispatch_receiver = if companion_extension {
                        None
                    } else {
                        access
                            .property
                            .getter
                            .singleton_dispatch
                            .as_deref()
                            .map(|singleton| self.singleton_call_receiver(expression, singleton))
                            .transpose()?
                    };
                    (
                        access.property.stable_declaration,
                        external,
                        dispatch_receiver,
                        extension_receiver,
                        access.context_args,
                    )
                }
                Some(ExprLowering::MemberExtensionPropertyRead {
                    stable_declaration,
                    getter,
                    dispatch_receiver,
                    context_args,
                    ..
                }) => {
                    let cause = self.expression_origin(expression)?;
                    let Some(dispatch_receiver) = self.materialize_implicit_receiver(
                        cause,
                        self.file.expr_span(expression),
                        &dispatch_receiver,
                    )?
                    else {
                        return Ok(None);
                    };
                    let extension_receiver = match receiver {
                        Some(receiver) => FirReceiver {
                            value: self.expression(receiver)?,
                            conversion: None,
                        },
                        None => {
                            let Some(receiver) = self.implicit_receiver(expression)? else {
                                return Ok(None);
                            };
                            receiver
                        }
                    };
                    let external = getter.and_then(|getter| {
                        getter
                            .external_property_identity
                            .map(|property| ExternalPropertyTarget {
                                property,
                                receiver: Some(Ty::obj_name(getter.owner)),
                                parameters: getter.params.clone(),
                                result: getter.ret,
                                extension_receiver_parameter: Some(
                                    u32::try_from(context_args.len())
                                        .expect("FIR parameter ordinals fit in u32"),
                                ),
                            })
                    });
                    (
                        stable_declaration,
                        external,
                        Some(dispatch_receiver),
                        Some(extension_receiver),
                        context_args,
                    )
                }
                Some(ExprLowering::AssociatedPropertyRead {
                    stable_declaration,
                    external_identity,
                    singleton_dispatch,
                    ..
                }) => {
                    let ty = self.info.semantic_ty(expression);
                    let dispatch_receiver = singleton_dispatch
                        .as_ref()
                        .map(|singleton| {
                            self.singleton_call_receiver(
                                expression,
                                &crate::libraries::SingletonDispatch {
                                    classifier: singleton.classifier,
                                },
                            )
                        })
                        .transpose()?;
                    (
                        stable_declaration,
                        external_identity.map(|property| ExternalPropertyTarget {
                            property,
                            receiver: singleton_dispatch
                                .as_ref()
                                .map(|singleton| Ty::obj_name(singleton.classifier)),
                            parameters: Vec::new(),
                            result: ty,
                            extension_receiver_parameter: None,
                        }),
                        dispatch_receiver,
                        None,
                        Vec::new(),
                    )
                }
                None => {
                    crate::trace_compiler!(
                        "fir",
                        "property read at {:?} has no selected lowering",
                        self.file.expr_span(expression),
                    );
                    return Ok(None);
                }
                Some(
                    ExprLowering::BuiltinUnaryCall { .. }
                    | ExprLowering::RuntimeTypeOperand(_)
                    | ExprLowering::ExtensionFunctionBinding { .. }
                    | ExprLowering::PluginExpression(_)
                    | ExprLowering::ClassStorageRead { .. }
                    | ExprLowering::EnumEntryPropertyRead { .. }
                    | ExprLowering::BackingFieldRead
                    | ExprLowering::ImplicitPropertyIncDec(_)
                    | ExprLowering::TopLevelPropertyIncDec(_)
                    | ExprLowering::LateinitInitialized { .. }
                    | ExprLowering::LocalFunction { .. }
                    | ExprLowering::AdaptedLocalFunctionRef { .. }
                    | ExprLowering::ConstructorRef { .. }
                    | ExprLowering::TopLevelFunctionRef(_)
                    | ExprLowering::CallableReference { .. }
                    | ExprLowering::AdaptedCallableReference { .. }
                    | ExprLowering::FunctionInvokeReference { .. }
                    | ExprLowering::AdaptedRef { .. }
                    | ExprLowering::SamConstructorReference { .. }
                    | ExprLowering::UnavailableCallableReference { .. }
                    | ExprLowering::Unavailable { .. }
                    | ExprLowering::Erased
                    | ExprLowering::IncDecAccessOperands(_)
                    | ExprLowering::CompilerSynthetic(_)
                    | ExprLowering::SamConstructor { .. }
                    | ExprLowering::Lambda(_)
                    | ExprLowering::SingletonValue(_)
                    | ExprLowering::ClassifierPropertyRead { .. }
                    | ExprLowering::LabeledThisInner
                    | ExprLowering::LabeledThisDispatch
                    | ExprLowering::IntrinsicProperty(_)
                    | ExprLowering::Invoke { .. }
                    | ExprLowering::SafePropertyInvoke { .. }
                    | ExprLowering::ClassLiteral { .. }
                    | ExprLowering::ReceiverFnInvoke { .. },
                ) => {
                    crate::trace_compiler!(
                        "fir",
                        "property read at {:?} selected {:?}, which has no checked FIR form",
                        self.file.expr_span(expression),
                        self.info.expr_lowers.get(&expression),
                    );
                    return Ok(None);
                }
            };
        let cause = self.expression_origin(expression)?;
        let declared_extension_receiver = declaration
            .and_then(|declaration| self.index.property_for_declaration(declaration))
            .and_then(|property| self.index.property(property))
            .and_then(|property| property.extension_receiver)
            .map(ResolvedTy::get);
        if let (Some(receiver), Some(target)) =
            (extension_receiver.as_mut(), declared_extension_receiver)
        {
            receiver.conversion = self.receiver_conversion(
                expression,
                cause,
                *receiver,
                Some(crate::types::stored_value_ty(target)),
            )?;
        }
        let context_arguments = context_args
            .iter()
            .map(|argument| self.materialize_context_argument(expression, cause, argument))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let target = self.property_target(expression, declaration, external)?;
        Ok(Some(FirExprKind::PropertyRead {
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            substitutions: Box::new([]),
        }))
    }

    fn property_target(
        &self,
        expression: ExprId,
        declaration: Option<DeclarationId>,
        external: Option<ExternalPropertyTarget>,
    ) -> Result<FirPropertyTarget, BodyCheckFailure> {
        self.property_target_at(self.file.expr_span(expression), declaration, external)
    }

    fn property_target_at(
        &self,
        span: Option<Span>,
        declaration: Option<DeclarationId>,
        external: Option<ExternalPropertyTarget>,
    ) -> Result<FirPropertyTarget, BodyCheckFailure> {
        if let Some(declaration) = declaration {
            return self
                .index
                .property_for_declaration(declaration)
                .map(FirPropertyTarget::Module)
                .ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget)
                });
        }
        let ExternalPropertyTarget {
            property,
            receiver,
            parameters,
            result,
            extension_receiver_parameter,
        } = external
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget))?;
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        Ok(FirPropertyTarget::External {
            property,
            receiver: receiver.map(resolved).transpose()?,
            parameters: parameters
                .into_iter()
                .map(resolved)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
            result: resolved(result)?,
            extension_receiver_parameter,
            dispatch: crate::fir::FirPropertyDispatch::Ordinary,
        })
    }

    /// `c++` / `c--` where `c` is a MEMBER property reached through an implicit receiver. The
    /// checker recorded the write target in `StmtLowering::ImplicitPropertyWrite`; this reads that
    /// property, applies the increment, and writes it back. Statement position discards the result,
    /// so no prefix/postfix distinction survives into FIR.
    ///
    /// `None` when the statement is not an implicit-property increment, so the caller keeps its own
    /// diagnosis rather than reporting a property failure for a genuinely unknown local.
    pub(super) fn implicit_property_inc_dec(
        &mut self,
        statement: StmtId,
        dec: bool,
    ) -> Result<Option<FirExprKind>, BodyCheckFailure> {
        let selected = self.info.stmt_lowers.get(&statement).cloned();
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let cause = self.statement_origin(statement)?;
        let target = match selected {
            Some(StmtLowering::ImplicitPropertyWrite(target)) => target,
            // A TOP-LEVEL `var` has no receiver at all; its increment reads and writes the same
            // property with no dispatch or extension receiver.
            Some(StmtLowering::TopLevelPropertySet(access)) => {
                let getter = Self::external_property_getter(&access.property, false);
                let setter = Self::external_property_setter(&access.property, false);
                return self.property_inc_dec_write(
                    statement,
                    dec,
                    span,
                    cause,
                    access.property.stable_declaration,
                    getter,
                    setter,
                    None,
                    None,
                    &access.context_args,
                );
            }
            _ => return Ok(None),
        };
        let (
            declaration,
            external_getter,
            external_setter,
            dispatch_receiver,
            extension_receiver,
            context_args,
        ) = match *target {
            crate::resolve::ImplicitPropertyWriteTarget::Member {
                receiver,
                stable_declaration,
                getter,
                setter,
                ..
            } => {
                let external_getter = getter.as_deref().and_then(Self::external_member_getter);
                let external_setter = setter
                    .as_deref()
                    .and_then(|setter| Self::external_member_setter(receiver.ty, &setter.callable));
                let Some(receiver) = self.materialize_implicit_receiver(cause, span, &receiver)?
                else {
                    return Ok(None);
                };
                (
                    stable_declaration,
                    external_getter,
                    external_setter,
                    Some(receiver),
                    None,
                    Vec::new(),
                )
            }
            crate::resolve::ImplicitPropertyWriteTarget::Extension { receiver, access } => {
                let external_getter = Self::external_property_getter(&access.property, true);
                let external_setter = Self::external_property_setter(&access.property, true);
                let companion_extension = access
                    .property
                    .stable_declaration
                    .and_then(|declaration| self.index.declaration_header(declaration))
                    .is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::COMPANION)
                    });
                let receiver = if companion_extension {
                    None
                } else {
                    let Some(receiver) =
                        self.materialize_implicit_receiver(cause, span, &receiver)?
                    else {
                        return Ok(None);
                    };
                    Some(receiver)
                };
                (
                    access.property.stable_declaration,
                    external_getter,
                    external_setter,
                    None,
                    receiver,
                    access.context_args,
                )
            }
        };
        self.property_inc_dec_write(
            statement,
            dec,
            span,
            cause,
            declaration,
            external_getter,
            external_setter,
            dispatch_receiver,
            extension_receiver,
            &context_args,
        )
    }

    /// The shared read → operator → write triple for a property increment, once its target and
    /// receivers are known.
    #[allow(clippy::too_many_arguments)]
    fn property_inc_dec_write(
        &mut self,
        statement: StmtId,
        dec: bool,
        span: Option<Span>,
        cause: OriginId,
        declaration: Option<DeclarationId>,
        external_getter: Option<ExternalPropertyTarget>,
        external_setter: Option<ExternalPropertyTarget>,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        context_args: &[crate::resolve::ResolvedContextArgument],
    ) -> Result<Option<FirExprKind>, BodyCheckFailure> {
        let read_target = self.property_target_at(span, declaration, external_getter)?;
        let write_target = self.property_target_at(span, declaration, external_setter)?;
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&crate::resolve::IncDecSite::Statement(statement))
            .copied()
            .ok_or_else(|| {
                self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedStatement(super::StatementForm::IncDec),
                )
            })?;
        // Context arguments are materialized from a source expression, and an increment STATEMENT
        // has none. A context-parameterized property increment keeps the caller's diagnosis rather
        // than being lowered from a fabricated origin.
        if !context_args.is_empty() {
            return Ok(None);
        }
        let context_arguments: Box<[FirReceiver]> = Box::new([]);
        let origin = cause;
        let span =
            span.ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let read = self.body.add_expr(FirExpr {
            origin,
            ty: self.resolved_type(span, resolution.receiver_ty)?,
            kind: FirExprKind::PropertyRead {
                target: read_target,
                dispatch_receiver,
                extension_receiver,
                context_arguments: context_arguments.clone(),
                substitutions: Box::new([]),
            },
        });
        let convention = if dec { "dec" } else { "inc" };
        let updated_kind = if self
            .info
            .resolved_stmt_operator_call(statement, convention)
            .is_some()
        {
            self.zero_arg_statement_operator_call_on_value(statement, convention, read)?
        } else {
            FirExprKind::Unary {
                operation: if dec {
                    super::FirUnaryOperation::Decrement
                } else {
                    super::FirUnaryOperation::Increment
                },
                operand: read,
            }
        };
        let updated_ty = self.resolved_type(span, resolution.updated_ty)?;
        let updated = self.body.add_expr(FirExpr {
            origin,
            ty: updated_ty,
            kind: updated_kind,
        });
        Ok(Some(FirExprKind::PropertyWrite {
            target: write_target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            value: updated,
            conversion: None,
            substitutions: Box::new([]),
        }))
    }

    /// `p++` / `++p` in VALUE position where `p` is a property (a member reached through an implicit
    /// receiver, or a top-level `var`). Getter-call COUNT is observable, so both forms match kotlinc
    /// exactly (verified against it with a counting getter):
    ///   postfix `t = read; write(op(t)); t`   — ONE read, the old value is the bound temporary
    ///   prefix  `write(op(read)); read`       — TWO reads, the result re-reads after writeback
    ///
    /// The prefix re-read is not redundant: property storage cannot carry the flow fact that a
    /// lexical local can, so its result keeps the selected receiver view rather than the operator's.
    ///
    /// `None` when the expression is not a property increment, so the caller keeps its own
    /// `UnknownLocal` diagnosis for a genuinely unknown name.
    pub(super) fn property_inc_dec_expression(
        &mut self,
        expression: ExprId,
        target: ExprId,
        decrement: bool,
        prefix: bool,
    ) -> Result<Option<FirExprKind>, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let cause = self.expression_origin(expression)?;
        // `field++` inside an accessor, in VALUE position. The backing field is addressed directly —
        // no getter, no setter — so this is the expression analogue of the statement arm added for
        // `field++` as a statement.
        if matches!(
            self.info.expr_lowers.get(&target),
            Some(ExprLowering::BackingFieldRead)
        ) {
            let property = self.enclosing_property(target)?;
            let span = self
                .file
                .expr_span(expression)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
            let cause = self.expression_origin(expression)?;
            let resolution = self
                .info
                .resolved_inc_dec
                .get(&crate::resolve::IncDecSite::Expression(expression))
                .copied()
                .ok_or_else(|| {
                    self.failure(
                        Some(span),
                        BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
                    )
                })?;
            let read_ty = self.resolved_type(span, resolution.receiver_ty)?;
            let updated_ty = self.resolved_type(span, resolution.updated_ty)?;
            let read = self.body.add_expr(FirExpr {
                origin: cause,
                ty: read_ty,
                kind: FirExprKind::BackingFieldRead { target: property },
            });
            let temporary = self.allocate_local();
            let (bound, bound_ty) = if prefix {
                let updated = self.increment_value(expression, target, decrement)?;
                (updated, updated_ty)
            } else {
                (read, read_ty)
            };
            let mut statements = vec![self.body.add_statement(FirStatement {
                origin: cause,
                kind: FirStatementKind::Local {
                    target: temporary,
                    ty: bound_ty,
                    mutable: false,
                    lateinit: false,
                    initializer: Some(bound),
                    conversion: None,
                },
            })];
            let stored = self.body.add_expr(FirExpr {
                origin: cause,
                ty: bound_ty,
                kind: FirExprKind::ValueRead(temporary),
            });
            let value = if prefix {
                stored
            } else {
                let operand = self.body.add_expr(FirExpr {
                    origin: cause,
                    ty: bound_ty,
                    kind: FirExprKind::ValueRead(temporary),
                });
                self.body.add_expr(FirExpr {
                    origin: cause,
                    ty: updated_ty,
                    kind: FirExprKind::Unary {
                        operation: if decrement {
                            super::FirUnaryOperation::Decrement
                        } else {
                            super::FirUnaryOperation::Increment
                        },
                        operand,
                    },
                })
            };
            let write = self.body.add_expr(FirExpr {
                origin: cause,
                ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                kind: FirExprKind::BackingFieldWrite {
                    target: property,
                    value,
                    conversion: None,
                },
            });
            statements.push(self.body.add_statement(FirStatement {
                origin: cause,
                kind: FirStatementKind::Expression(write),
            }));
            return Ok(Some(FirExprKind::Block {
                statements: statements.into_boxed_slice(),
                result: Some(stored),
            }));
        }
        let selected = self.info.expr_lowers.get(&expression).cloned();
        let (
            declaration,
            external_getter,
            external_setter,
            dispatch_receiver,
            extension_receiver,
            context_args,
        ) = match selected {
            Some(ExprLowering::TopLevelPropertyIncDec(access)) => {
                let getter = Self::external_property_getter(&access.property, false);
                let setter = Self::external_property_setter(&access.property, false);
                (
                    access.property.stable_declaration,
                    getter,
                    setter,
                    None,
                    None,
                    access.context_args,
                )
            }
            Some(ExprLowering::ImplicitPropertyIncDec(target)) => match *target {
                crate::resolve::ImplicitPropertyWriteTarget::Member {
                    receiver,
                    stable_declaration,
                    getter,
                    setter,
                    ..
                } => {
                    let external_getter = getter.as_deref().and_then(Self::external_member_getter);
                    let external_setter = setter.as_deref().and_then(|setter| {
                        Self::external_member_setter(receiver.ty, &setter.callable)
                    });
                    let Some(receiver) =
                        self.materialize_implicit_receiver(cause, span, &receiver)?
                    else {
                        return Ok(None);
                    };
                    (
                        stable_declaration,
                        external_getter,
                        external_setter,
                        Some(receiver),
                        None,
                        Vec::new(),
                    )
                }
                crate::resolve::ImplicitPropertyWriteTarget::Extension { receiver, access } => {
                    let external_getter = Self::external_property_getter(&access.property, true);
                    let external_setter = Self::external_property_setter(&access.property, true);
                    let Some(receiver) =
                        self.materialize_implicit_receiver(cause, span, &receiver)?
                    else {
                        return Ok(None);
                    };
                    (
                        access.property.stable_declaration,
                        external_getter,
                        external_setter,
                        None,
                        Some(receiver),
                        access.context_args,
                    )
                }
            },
            _ => return Ok(None),
        };
        // Context arguments materialize from a source expression the increment does not have.
        if !context_args.is_empty() {
            return Ok(None);
        }
        let span =
            span.ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let read_target = self.property_target_at(Some(span), declaration, external_getter)?;
        let write_target = self.property_target_at(Some(span), declaration, external_setter)?;
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&crate::resolve::IncDecSite::Expression(expression))
            .copied()
            .ok_or_else(|| {
                self.failure(
                    Some(span),
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
                )
            })?;
        let read_ty = self.resolved_type(span, resolution.receiver_ty)?;
        let updated_ty = self.resolved_type(span, resolution.updated_ty)?;
        let read = self.body.add_expr(FirExpr {
            origin: cause,
            ty: read_ty,
            kind: FirExprKind::PropertyRead {
                target: read_target.clone(),
                dispatch_receiver,
                extension_receiver,
                context_arguments: Box::new([]),
                substitutions: Box::new([]),
            },
        });
        let convention = if decrement { "dec" } else { "inc" };
        let increment = |checker: &mut Self, operand| -> Result<FirExprId, BodyCheckFailure> {
            let kind = if checker.selected_operator(expression, convention) {
                if let Some(ResolvedCall::LocalFunction(selected)) = checker
                    .info
                    .resolved_operator_call(expression, convention)
                    .cloned()
                {
                    checker.local_operator_call_on_value(
                        checker.file.expr_span(expression),
                        cause,
                        &selected,
                        operand,
                        &[],
                    )?
                } else {
                    FirExprKind::Call(checker.source_member_operator_call_on_value(
                        expression,
                        convention,
                        operand,
                        &[],
                    )?)
                }
            } else {
                FirExprKind::Unary {
                    operation: if decrement {
                        super::FirUnaryOperation::Decrement
                    } else {
                        super::FirUnaryOperation::Increment
                    },
                    operand,
                }
            };
            Ok(checker.body.add_expr(FirExpr {
                origin: cause,
                ty: updated_ty,
                kind,
            }))
        };
        let mut statements = Vec::new();
        let (value, result_source) = if prefix {
            (increment(self, read)?, None)
        } else {
            let temporary = self.allocate_local();
            statements.push(self.body.add_statement(FirStatement {
                origin: cause,
                kind: FirStatementKind::Local {
                    target: temporary,
                    ty: read_ty,
                    mutable: false,
                    lateinit: false,
                    initializer: Some(read),
                    conversion: None,
                },
            }));
            let stored = self.body.add_expr(FirExpr {
                origin: cause,
                ty: read_ty,
                kind: FirExprKind::ValueRead(temporary),
            });
            (increment(self, stored)?, Some(temporary))
        };
        let write = self.body.add_expr(FirExpr {
            origin: cause,
            ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
            kind: FirExprKind::PropertyWrite {
                target: write_target,
                dispatch_receiver,
                extension_receiver,
                context_arguments: Box::new([]),
                value,
                conversion: None,
                substitutions: Box::new([]),
            },
        });
        statements.push(self.body.add_statement(FirStatement {
            origin: cause,
            kind: FirStatementKind::Expression(write),
        }));
        let result_ty = self.expression_type(expression)?;
        let result = match result_source {
            Some(temporary) => self.body.add_expr(FirExpr {
                origin: cause,
                ty: result_ty,
                kind: FirExprKind::ValueRead(temporary),
            }),
            // Prefix re-reads the property after the write, exactly as kotlinc does.
            None => self.body.add_expr(FirExpr {
                origin: cause,
                ty: result_ty,
                kind: FirExprKind::PropertyRead {
                    target: read_target,
                    dispatch_receiver,
                    extension_receiver,
                    context_arguments: Box::new([]),
                    substitutions: Box::new([]),
                },
            }),
        };
        Ok(Some(FirExprKind::Block {
            statements: statements.into_boxed_slice(),
            result: Some(result),
        }))
    }

    pub(super) fn source_property_write(
        &mut self,
        statement: StmtId,
        receiver: Option<ExprId>,
        value: ExprId,
    ) -> Result<Option<FirExprKind>, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let cause = self.statement_origin(statement)?;
        let selected = self.info.stmt_lowers.get(&statement).cloned();
        let (declaration, external, dispatch_receiver, mut extension_receiver, context_args) =
            match selected {
                Some(StmtLowering::TopLevelPropertySet(access)) => (
                    access.property.stable_declaration,
                    Self::external_property_setter(&access.property, false),
                    None,
                    None,
                    access.context_args,
                ),
                Some(StmtLowering::MemberPropertyWrite {
                    stable_declaration,
                    backing_field,
                    setter,
                    setter_declaration,
                    context_access,
                    ..
                }) => {
                    let Some(receiver) = receiver else {
                        return Ok(None);
                    };
                    if backing_field {
                        let declaration = stable_declaration.ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget)
                        })?;
                        let target = self
                            .index
                            .property_for_declaration(declaration)
                            .ok_or_else(|| {
                                self.failure(
                                    span,
                                    BodyCheckFailureKind::MissingStablePropertyTarget,
                                )
                            })?;
                        return Ok(Some(FirExprKind::BackingFieldWrite {
                            target,
                            value: self.expression(value)?,
                            conversion: None,
                        }));
                    }
                    let receiver_ty = self.info.semantic_ty(receiver);
                    let dispatch_receiver = FirReceiver {
                        value: self.expression(receiver)?,
                        conversion: None,
                    };
                    let external = setter
                        .as_deref()
                        .or_else(|| {
                            context_access
                                .as_ref()
                                .and_then(|access| access.property.setter.as_ref())
                        })
                        .and_then(|setter| {
                            setter.external_property_identity.map(|property| {
                                ExternalPropertyTarget {
                                    property,
                                    receiver: Some(receiver_ty),
                                    parameters: setter.params.clone(),
                                    result: setter.ret,
                                    extension_receiver_parameter: None,
                                }
                            })
                        });
                    if stable_declaration.is_none() && external.is_none() {
                        let setter = setter
                            .as_deref()
                            .or_else(|| {
                                context_access
                                    .as_ref()
                                    .and_then(|access| access.property.setter.as_ref())
                            })
                            .ok_or_else(|| {
                                self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                            })?;
                        let mut selected = crate::symbol_resolver::ResolvedMember::from_callable(
                            receiver_ty,
                            setter.clone(),
                            false,
                        );
                        selected.member.stable_declaration = setter_declaration.or_else(|| {
                            context_access.as_ref().and_then(|access| {
                                access
                                    .property
                                    .setter_declaration
                                    .or(access.property.stable_declaration)
                            })
                        });
                        selected.context_args = context_access
                            .as_ref()
                            .map(|access| {
                                access
                                    .context_args
                                    .iter()
                                    .cloned()
                                    .map(Some)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        return self
                            .selected_member_setter_call(value, &selected, dispatch_receiver)
                            .map(Some);
                    }
                    (
                        stable_declaration,
                        external,
                        Some(dispatch_receiver),
                        None,
                        context_access.map_or_else(Vec::new, |access| access.context_args),
                    )
                }
                Some(StmtLowering::ExtensionPropertyWrite { access }) => {
                    let Some(receiver) = receiver else {
                        return Ok(None);
                    };
                    let companion_extension = access.property.is_companion_extension();
                    let extension_receiver = if companion_extension {
                        None
                    } else {
                        Some(FirReceiver {
                            value: self.expression(receiver)?,
                            conversion: None,
                        })
                    };
                    let dispatch_receiver = if companion_extension {
                        None
                    } else {
                        access
                            .property
                            .setter
                            .as_ref()
                            .and_then(|setter| setter.singleton_dispatch.as_deref())
                            .map(|singleton| self.singleton_call_receiver(value, singleton))
                            .transpose()?
                    };
                    (
                        access.property.stable_declaration,
                        Self::external_property_setter(&access.property, true),
                        dispatch_receiver,
                        extension_receiver,
                        access.context_args,
                    )
                }
                Some(StmtLowering::ImplicitPropertyWrite(target)) => match *target {
                    crate::resolve::ImplicitPropertyWriteTarget::Member {
                        receiver,
                        stable_declaration,
                        setter,
                        ..
                    } => {
                        let receiver_ty = receiver.ty;
                        let external = setter.as_deref().and_then(|setter| {
                            setter.callable.external_property_identity.map(|property| {
                                ExternalPropertyTarget {
                                    property,
                                    receiver: Some(receiver_ty),
                                    parameters: setter.callable.params.clone(),
                                    result: setter.callable.ret,
                                    extension_receiver_parameter: None,
                                }
                            })
                        });
                        let Some(dispatch_receiver) =
                            self.materialize_implicit_receiver(cause, span, &receiver)?
                        else {
                            return Ok(None);
                        };
                        if stable_declaration.is_none() && external.is_none() {
                            let setter = setter.ok_or_else(|| {
                                self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                            })?;
                            let mut selected =
                                crate::symbol_resolver::ResolvedMember::from_callable(
                                    receiver_ty,
                                    setter.callable,
                                    false,
                                );
                            selected.member.stable_declaration = setter.stable_declaration;
                            return self
                                .selected_member_setter_call(value, &selected, dispatch_receiver)
                                .map(Some);
                        }
                        (
                            stable_declaration,
                            external,
                            Some(dispatch_receiver),
                            None,
                            Vec::new(),
                        )
                    }
                    crate::resolve::ImplicitPropertyWriteTarget::Extension { receiver, access } => {
                        let companion_extension = access
                            .property
                            .stable_declaration
                            .and_then(|declaration| self.index.declaration_header(declaration))
                            .is_some_and(|header| {
                                header.flags.has(crate::fir::DeclarationFlags::COMPANION)
                            });
                        let receiver = if companion_extension {
                            None
                        } else {
                            let Some(receiver) =
                                self.materialize_implicit_receiver(cause, span, &receiver)?
                            else {
                                return Ok(None);
                            };
                            Some(receiver)
                        };
                        let dispatch_receiver = if companion_extension {
                            None
                        } else {
                            access
                                .property
                                .setter
                                .as_ref()
                                .and_then(|setter| setter.singleton_dispatch.as_deref())
                                .map(|singleton| self.singleton_call_receiver(value, singleton))
                                .transpose()?
                        };
                        (
                            access.property.stable_declaration,
                            Self::external_property_setter(&access.property, true),
                            dispatch_receiver,
                            receiver,
                            access.context_args,
                        )
                    }
                },
                Some(StmtLowering::MemberExtensionPropertyWrite {
                    stable_declaration,
                    setter,
                    dispatch_receiver,
                    context_args,
                    ..
                }) => {
                    let Some(receiver) = receiver else {
                        return Ok(None);
                    };
                    let Some(dispatch_receiver) =
                        self.materialize_implicit_receiver(cause, span, &dispatch_receiver)?
                    else {
                        return Ok(None);
                    };
                    let external = setter.and_then(|setter| {
                        setter
                            .external_property_identity
                            .map(|property| ExternalPropertyTarget {
                                property,
                                receiver: Some(Ty::obj_name(setter.owner)),
                                parameters: setter.params.clone(),
                                result: setter.ret,
                                extension_receiver_parameter: Some(
                                    u32::try_from(context_args.len())
                                        .expect("FIR parameter ordinals fit in u32"),
                                ),
                            })
                    });
                    (
                        stable_declaration,
                        external,
                        Some(dispatch_receiver),
                        Some(FirReceiver {
                            value: self.expression(receiver)?,
                            conversion: None,
                        }),
                        context_args,
                    )
                }
                Some(StmtLowering::AssociatedPropertyWrite {
                    stable_declaration,
                    external_identity,
                    ty,
                    ..
                }) => (
                    stable_declaration,
                    external_identity.map(|property| ExternalPropertyTarget {
                        property,
                        receiver: None,
                        parameters: vec![ty],
                        result: Ty::Unit,
                        extension_receiver_parameter: None,
                    }),
                    None,
                    None,
                    Vec::new(),
                ),
                None => return Ok(None),
                Some(
                    StmtLowering::LocalFunction(_)
                    | StmtLowering::PlusAssign(_)
                    | StmtLowering::BackingFieldWrite
                    | StmtLowering::DeferredPropertyWrite { .. }
                    | StmtLowering::SuperPropertyWrite { .. }
                    | StmtLowering::Erased,
                ) => return Ok(None),
            };
        let declared_extension_receiver = declaration
            .and_then(|declaration| self.index.property_for_declaration(declaration))
            .and_then(|property| self.index.property(property))
            .and_then(|property| property.extension_receiver)
            .map(ResolvedTy::get);
        if let (Some(receiver), Some(target)) =
            (extension_receiver.as_mut(), declared_extension_receiver)
        {
            receiver.conversion = self.receiver_conversion(
                value,
                cause,
                *receiver,
                Some(crate::types::stored_value_ty(target)),
            )?;
        }
        let context_arguments = context_args
            .iter()
            .map(|argument| self.materialize_context_argument(value, cause, argument))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let value_target = declaration
            .and_then(|declaration| self.index.signature(declaration))
            .map(|signature| signature.result)
            .or_else(|| {
                external
                    .as_ref()
                    .and_then(|target| target.parameters.last().copied())
                    .and_then(|ty| ResolvedTy::new(ty).ok())
            })
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget))?;
        let conversion = self.selected_value_conversion(value, value_target, cause)?;
        let target = self.property_target(value, declaration, external)?;
        Ok(Some(FirExprKind::PropertyWrite {
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            value: self.expression(value)?,
            conversion,
            substitutions: Box::new([]),
        }))
    }
}

impl BodyFirChecker<'_> {
    /// Check a member access the checker selected as a zero-argument member CALL.
    ///
    /// Kotlin's property syntax reads a JVM `getX()` with no Kotlin property metadata behind it
    /// (`ArrayList.size` → `Collection.getSize()`), and the checker records that as a member call.
    /// Both providers route through the shared operator/call target mapping, so a dependency getter
    /// is a linkage difference rather than a missing target.
    pub(super) fn zero_argument_member_read(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        // `super.prop` reads the SUPER accessor, and the checker records that selection in
        // `resolved_super_calls` rather than `resolved_calls` — the same channel a `super.f()` call
        // uses, which `qualified_call` already consults. Reading it here is what makes a super
        // property read resolve instead of reporting an unsupported member expression.
        if let Some(target) = self.info.resolved_super_call(expression).cloned() {
            return self.selected_super_property_read(expression, &target);
        }
        // A qualified spelling can denote a VALUE rather than a member read: `C.Companion` is the
        // companion instance and `E.ENTRY` is an enum entry. The bare-name arm already consumes both
        // channels; a qualified one reaches here, so read them here too rather than demanding a
        // resolved call that was never recorded.
        if let Some(ExprLowering::SingletonValue(singleton)) =
            self.info.expr_lowers.get(&expression)
        {
            return Ok(FirExprKind::SingletonValue {
                classifier: singleton.classifier,
            });
        }
        if let Some(entry) = self.info.resolved_enum_entry(expression) {
            return Ok(FirExprKind::EnumEntry {
                classifier: entry.classifier,
                ordinal: entry.ordinal,
                name: entry.name.clone().into_boxed_str(),
            });
        }
        // `::v.isInitialized` on a `lateinit var`. kotlinc compiles it to a NULL CHECK on the
        // backing field — a lateinit field holds `null` until assigned — so there is no reflection
        // and no `KProperty` value involved.
        if let Some(ExprLowering::LateinitInitialized { declaration, .. }) =
            self.info.expr_lowers.get(&expression).cloned()
        {
            let target = declaration
                .and_then(|declaration| self.index.property_for_declaration(declaration))
                .ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget)
                })?;
            let cause = self.expression_origin(expression)?;
            let field = self.body.add_expr(FirExpr {
                origin: cause,
                ty: ResolvedTy::new(Ty::nullable(Ty::obj("kotlin/Any")))
                    .expect("a nullable Any is a publishable FIR type"),
                kind: FirExprKind::LateinitFieldRead { target },
            });
            let null = self.body.add_expr(FirExpr {
                origin: cause,
                ty: ResolvedTy::new(Ty::Null).expect("Null is a publishable FIR type"),
                kind: FirExprKind::Constant(FirConstant::Null),
            });
            return Ok(FirExprKind::Binary {
                operation: FirBinaryOperation::ReferentialNotEqual,
                lhs: field,
                rhs: null,
            });
        }
        let selected = self
            .info
            .resolved_calls
            .get(&expression)
            .cloned()
            .ok_or_else(|| {
                self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Member),
                )
            })?;
        let selected = self.selected_call_target(span, Some(&selected))?;
        if !selected.context_arguments.is_empty()
            || selected.vararg_index.is_some()
            || !selected.value_parameters.is_empty()
        {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        }
        let bound = FirReceiver {
            value: self.expression(receiver)?,
            conversion: None,
        };
        let parameter_types = selected.parameter_types();
        Ok(FirExprKind::Call(FirCall {
            target: selected.target,
            dispatch_receiver: (!selected.extension).then_some(bound),
            extension_receiver: selected.extension.then_some(bound),
            parameter_types,
            arguments: Box::new([]),
            substitutions: Box::new([]),
        }))
    }
}

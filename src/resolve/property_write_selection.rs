//! Ordered selection and recording for writes through implicit receiver and static-scope rungs.

use super::*;

pub(super) struct ImplicitPropertyWriteResolution {
    pub(super) receiver: ImplicitReceiver,
    pub(super) property_ty: Ty,
    pub(super) is_var: bool,
    context_args: Vec<ResolvedContextArgument>,
    pub(super) getter: Option<crate::symbol_resolver::ResolvedMember>,
    pub(super) setter: Option<crate::symbol_resolver::ResolvedPropertySetter>,
    extension: Option<ResolvedPropertyAccess>,
    stable_declaration: Option<crate::fir::DeclarationId>,
}

pub(super) enum PropertyWriteSelection {
    None,
    Implicit(Box<ImplicitPropertyWriteResolution>),
    Receiverless(Box<ResolvedPropertyAccess>),
    /// A language-defined classifier property selected by the same implicit tower as reads. These
    /// properties are read-only, but still terminate write lookup so a farther mutable property
    /// cannot become a fallback target.
    ClassifierProperty(Box<crate::libraries::ClassifierProperty>),
    MissingContext(MissingContextParameter, Vec<String>),
    AccessorAmbiguous(Vec<String>),
    Ambiguous,
}

impl PropertyWriteSelection {
    pub(super) fn storage(&self) -> Option<(Ty, bool)> {
        match self {
            Self::Implicit(property) => Some((property.property_ty, property.is_var)),
            Self::Receiverless(property) => {
                Some((property.property.ty, property.property.setter.is_some()))
            }
            Self::ClassifierProperty(property) => Some((property.ty, false)),
            Self::None
            | Self::MissingContext(..)
            | Self::AccessorAmbiguous(_)
            | Self::Ambiguous => None,
        }
    }
}

impl Checker<'_> {
    /// Resolve a bare property write through Kotlin's ordered implicit rungs. A read-only property
    /// on a nearer rung is terminal and must not fall through to a farther writable property.
    pub(super) fn implicit_property_write(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> PropertyWriteSelection {
        for rung in self.implicit_rungs(scope) {
            match rung {
                implicit_rungs::ImplicitRung::PrioritizedClassifierProperties(owner) => {
                    if let Some((_, property)) = self.classifier_property_for_owner(owner, name) {
                        return PropertyWriteSelection::ClassifierProperty(Box::new(property));
                    }
                }
                implicit_rungs::ImplicitRung::Receiver(receiver) => {
                    match self.property_write_on_receiver(scope, receiver, name) {
                        Ok(Some(property)) => {
                            return PropertyWriteSelection::Implicit(Box::new(property));
                        }
                        Ok(None) => {}
                        Err(candidates) => {
                            return PropertyWriteSelection::AccessorAmbiguous(candidates);
                        }
                    }
                }
                implicit_rungs::ImplicitRung::StaticScope(classifier) => {
                    match self.select_static_scope_property(scope, classifier, name) {
                        TopLevelPropertySelection::None => {}
                        TopLevelPropertySelection::Selected(property) => {
                            return PropertyWriteSelection::Receiverless(property);
                        }
                        TopLevelPropertySelection::MissingContext(missing, names) => {
                            return PropertyWriteSelection::MissingContext(missing, names);
                        }
                        TopLevelPropertySelection::Ambiguous => {
                            return PropertyWriteSelection::Ambiguous;
                        }
                    }
                }
            }
        }
        if let Some((receiver, declared_name, _)) = self.imported_singleton_member(name) {
            match self.property_write_on_receiver(scope, receiver, &declared_name) {
                Ok(Some(property)) => {
                    return PropertyWriteSelection::Implicit(Box::new(property));
                }
                Ok(None) => {}
                Err(candidates) => {
                    return PropertyWriteSelection::AccessorAmbiguous(candidates);
                }
            }
        }
        match self.select_declared_top_level_property(scope, name) {
            TopLevelPropertySelection::None => PropertyWriteSelection::None,
            TopLevelPropertySelection::Selected(property) => {
                PropertyWriteSelection::Receiverless(property)
            }
            TopLevelPropertySelection::MissingContext(missing, names) => {
                PropertyWriteSelection::MissingContext(missing, names)
            }
            TopLevelPropertySelection::Ambiguous => PropertyWriteSelection::Ambiguous,
        }
    }

    fn property_write_on_receiver(
        &self,
        scope: &CheckerScope<'_>,
        receiver: ImplicitReceiver,
        name: &str,
    ) -> Result<Option<ImplicitPropertyWriteResolution>, Vec<String>> {
        if let Some(property) = self.checked_body_local_property(receiver.ty, name) {
            return Ok(Some(ImplicitPropertyWriteResolution {
                receiver,
                property_ty: property.ty,
                is_var: property.mutable,
                context_args: Vec::new(),
                getter: None,
                setter: None,
                extension: None,
                stable_declaration: property.stable_declaration,
            }));
        }
        let selected = self.resolver().select_member_property_applicable_where(
            receiver.ty,
            name,
            |property| {
                let context_types = property.getter.params.get(..property.context_count)?;
                self.select_context_arguments_with_types(scope, context_types)
                    .ok()
                    .map(|_| {
                        (
                            self.receiver_property_accessible(
                                property.visibility,
                                property.owner,
                                receiver.ty,
                            ),
                            property.context_count,
                        )
                    })
            },
        );
        if let Some(selected) = selected {
            if let Some(candidates) = self.competing_accessor_candidates(name, &selected) {
                return Err(candidates);
            }
            let selected_ty = selected.ty;
            let Some(property) = selected.property else {
                return Ok(None);
            };
            let Some(context_types) = property.getter.params.get(..property.context_count) else {
                return Ok(None);
            };
            let Some(context_args) = self.select_context_arguments(scope, context_types) else {
                return Ok(None);
            };
            let mut getter = crate::symbol_resolver::ResolvedMember::from_callable(
                receiver.ty,
                property.getter.clone(),
                false,
            );
            getter.context_args = context_args.iter().cloned().map(Some).collect();
            getter.member.context_count = property.context_count;
            getter.member.stable_declaration =
                property.getter_declaration.or(property.stable_declaration);
            let setter = property.setter.clone().map(|callable| {
                crate::symbol_resolver::ResolvedPropertySetter {
                    callable,
                    visibility: property.setter_visibility,
                    source_member: property.source_member,
                    stable_declaration: property.stable_declaration,
                }
            });
            return Ok(Some(ImplicitPropertyWriteResolution {
                receiver,
                property_ty: selected_ty,
                is_var: setter.is_some(),
                context_args,
                getter: Some(getter),
                setter,
                extension: None,
                stable_declaration: property.stable_declaration,
            }));
        }
        let getter = self.select_property_member(receiver.ty, name);
        let setter = self.select_property_setter(receiver.ty, name);
        if let Some(setter) = setter {
            let ty = setter.callable.params.first().copied().unwrap_or(Ty::Error);
            let stable_declaration = setter.stable_declaration;
            return Ok(Some(ImplicitPropertyWriteResolution {
                receiver,
                property_ty: ty,
                is_var: true,
                context_args: Vec::new(),
                getter,
                setter: Some(setter),
                extension: None,
                stable_declaration,
            }));
        }
        if let Some(property) = getter {
            let stable_declaration = property.member.stable_declaration;
            return Ok(Some(ImplicitPropertyWriteResolution {
                receiver,
                property_ty: property.ret,
                is_var: false,
                context_args: Vec::new(),
                getter: Some(property),
                setter: None,
                extension: None,
                stable_declaration,
            }));
        }
        let type_variables = self.postponed_type_variables_in(receiver.ty);
        let resolver = self.resolver().with_type_variables(&type_variables);
        if let Ok(Some(property)) = resolver.select_extension_property(receiver.ty, name) {
            let context_args = if property.context_count == 0 {
                Vec::new()
            } else {
                let Some(context_types) = property.getter.params.get(1..1 + property.context_count)
                else {
                    return Ok(None);
                };
                let Some(context_args) = self.select_context_arguments(scope, context_types) else {
                    return Ok(None);
                };
                context_args
            };
            return Ok(Some(ImplicitPropertyWriteResolution {
                receiver,
                property_ty: property.ty,
                is_var: property.setter.is_some(),
                context_args: Vec::new(),
                getter: None,
                setter: None,
                extension: Some(ResolvedPropertyAccess {
                    property,
                    context_args,
                }),
                stable_declaration: None,
            }));
        }
        Ok(None)
    }

    fn implicit_property_write_target(
        &self,
        resolution: &ImplicitPropertyWriteResolution,
    ) -> ImplicitPropertyWriteTarget {
        let receiver = self.implicit_receiver_selection(resolution.receiver);
        if let Some(access) = &resolution.extension {
            ImplicitPropertyWriteTarget::Extension {
                receiver,
                access: Box::new(access.clone()),
            }
        } else {
            ImplicitPropertyWriteTarget::Member {
                receiver,
                stable_declaration: resolution.stable_declaration,
                property_ty: resolution.property_ty,
                context_args: resolution.context_args.clone(),
                getter: resolution.getter.clone().map(Box::new),
                setter: resolution.setter.clone().map(Box::new),
            }
        }
    }

    /// An implicit receiver over a postponed call's type variables adds its constraint against
    /// the selected extension property's declared receiver to that call.
    fn constrain_postponed_extension_receiver(
        &mut self,
        resolution: &ImplicitPropertyWriteResolution,
        span: Span,
    ) {
        let declared = resolution
            .extension
            .as_ref()
            .and_then(|access| access.property.receiver);
        if let Some(declared) = declared {
            if self.postponed_call_mentions(resolution.receiver.ty) {
                self.expect_assignable(
                    declared,
                    resolution.receiver.ty,
                    span,
                    "extension receiver",
                );
            }
        }
    }

    pub(super) fn record_implicit_property_write(
        &mut self,
        stmt: StmtId,
        selection: &PropertyWriteSelection,
    ) {
        match selection {
            PropertyWriteSelection::Implicit(resolution) => {
                let span = self.file.stmt_spans[stmt.0 as usize];
                self.constrain_postponed_extension_receiver(resolution, span);
                let target = self.implicit_property_write_target(resolution);
                self.stmt_lowers
                    .insert(stmt, StmtLowering::ImplicitPropertyWrite(Box::new(target)));
            }
            PropertyWriteSelection::Receiverless(property) => {
                self.stmt_lowers
                    .insert(stmt, StmtLowering::TopLevelPropertySet(property.clone()));
            }
            PropertyWriteSelection::None
            | PropertyWriteSelection::ClassifierProperty(_)
            | PropertyWriteSelection::MissingContext(..)
            | PropertyWriteSelection::AccessorAmbiguous(_)
            | PropertyWriteSelection::Ambiguous => {}
        }
    }

    pub(super) fn record_implicit_property_incdec(
        &mut self,
        expression: ExprId,
        selection: &PropertyWriteSelection,
    ) {
        match selection {
            PropertyWriteSelection::Implicit(resolution) => {
                self.mark_extension_receiver_used(expression, resolution.receiver);
                self.constrain_postponed_extension_receiver(resolution, self.span(expression));
                let target = self.implicit_property_write_target(resolution);
                self.expr_lowers.insert(
                    expression,
                    ExprLowering::ImplicitPropertyIncDec(Box::new(target)),
                );
            }
            PropertyWriteSelection::Receiverless(property) => {
                self.expr_lowers.insert(
                    expression,
                    ExprLowering::TopLevelPropertyIncDec(property.clone()),
                );
            }
            PropertyWriteSelection::None
            | PropertyWriteSelection::ClassifierProperty(_)
            | PropertyWriteSelection::MissingContext(..)
            | PropertyWriteSelection::AccessorAmbiguous(_)
            | PropertyWriteSelection::Ambiguous => {}
        }
    }
}

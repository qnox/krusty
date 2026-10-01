//! Ordered selection and recording for a property write through an explicit receiver
//! (`receiver.name = value`) once no accessor-derived member was applicable.
//!
//! The families follow Kotlin's scope tower: a declared member property, a member setter, a
//! read-only member, a member extension from an implicit dispatch receiver, and only then a
//! top-level extension property. A family is queried only while every higher-priority family is
//! absent, and only the selected family records facts. In particular an extension's receiver
//! constraint on a postponed builder call belongs to the extension's selection: a shadowed
//! `B<String>.p` must not fix `T` when the member `B<T>.p` wins the write.

use super::*;

/// The statement being checked and its already-typed value.
struct ExplicitWriteSite<'n> {
    stmt: StmtId,
    receiver: ExprId,
    /// The receiver's type as the write sees it (non-null after `?.`).
    receiver_ty: Ty,
    name: &'n str,
    value: ExprId,
    value_ty: Ty,
}

struct SourcePropertyWrite {
    owner: TypeName,
    ty: Ty,
    is_var: bool,
    setter_visibility: Option<Visibility>,
    stable_declaration: Option<crate::fir::DeclarationId>,
}

enum ExplicitPropertyWrite {
    Source(SourcePropertyWrite),
    /// A member setter. A read-only declared property of the same name still supplies the value's
    /// expected type.
    Setter {
        setter: Box<crate::symbol_resolver::ResolvedPropertySetter>,
        declared_ty: Option<Ty>,
    },
    ReadOnlyMember(Ty),
    MemberExtension(Box<MemberExtensionProperty>),
    MemberExtensionAmbiguous,
    Extension(Box<crate::libraries::PropertyInfo>),
    ExtensionAmbiguous,
    None,
}

impl ExplicitPropertyWrite {
    fn expected_value_ty(&self) -> Option<Ty> {
        match self {
            Self::Source(property) => Some(property.ty),
            Self::Setter {
                setter,
                declared_ty,
            } => declared_ty.or_else(|| setter.callable.params.first().copied()),
            Self::ReadOnlyMember(ty) => Some(*ty),
            Self::MemberExtension(property) => Some(property.ty),
            Self::Extension(property) => Some(property.ty),
            Self::MemberExtensionAmbiguous | Self::ExtensionAmbiguous | Self::None => None,
        }
    }
}

impl Checker<'_> {
    /// Select the write target of `receiver.name` on the non-accessor member rungs, in priority
    /// order. Nothing is recorded here.
    fn select_explicit_property_write(
        &self,
        scope: &CheckerScope<'_>,
        rt: Ty,
        name: &str,
    ) -> ExplicitPropertyWrite {
        let source_property = if rt.is_nullable() {
            None
        } else {
            rt.obj_internal()
                .and_then(|_| self.lookup_prop_with_owner_name(rt, name))
        };
        let setter = (!rt.is_nullable())
            .then(|| self.select_property_setter(rt, name))
            .flatten();
        if let Some((owner, ty, is_var, setter_visibility, stable_declaration)) = source_property {
            if is_var || setter.is_none() {
                return ExplicitPropertyWrite::Source(SourcePropertyWrite {
                    owner,
                    ty,
                    is_var,
                    setter_visibility,
                    stable_declaration,
                });
            }
        }
        if let Some(setter) = setter {
            return ExplicitPropertyWrite::Setter {
                setter: Box::new(setter),
                declared_ty: source_property.map(|(_, ty, _, _, _)| ty),
            };
        }
        if let Some(property) = (!rt.is_nullable())
            .then(|| self.select_property_member(rt, name))
            .flatten()
        {
            return ExplicitPropertyWrite::ReadOnlyMember(property.ret);
        }
        match self.member_extension_property(scope, rt, name) {
            Ok(Some(property)) => {
                return ExplicitPropertyWrite::MemberExtension(Box::new(property))
            }
            Err(()) => return ExplicitPropertyWrite::MemberExtensionAmbiguous,
            Ok(None) => {}
        }
        let type_variables = self.postponed_type_variables_in(rt);
        let resolver = self.resolver().with_type_variables(&type_variables);
        match resolver.select_extension_property(rt, name) {
            Ok(Some(property)) => ExplicitPropertyWrite::Extension(Box::new(property)),
            Ok(None) => ExplicitPropertyWrite::None,
            Err(_) => ExplicitPropertyWrite::ExtensionAmbiguous,
        }
    }

    /// Check and record `receiver.name = value` through the selected non-accessor member rung.
    pub(super) fn assign_explicit_receiver_property(
        &mut self,
        scope: &CheckerScope<'_>,
        s: StmtId,
        receiver: ExprId,
        name: &str,
        value: ExprId,
        rt: Ty,
    ) {
        let selection = self.select_explicit_property_write(scope, rt, name);
        crate::trace_compiler!(
            "resolve",
            "member assignment name={name} receiver={rt:?} selected={}",
            match &selection {
                ExplicitPropertyWrite::Source(_) => "source",
                ExplicitPropertyWrite::Setter { .. } => "member setter",
                ExplicitPropertyWrite::ReadOnlyMember(_) => "read-only member",
                ExplicitPropertyWrite::MemberExtension(_) => "member extension",
                ExplicitPropertyWrite::MemberExtensionAmbiguous => "ambiguous member extension",
                ExplicitPropertyWrite::Extension(_) => "extension",
                ExplicitPropertyWrite::ExtensionAmbiguous => "ambiguous extension",
                ExplicitPropertyWrite::None => "none",
            },
        );
        let vt = match selection.expected_value_ty() {
            Some(expected) => self.expr_expected(scope, value, expected),
            None => self.expr(scope, value),
        };
        let span = self.file.stmt_spans[s.0 as usize];
        let target_span = self.assignment_target_span(s);
        let site = ExplicitWriteSite {
            stmt: s,
            receiver,
            receiver_ty: rt,
            name,
            value,
            value_ty: vt,
        };
        match selection {
            ExplicitPropertyWrite::Source(property) => {
                self.write_source_property(scope, &site, property);
            }
            ExplicitPropertyWrite::Setter { setter, .. } => {
                let setter_declaration = setter.stable_declaration;
                let callable = setter.callable;
                let pty = callable.params.first().copied().unwrap_or(Ty::Error);
                self.expect_assignable(
                    pty,
                    vt,
                    self.value_diagnostic_span(value, vt),
                    "assignment",
                );
                let owner = callable.owner;
                if setter.visibility != Visibility::Public {
                    self.reject_if_inaccessible(setter.visibility, name, owner, target_span);
                }
                self.stmt_lowers.insert(
                    s,
                    StmtLowering::MemberPropertyWrite {
                        stable_declaration: setter_declaration,
                        backing_field: false,
                        setter: Some(Box::new(callable)),
                        setter_declaration,
                        owner,
                        ty: pty,
                        interface: self.resolved_owner_is_interface(owner),
                        context_access: None,
                    },
                );
            }
            ExplicitPropertyWrite::ReadOnlyMember(_) => {
                self.report_val_reassignment(target_span, "'val' cannot be reassigned.");
            }
            ExplicitPropertyWrite::MemberExtension(property) => {
                self.write_member_extension_property(scope, &site, *property);
            }
            ExplicitPropertyWrite::MemberExtensionAmbiguous => {
                self.diags.error(
                    span,
                    format!("overload resolution ambiguity for member '{name}'"),
                );
            }
            ExplicitPropertyWrite::Extension(property) => {
                self.write_extension_property(scope, &site, *property);
            }
            ExplicitPropertyWrite::ExtensionAmbiguous => self.diags.error(
                span,
                format!("overload resolution ambiguity for extension property '{name}'"),
            ),
            ExplicitPropertyWrite::None => match rt {
                Ty::Error => {}
                Ty::Obj(..) => {
                    let hidden_deprecated = self
                        .resolver()
                        .receiver_has_hidden_deprecated_member(rt, name);
                    self.diags
                        .error(span, unresolved_member_message(name, rt, hidden_deprecated))
                }
                _ => self.diags.error(
                    span,
                    format!("cannot assign to a member of '{}'", rt.source_name()),
                ),
            },
        }
    }

    fn write_source_property(
        &mut self,
        scope: &CheckerScope<'_>,
        site: &ExplicitWriteSite<'_>,
        property: SourcePropertyWrite,
    ) {
        let SourcePropertyWrite {
            owner,
            ty,
            is_var,
            setter_visibility,
            stable_declaration,
        } = property;
        let target_span = self.assignment_target_span(site.stmt);
        // A deferred `val` has no setter, but each constructor may initialize its backing field.
        // Constructor scopes publish that one permission on the existing dispatch-property
        // binding. Explicit `this.p = …` must consume the same binding as bare `p = …`; looking
        // only at the class declaration here incorrectly turns the initialization into a
        // reassignment.
        let deferred_constructor_write = !is_var
            && self.is_deferred_constructor_property_write(scope, site.receiver, site.name, owner);
        if !is_var && !deferred_constructor_write {
            self.report_val_reassignment(target_span, "'val' cannot be reassigned.");
        } else if let Some(visibility) = setter_visibility {
            if visibility != Visibility::Public {
                self.reject_if_inaccessible(visibility, site.name, owner, target_span);
            }
        }
        self.expect_assignable(
            ty,
            site.value_ty,
            self.value_diagnostic_span(site.value, site.value_ty),
            "assignment",
        );
        if is_var || deferred_constructor_write {
            let lowering = StmtLowering::MemberPropertyWrite {
                stable_declaration,
                backing_field: deferred_constructor_write,
                setter: None,
                setter_declaration: None,
                owner,
                ty,
                interface: self.resolved_owner_is_interface(owner),
                context_access: None,
            };
            self.stmt_lowers.insert(site.stmt, lowering);
        }
    }

    fn write_member_extension_property(
        &mut self,
        scope: &CheckerScope<'_>,
        site: &ExplicitWriteSite<'_>,
        property: MemberExtensionProperty,
    ) {
        let target_span = self.assignment_target_span(site.stmt);
        // A write is governed by the setter, not merely by the visibility of the readable
        // property. Resolve that semantic fact here for every implicit-dispatch origin; lowering
        // receives only an already-authorized accessor plan.
        let write_visibility = property.setter_visibility.unwrap_or(property.visibility);
        if write_visibility != Visibility::Public {
            self.reject_if_inaccessible(write_visibility, site.name, property.owner, target_span);
        }
        self.mark_extension_receiver_stmt_used(site.stmt, property.dispatch_receiver);
        for source in &property.context_args {
            let ResolvedContextArgument::ImplicitReceiver(selected) = source else {
                continue;
            };
            if let Some(receiver) = self
                .implicit_receivers(scope)
                .into_iter()
                .find(|receiver| receiver.ty == selected.ty && receiver.current == selected.current)
            {
                self.mark_extension_receiver_stmt_used(site.stmt, receiver);
            }
        }
        if !property.is_var {
            self.report_val_reassignment(target_span, "'val' cannot be reassigned.");
        }
        self.expect_assignable(
            property.ty,
            site.value_ty,
            self.value_diagnostic_span(site.value, site.value_ty),
            "assignment",
        );
        self.stmt_lowers.insert(
            site.stmt,
            StmtLowering::MemberExtensionPropertyWrite {
                stable_declaration: property.stable_declaration,
                setter: property.setter.map(Box::new),
                dispatch_receiver: self.implicit_receiver_selection(property.dispatch_receiver),
                owner: property.owner,
                receiver: property.declared_receiver,
                ty: property.ty,
                context_params: property.context_params,
                context_args: property.context_args,
            },
        );
    }

    /// Record the selected top-level extension-property write. Only here, once the extension has
    /// won the write, does a receiver over a postponed call's type variables add its constraint
    /// against the declared receiver to that call.
    fn write_extension_property(
        &mut self,
        scope: &CheckerScope<'_>,
        site: &ExplicitWriteSite<'_>,
        property: crate::libraries::PropertyInfo,
    ) {
        let target_span = self.assignment_target_span(site.stmt);
        if let Some(declared) = property.receiver {
            if self.postponed_call_mentions(site.receiver_ty) {
                let span = self.span(site.receiver);
                self.expect_assignable(declared, site.receiver_ty, span, "extension receiver");
            }
        }
        if property.setter.is_none() {
            self.report_val_reassignment(target_span, "'val' cannot be reassigned.");
        }
        self.expect_assignable(
            property.ty,
            site.value_ty,
            self.value_diagnostic_span(site.value, site.value_ty),
            "assignment",
        );
        let context_args = if property.context_count == 0 {
            Vec::new()
        } else {
            let Some(context_args) = property
                .getter
                .params
                .get(1..1 + property.context_count)
                .and_then(|context_types| self.select_context_arguments(scope, context_types))
            else {
                self.diags.error(
                    target_span,
                    format!("No context argument for '{}' found.", site.name),
                );
                return;
            };
            context_args
        };
        self.stmt_lowers.insert(
            site.stmt,
            StmtLowering::ExtensionPropertyWrite {
                access: Box::new(ResolvedPropertyAccess {
                    property,
                    context_args,
                }),
            },
        );
    }
}

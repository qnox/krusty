//! kotlinc's `FirMissingDependencyClassChecker`: a qualified access whose selected declaration
//! names a class no classpath entry or source provides reports `MISSING_DEPENDENCY_CLASS`.
//!
//! The declaration's return type is examined first, type arguments included, then its value
//! parameter types; a callable reference examines its return type only. Each missing class is
//! reported once per access, at the selected name: the class name of a constructor call. A type
//! parameter bound is not examined. A none-applicable overload family has no resolved callee, so
//! it reports nothing, including when one of the rejected candidates names a missing class.
//!
//! Such a class reaches a signature from a classpath class compiled against a dependency that is
//! absent, or from a Java source of the module whose type does not resolve: the source header
//! keeps that type under its written name, as kotlinc's Java model does, so `Absent` here names the
//! class `Absent` rather than a fabricated `Object`. Only a dependency declaration can name such a
//! class: the current module's declarations name theirs in source, where an unresolved one is an
//! unresolved reference.

use super::super::*;
use crate::types::TyKind;

impl Checker<'_> {
    /// Report the missing classes the declaration selected at `expression` names.
    pub(super) fn check_expression_missing_classes(&mut self, expression: ExprId) {
        if self.discover_anonymous_captures {
            return;
        }
        let Some((span, types)) = self.selected_signature_types(expression) else {
            return;
        };
        let mut missing = Vec::new();
        for ty in types {
            self.collect_missing_classes(ty, &mut missing);
        }
        for class in missing {
            let class = missing_class_display(class);
            self.diags.error_named(
                span,
                crate::diag::names::MISSING_DEPENDENCY_CLASS,
                format!(
                    "Cannot access class '{class}'. Check your module classpath for missing or \
                     conflicting dependencies."
                ),
            );
        }
    }

    /// Classes `ty` names that nothing declares, in first-occurrence order, without repeats.
    fn collect_missing_classes(&self, ty: Ty, missing: &mut Vec<TypeName>) {
        match ty.kind() {
            TyKind::Obj(class, arguments) => {
                // A builtin type is always present, as kotlinc's builtins are with or without a
                // standard library on the classpath.
                if !missing.contains(&class)
                    && crate::types::builtin_semantic(class).is_none()
                    && self.fed_source().classifier(class).is_none()
                {
                    missing.push(class);
                }
                for argument in arguments.iter() {
                    self.collect_missing_classes(*argument, missing);
                }
            }
            TyKind::Nullable(inner)
            | TyKind::PlatformNullable(inner)
            | TyKind::MutabilityFlexible(inner)
            | TyKind::InProjection(inner)
            | TyKind::OutProjection(inner)
            | TyKind::StarProjection(inner) => self.collect_missing_classes(inner, missing),
            _ => {}
        }
    }

    /// Where an access reports, and the declared types kotlinc examines for it: the return type,
    /// then the value parameters (a callable reference: its return type alone). Only a dependency
    /// declaration is examined; see [`dependency`].
    fn selected_signature_types(&self, expression: ExprId) -> Option<(Span, Vec<Ty>)> {
        match self.file.expr(expression) {
            Expr::Call { callee, .. } => {
                let span = self.missing_supertype_callee_span(*callee);
                if let Some(types) = self.rejected_dependency_signatures.get(&expression) {
                    if !self.resolved_calls.contains_key(&expression)
                        && !self.resolved_constructors.contains_key(&expression)
                    {
                        return Some((span, types.clone()));
                    }
                }
                if let Some(constructor) = self.resolved_constructors.get(&expression) {
                    let member = match constructor {
                        ResolvedConstructor::Source { .. } => return None,
                        ResolvedConstructor::Plain { member, .. }
                        | ResolvedConstructor::PlainSlots { member, .. } => member,
                        ResolvedConstructor::Synthetic { ctor, .. } => &ctor.declaration,
                    };
                    dependency(member.external_identity)?;
                    return Some((span, member.params.clone()));
                }
                let (identity, ret, params) = match self.resolved_calls.get(&expression)? {
                    ResolvedCall::Member(selected) => (
                        selected.member.external_identity,
                        selected.member.ret,
                        &selected.member.params,
                    ),
                    ResolvedCall::Companion(member) => {
                        (member.external_identity, member.ret, &member.params)
                    }
                    ResolvedCall::Extension(call) => (
                        call.callable.external_identity,
                        call.callable.ret,
                        &call.callable.params,
                    ),
                    ResolvedCall::TopLevel(call) => (
                        call.callable.external_identity,
                        call.callable.ret,
                        &call.callable.params,
                    ),
                    ResolvedCall::MemberExtension {
                        external_identity,
                        ret,
                        params,
                        ..
                    } => (*external_identity, *ret, params),
                    ResolvedCall::LocalFunction(_) => return None,
                };
                dependency(identity)?;
                Some((
                    span,
                    std::iter::once(ret).chain(params.iter().copied()).collect(),
                ))
            }
            // `value[index]` is the selected `get`, or a built-in array read that records no call.
            // A declaration-site `List<Absent>` or `Absent[]` names the element only after
            // substitution, so the expression's type is examined as well as the selected signature.
            Expr::Index { array, .. } => {
                let span = self.span(expression);
                let mut types = self
                    .indexed_or_safe_call_types(expression, span)
                    .map(|(_, types)| types)
                    .unwrap_or_default();
                if types.is_empty() {
                    let receiver = self.expr_types.get(array.0 as usize).copied()?;
                    receiver.array_elem()?;
                }
                if let Some(element) = self.expr_types.get(expression.0 as usize).copied() {
                    types.push(element);
                }
                (!types.is_empty()).then_some((span, types))
            }
            Expr::SafeCall {
                name,
                args: Some(_),
                ..
            } => {
                self.indexed_or_safe_call_types(expression, self.member_name_span(expression, name))
            }
            Expr::Member { name, .. }
            | Expr::SafeCall {
                name, args: None, ..
            } => {
                let selected_dependency = match self.expr_lowers.get(&expression)? {
                    ExprLowering::AssociatedPropertyRead {
                        external_identity, ..
                    } => external_identity.is_some(),
                    ExprLowering::ClassifierPropertyRead { property, .. } => property
                        .getter
                        .as_ref()
                        .is_some_and(|getter| getter.external_identity.is_some()),
                    ExprLowering::MemberPropertyRead {
                        stable_declaration,
                        source_member,
                        ..
                    } => stable_declaration.is_none() && source_member.is_none(),
                    ExprLowering::MemberExtensionPropertyRead { getter, .. } => getter
                        .as_ref()
                        .is_some_and(|getter| getter.external_identity.is_some()),
                    ExprLowering::ExtensionPropertyGet { access }
                    | ExprLowering::TopLevelPropertyGet(access) => {
                        access.property.getter.external_identity.is_some()
                    }
                    _ => false,
                };
                selected_dependency.then(|| {
                    (
                        self.member_name_span(expression, name),
                        vec![self.expr_types[expression.0 as usize]],
                    )
                })
            }
            Expr::CallableRef { name, .. } => {
                let (identity, ret) = match self.expr_lowers.get(&expression)? {
                    ExprLowering::CallableReference { target, .. }
                    | ExprLowering::AdaptedCallableReference { target, .. } => match target {
                        CallableReferenceTarget::Member { member, .. } => {
                            (member.external_identity, member.ret)
                        }
                        CallableReferenceTarget::Extension { callable, .. } => {
                            (callable.external_identity, callable.ret)
                        }
                        CallableReferenceTarget::Property(property) => {
                            (property.getter.external_identity, property.getter.ret)
                        }
                        _ => return None,
                    },
                    _ => return None,
                };
                dependency(identity)?;
                let span = self.span(expression);
                let span = Span::new(span.hi.saturating_sub(name.len() as u32), span.hi);
                Some((span, vec![ret]))
            }
            _ => None,
        }
    }

    /// Signature types of a selected dependency `get` or safe call, using the substituted return
    /// type so a type argument such as `Absent` is visible.
    fn indexed_or_safe_call_types(
        &self,
        expression: ExprId,
        span: Span,
    ) -> Option<(Span, Vec<Ty>)> {
        if let Some(types) = self.rejected_dependency_signatures.get(&expression) {
            if !self.resolved_calls.contains_key(&expression) {
                return Some((span, types.clone()));
            }
        }
        let call = self.resolved_calls.get(&expression)?;
        let (identity, declared_params) = match call {
            ResolvedCall::Member(selected) => (
                selected.member.external_identity,
                selected.member.params.as_slice(),
            ),
            ResolvedCall::Companion(member) => (member.external_identity, member.params.as_slice()),
            ResolvedCall::Extension(call) => (
                call.callable.external_identity,
                call.callable.params.as_slice(),
            ),
            ResolvedCall::TopLevel(call) => (
                call.callable.external_identity,
                call.callable.params.as_slice(),
            ),
            ResolvedCall::MemberExtension {
                external_identity,
                params,
                ..
            } => (*external_identity, params.as_slice()),
            ResolvedCall::LocalFunction(_) => return None,
        };
        dependency(identity)?;
        let mut types = vec![call.ret()];
        types.extend(declared_params.iter().copied());
        if let Some(result) = self.expr_types.get(expression.0 as usize).copied() {
            types.push(result);
        }
        Some((span, types))
    }
}

/// `class` as a missing-class diagnostic names it. Unresolved Java evidence keeps the written
/// spelling; it is not the internal name of the placeholder package.
fn missing_class_display(class: TypeName) -> String {
    crate::jvm::java_stub::unresolved_evidence_spelling(class)
        .unwrap_or_else(|| access_control::owner_display(class))
}

/// Whether a selected declaration is a dependency's, which is the only kind whose signature can name
/// a class this compile lacks. A declaration of the current module names its classes in source, and
/// one it cannot resolve is reported there as an unresolved reference.
fn dependency<T>(external_identity: Option<T>) -> Option<()> {
    external_identity.map(|_| ())
}

//! Deprecated declarations: kotlinc's `FirDeprecationChecker`, `FirDeprecatedQualifierChecker` and
//! `FirDeprecatedTypeChecker` for this compilation's own declarations.
//!
//! A use of a declaration annotated `@Deprecated` reports `'<symbol>' is deprecated.<message>` at
//! the reference: the called or read name, the name written after `::`, the written target of an
//! assignment, a classifier qualifier, or a type reference. `WARNING` reports `DEPRECATION`, a
//! warning `@Suppress("DEPRECATION")` silences; `ERROR` and `HIDDEN` report the error
//! `DEPRECATION_ERROR`. A `HIDDEN` callable never reaches this check: candidate collection drops
//! it.
//!
//! A property's deprecation covers both its reads and its writes; a getter's covers its reads and
//! a setter's its writes. Both render the property. A constructor call reports once, for the more
//! severe of the constructor's and its classifier's deprecations, rendered as the constructor.
//! Uses inside a deprecated declaration are reported like any other.
//!
//! A dependency declaration's provider publishes the deprecation in effect with the declaration's
//! rendering ([`DeprecatedDeclaration`]); one of this compilation's declarations states it in its
//! checked annotations. A use reads whichever the selected declaration carries.

use super::symbol_display::source_declaration_display;
use super::*;
use crate::features::{DeprecationMigration, DeprecationMigrations};
use crate::fir::DeclarationKind;
use crate::types::{DeprecatedDeclaration, Deprecation, DeprecationLevel};
use std::sync::Arc;

/// One deprecated declaration a reference uses, with the rendering its diagnostic names.
pub(super) struct DeprecatedUse {
    pub(super) symbol: String,
    pub(super) deprecation: Deprecation,
}

/// Which accessor of a property a reference uses.
#[derive(Clone, Copy)]
enum PropertyAccess {
    Read,
    Write,
}

impl Checker<'_> {
    /// Report the deprecated declarations expression `expression` uses.
    pub(super) fn check_expression_deprecation(&mut self, expression: ExprId) {
        if self.discover_anonymous_captures {
            return;
        }
        if let Some((qualifier, classifier)) = self.deprecation_qualifier(expression) {
            let used = self.classifier_deprecated_use(classifier);
            self.report_deprecated_use(self.span(qualifier), used);
        }
        if matches!(
            self.file.expr(expression),
            Expr::Name(_) | Expr::Member { .. }
        ) {
            if let Some(target) = self.file.compound_assignment_read_spans.get(&expression.0) {
                let used = self.read_deprecated_use(expression);
                let migration = self
                    .implicit_companion_read(expression)
                    .then_some(DeprecationMigration::CompanionInDelegation);
                self.report_deprecated_use_as(*target, used, migration);
                return;
            }
        }
        let (span, used) = match self.file.expr(expression) {
            Expr::Call { callee, .. } => {
                let span = self.deprecation_callee_span(*callee);
                let used = match self.resolved_constructors.get(&expression) {
                    Some(constructor) => self.constructor_deprecated_use(constructor),
                    None => self
                        .resolved_calls
                        .get(&expression)
                        .and_then(|call| self.call_deprecated_use(call)),
                };
                self.check_implicit_invoke_receiver_deprecation(expression);
                (span, used)
            }
            Expr::SafeCall { name, args, .. } => {
                let used = if args.is_some() {
                    self.resolved_calls
                        .get(&expression)
                        .and_then(|call| self.call_deprecated_use(call))
                } else {
                    self.read_deprecated_use(expression)
                };
                (self.member_name_span(expression, name), used)
            }
            Expr::Member { name, .. } => (
                self.member_name_span(expression, name),
                self.read_deprecated_use(expression),
            ),
            Expr::Name(_) => (self.span(expression), self.read_deprecated_use(expression)),
            Expr::CallableRef { name, .. } => {
                let span = self.span(expression);
                (
                    Span::new(span.hi.saturating_sub(name.len() as u32), span.hi),
                    self.expr_lowers
                        .get(&expression)
                        .and_then(|lowering| self.reference_deprecated_use(lowering)),
                )
            }
            _ => return,
        };
        self.report_deprecated_use(span, used);
    }

    /// Report the deprecated property assignment statement `statement` writes, at its written
    /// target name.
    pub(super) fn check_property_write_deprecation(&mut self, statement: StmtId) {
        if self.discover_anonymous_captures {
            return;
        }
        let span = match self.file.stmt(statement) {
            Stmt::Assign { name, .. } => self.assignment_member_name_span(statement, name),
            Stmt::AssignMember { name, .. } => self.assignment_member_name_span(statement, name),
            _ => return,
        };
        let (declaration, setter) = match self.stmt_lowers.get(&statement) {
            Some(
                StmtLowering::MemberPropertyWrite {
                    stable_declaration,
                    setter,
                    ..
                }
                | StmtLowering::MemberExtensionPropertyWrite {
                    stable_declaration,
                    setter,
                    ..
                },
            ) => (*stable_declaration, setter.as_deref()),
            Some(StmtLowering::AssociatedPropertyWrite {
                stable_declaration, ..
            }) => (*stable_declaration, None),
            Some(
                StmtLowering::ExtensionPropertyWrite { access }
                | StmtLowering::TopLevelPropertySet(access),
            ) => (
                access.property.stable_declaration,
                access.property.setter.as_ref(),
            ),
            Some(StmtLowering::ImplicitPropertyWrite(target)) => match target.as_ref() {
                ImplicitPropertyWriteTarget::Member {
                    stable_declaration, ..
                } => (*stable_declaration, None),
                ImplicitPropertyWriteTarget::Extension { access, .. } => (
                    access.property.stable_declaration,
                    access.property.setter.as_ref(),
                ),
            },
            _ => (None, None),
        };
        let used = match declaration {
            Some(declaration) => self.property_deprecated_use(declaration, PropertyAccess::Write),
            None => {
                dependency_deprecated_use(setter.and_then(|setter| setter.deprecation.as_ref()))
            }
        };
        self.report_deprecated_use(span, used);
    }

    /// The deprecated classifier a written type reference names.
    pub(super) fn check_type_reference_deprecation(&mut self, reference: &TypeRef, resolved: Ty) {
        if self.discover_anonymous_captures || reference.is_import() || reference.is_annotation() {
            return;
        }
        let Some(classifier) = resolved.non_null().kotlin_class_internal() else {
            return;
        };
        let used = self.classifier_deprecated_use(classifier);
        self.report_deprecated_use(reference.span, used);
    }

    /// [`Self::check_type_reference_deprecation`] for a declaration type finalized in Pass 1 and
    /// each type argument written in it.
    pub(super) fn check_finalized_type_reference_deprecation(
        &mut self,
        reference: &TypeRef,
        resolved: Ty,
    ) {
        self.check_type_reference_deprecation(reference, resolved);
        let arguments = resolved.non_null().type_args();
        for (argument, resolved) in reference.targs.iter().zip(arguments) {
            if !argument.is_star_projection() {
                self.check_finalized_type_reference_deprecation(
                    argument,
                    resolved.projection_read_ty(),
                );
            }
        }
    }

    /// The classifier qualifier a call is selected through (`Obj` in `Obj.f()`, `Outer` in
    /// `Outer.Nested()`). A qualified read reports its qualifier when the qualifier itself is
    /// checked, as the singleton read it also is.
    fn deprecation_qualifier(&self, expression: ExprId) -> Option<(ExprId, TypeName)> {
        if !matches!(self.file.expr(expression), Expr::Call { .. }) {
            return None;
        }
        self.classifier_qualifier(expression)
    }

    fn deprecation_callee_span(&self, callee: ExprId) -> Span {
        match self.file.expr(callee) {
            Expr::Member { name, .. } => self.member_name_span(callee, name),
            _ => self.span(callee),
        }
    }

    /// The deprecated declaration a property read or singleton read uses.
    fn read_deprecated_use(&self, expression: ExprId) -> Option<DeprecatedUse> {
        let (declaration, getter) = match self.expr_lowers.get(&expression) {
            None => (self.constant_property_reads.get(&expression).copied(), None),
            Some(lowering) => match lowering {
                ExprLowering::MemberPropertyRead {
                    stable_declaration,
                    accessor,
                    ..
                } => (*stable_declaration, accessor.as_deref()),
                ExprLowering::MemberExtensionPropertyRead {
                    stable_declaration,
                    getter,
                    ..
                } => (*stable_declaration, getter.as_deref()),
                ExprLowering::AssociatedPropertyRead {
                    stable_declaration, ..
                } => (*stable_declaration, None),
                ExprLowering::TopLevelPropertyGet(access)
                | ExprLowering::ExtensionPropertyGet { access } => (
                    access.property.stable_declaration,
                    Some(&access.property.getter),
                ),
                ExprLowering::SingletonValue(singleton) => {
                    return self.classifier_deprecated_use(singleton.classifier)
                }
                _ => (None, None),
            },
        };
        match declaration {
            Some(declaration) => self.property_deprecated_use(declaration, PropertyAccess::Read),
            None => {
                dependency_deprecated_use(getter.and_then(|getter| getter.deprecation.as_ref()))
            }
        }
    }

    fn reference_deprecated_use(&self, lowering: &ExprLowering) -> Option<DeprecatedUse> {
        let (declaration, dependency) = match lowering {
            ExprLowering::ConstructorRef { constructor, .. } => (
                constructor.stable_declaration,
                constructor.deprecation.as_ref(),
            ),
            ExprLowering::TopLevelFunctionRef(reference) => (
                reference.stable_declaration,
                reference.target.deprecation.as_ref(),
            ),
            ExprLowering::AdaptedRef {
                stable_declaration,
                target,
                ..
            } => (*stable_declaration, target.deprecation.as_ref()),
            ExprLowering::CallableReference { target, .. }
            | ExprLowering::AdaptedCallableReference { target, .. } => match target {
                CallableReferenceTarget::Classifier(member)
                | CallableReferenceTarget::Member { member, .. } => {
                    (member.stable_declaration, member.deprecation.as_ref())
                }
                CallableReferenceTarget::Extension {
                    callable,
                    stable_declaration,
                } => (*stable_declaration, callable.deprecation.as_ref()),
                CallableReferenceTarget::Property(property) => (
                    property.stable_declaration,
                    property.getter.deprecation.as_ref(),
                ),
                CallableReferenceTarget::TopLevelProperty(property) => (
                    property.stable_declaration,
                    property.getter.deprecation.as_ref(),
                ),
                CallableReferenceTarget::ClassifierProperty { .. } => (None, None),
            },
            _ => (None, None),
        };
        match declaration {
            Some(declaration) => self.declaration_deprecated_use(declaration),
            None => dependency_deprecated_use(dependency),
        }
    }

    /// The deprecated declaration a call selected.
    fn call_deprecated_use(&self, call: &ResolvedCall) -> Option<DeprecatedUse> {
        if let Some(declaration) = call_declaration(call) {
            return self.declaration_deprecated_use(declaration);
        }
        dependency_deprecated_use(match call {
            ResolvedCall::Member(selected) => selected.member.deprecation.as_ref(),
            ResolvedCall::Companion(member) => member.deprecation.as_ref(),
            ResolvedCall::TopLevel(call) => call.callable.deprecation.as_ref(),
            ResolvedCall::Extension(call) => call.callable.deprecation.as_ref(),
            ResolvedCall::MemberExtension { deprecation, .. } => deprecation.as_ref(),
            ResolvedCall::LocalFunction(_) => None,
        })
    }

    /// The deprecated declaration a constructor call uses: the more severe of the constructor's
    /// own deprecation and its classifier's, rendered as the constructor.
    fn constructor_deprecated_use(
        &self,
        constructor: &ResolvedConstructor,
    ) -> Option<DeprecatedUse> {
        let declaration = match constructor {
            ResolvedConstructor::Source {
                stable_declaration, ..
            } => *stable_declaration,
            ResolvedConstructor::Plain { member, .. }
            | ResolvedConstructor::PlainSlots { member, .. } => match member.stable_declaration {
                Some(declaration) => Some(declaration),
                None => return dependency_deprecated_use(member.deprecation.as_ref()),
            },
            ResolvedConstructor::Synthetic { ctor, .. } => ctor.declaration.stable_declaration,
        }?;
        let index = self.resolved_index?;
        let own = self.source_deprecation(declaration);
        let classifier = index
            .classifier_declaration(constructor.owner())
            .and_then(|classifier| self.source_deprecation(classifier));
        let deprecation = match (own, classifier) {
            (Some(own), Some(classifier)) if classifier.level > own.level => classifier,
            (Some(own), _) => own,
            (None, classifier) => classifier?,
        };
        Some(DeprecatedUse {
            symbol: source_declaration_display(index, declaration)?,
            deprecation,
        })
    }

    /// A property's deprecation for `access`: the selected accessor overrides the property's own
    /// deprecation, as it does in kotlinc's declaration use-site checker.
    fn property_deprecated_use(
        &self,
        declaration: crate::fir::DeclarationId,
        access: PropertyAccess,
    ) -> Option<DeprecatedUse> {
        let index = self.resolved_index?;
        let accessor = match access {
            PropertyAccess::Read => 0,
            PropertyAccess::Write => 1,
        };
        let deprecation = index
            .owned_declaration(declaration, DeclarationKind::Accessor, accessor)
            .and_then(|accessor| self.source_deprecation(accessor))
            .or_else(|| self.source_deprecation(declaration))?;
        Some(DeprecatedUse {
            symbol: source_declaration_display(index, declaration)?,
            deprecation,
        })
    }

    fn declaration_deprecated_use(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<DeprecatedUse> {
        let index = self.resolved_index?;
        if index.declaration_header(declaration)?.kind == DeclarationKind::Property {
            return self.property_deprecated_use(declaration, PropertyAccess::Read);
        }
        Some(DeprecatedUse {
            deprecation: self.source_deprecation(declaration)?,
            symbol: source_declaration_display(index, declaration)?,
        })
    }

    fn classifier_deprecated_use(&self, classifier: TypeName) -> Option<DeprecatedUse> {
        if let Some(declaration) = self
            .resolved_index
            .and_then(|index| index.classifier_declaration(classifier))
        {
            return self.declaration_deprecated_use(declaration);
        }
        dependency_deprecated_use(self.resolver().classifier(classifier)?.deprecation.as_ref())
    }

    /// The deprecation a source declaration's checked annotations declare. A common source's
    /// reference binds to the `expect` header that an `actual` (or a member of one) replaces, so
    /// the implementation's deprecation does not apply there.
    fn source_deprecation(&self, declaration: crate::fir::DeclarationId) -> Option<Deprecation> {
        let index = self.resolved_index?;
        if index.binds_expectation(
            crate::fir::SourceFileId::from_raw(self.file_index),
            declaration,
        ) {
            return None;
        }
        index
            .declaration_applied_annotations(declaration)
            .iter()
            .find_map(crate::types::ResolvedAnnotation::deprecation)
    }

    fn report_deprecated_use(&mut self, span: Span, used: Option<DeprecatedUse>) {
        self.report_deprecated_use_as(span, used, None);
    }

    fn report_deprecated_use_as(
        &mut self,
        span: Span,
        used: Option<DeprecatedUse>,
        migration: Option<DeprecationMigration>,
    ) {
        emit_deprecated_use(
            self.diags,
            &self.file.deprecation_migrations,
            &self.active_lexical_policies,
            span,
            used,
            migration,
        );
    }

    /// Report the selected object receiver of an implicit `invoke`. A companion selected through
    /// its class value is excluded by retained selection provenance, not an owner/name query.
    fn check_implicit_invoke_receiver_deprecation(&mut self, call: ExprId) {
        let Some(ExprLowering::Invoke { receiver, .. }) = self.expr_lowers.get(&call) else {
            return;
        };
        let receiver = *receiver;
        if !matches!(
            self.file.expr(receiver),
            Expr::Name(_) | Expr::Member { .. }
        ) {
            return;
        }
        let Some(ExprLowering::SingletonValue(singleton)) = self.expr_lowers.get(&receiver) else {
            return;
        };
        if singleton.companion_owner.is_some() {
            return;
        }
        let used = self.classifier_deprecated_use(singleton.classifier);
        let span = match self.file.expr(receiver) {
            Expr::Member { name, .. } => self.member_name_span(receiver, name),
            _ => self.span(receiver),
        };
        self.report_deprecated_use_as(
            span,
            used,
            Some(DeprecationMigration::ClassifierInImplicitInvoke),
        );
    }

    fn implicit_companion_read(&self, expression: ExprId) -> bool {
        matches!(
            self.expr_lowers.get(&expression),
            Some(ExprLowering::SingletonValue(SingletonValue {
                companion_owner: Some(_),
                ..
            }))
        )
    }
}

/// Resolve a classifier's published deprecation from its already-selected identity.
pub(super) fn classifier_deprecated_use_by_identity(
    index: &crate::fir::ResolvedModuleIndex,
    libraries: &dyn SemanticPlatform,
    file_index: u32,
    classifier: TypeName,
) -> Option<DeprecatedUse> {
    if let Some(declaration) = index.classifier_declaration(classifier) {
        if index.binds_expectation(crate::fir::SourceFileId::from_raw(file_index), declaration) {
            return None;
        }
        let deprecation = index
            .declaration_applied_annotations(declaration)
            .iter()
            .find_map(crate::types::ResolvedAnnotation::deprecation)?;
        return Some(DeprecatedUse {
            symbol: source_declaration_display(index, declaration)?,
            deprecation,
        });
    }
    dependency_deprecated_use(libraries.classifier(classifier)?.deprecation.as_ref())
}

/// Emit an already-bound declaration use under finalized lexical and migration policies.
pub(super) fn emit_deprecated_use(
    diags: &mut DiagSink,
    migrations: &DeprecationMigrations,
    policies: &[LexicalPolicy],
    span: Span,
    used: Option<DeprecatedUse>,
    migration: Option<DeprecationMigration>,
) {
    let Some(DeprecatedUse {
        symbol,
        deprecation,
    }) = used
    else {
        return;
    };
    let message = deprecated_use_message(&symbol, &deprecation);
    let suppresses = |diagnostic: &str| {
        policies.iter().rev().any(|policy| {
                matches!(policy, LexicalPolicy::Suppress(suppressed) if suppressed == diagnostic)
            })
    };
    match deprecation.level {
        DeprecationLevel::Warning => {
            if !suppresses("DEPRECATION") {
                diags.warning_named(span, crate::diag::CompilerWarningName::Deprecation, message);
            }
        }
        DeprecationLevel::Error | DeprecationLevel::Hidden => {
            let warning = migration.and_then(|migration| {
                let policy = migrations.policy(migration);
                (!policy.completed)
                    .then(|| policy.warning_format.as_ref())
                    .flatten()
            });
            if let Some(format) = warning {
                // This warning stands in for the future error and is not suppressible as a
                // regular `DEPRECATION`/`DEPRECATION_ERROR` report in kotlinc.
                diags.warning_named(
                    span,
                    crate::diag::CompilerWarningName::DeprecationErrorMigrationPeriodWarning,
                    format.message(&message),
                );
            } else if !suppresses("DEPRECATION_ERROR") {
                diags.error(span, message);
            }
        }
    }
}

/// What a dependency declaration's provider published for its uses.
fn dependency_deprecated_use(
    published: Option<&Arc<DeprecatedDeclaration>>,
) -> Option<DeprecatedUse> {
    let published = published?;
    Some(DeprecatedUse {
        symbol: published.symbol.to_string(),
        deprecation: published.deprecation.clone(),
    })
}

/// The selected declaration of a call, when this compilation declares it.
fn call_declaration(call: &ResolvedCall) -> Option<crate::fir::DeclarationId> {
    match call {
        ResolvedCall::Member(selected) => selected.member.stable_declaration,
        ResolvedCall::Companion(member) => member.stable_declaration,
        ResolvedCall::TopLevel(call) => call.stable_declaration,
        ResolvedCall::Extension(call) => call.stable_declaration,
        ResolvedCall::MemberExtension {
            stable_declaration, ..
        } => *stable_declaration,
        ResolvedCall::LocalFunction(_) => None,
    }
}

fn deprecated_use_message(symbol: &str, deprecation: &Deprecation) -> String {
    format!(
        "'{symbol}' is deprecated.{}",
        optional_sentence(deprecation.message.as_deref())
    )
}

/// kotlinc's `OPTIONAL_SENTENCE`: nothing for a blank message, otherwise a space, the trimmed
/// message, and a closing period unless it already ends with one.
fn optional_sentence(message: Option<&str>) -> String {
    let Some(message) = message.map(str::trim).filter(|message| !message.is_empty()) else {
        return String::new();
    };
    if message.ends_with('.') {
        format!(" {message}")
    } else {
        format!(" {message}.")
    }
}

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

use super::symbol_display::source_declaration_display;
use super::*;
use crate::fir::DeclarationKind;
use crate::types::{Deprecation, DeprecationLevel};

/// One deprecated declaration a reference uses, with the rendering its diagnostic names.
struct DeprecatedUse {
    symbol: String,
    deprecation: Deprecation,
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
        let (span, used) = match self.file.expr(expression) {
            Expr::Call { callee, .. } => {
                let span = self.deprecation_callee_span(*callee);
                let used = match self.resolved_constructors.get(&expression) {
                    Some(constructor) => self.constructor_deprecated_use(constructor),
                    None => self
                        .resolved_calls
                        .get(&expression)
                        .and_then(call_declaration)
                        .and_then(|declaration| self.declaration_deprecated_use(declaration)),
                };
                (span, used)
            }
            Expr::SafeCall { name, args, .. } => {
                let used = if args.is_some() {
                    self.resolved_calls
                        .get(&expression)
                        .and_then(call_declaration)
                        .and_then(|declaration| self.declaration_deprecated_use(declaration))
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
        let declaration = match self.stmt_lowers.get(&statement) {
            Some(
                StmtLowering::MemberPropertyWrite {
                    stable_declaration, ..
                }
                | StmtLowering::AssociatedPropertyWrite {
                    stable_declaration, ..
                }
                | StmtLowering::MemberExtensionPropertyWrite {
                    stable_declaration, ..
                },
            ) => *stable_declaration,
            Some(
                StmtLowering::ExtensionPropertyWrite { access }
                | StmtLowering::TopLevelPropertySet(access),
            ) => access.property.stable_declaration,
            Some(StmtLowering::ImplicitPropertyWrite(target)) => match target.as_ref() {
                ImplicitPropertyWriteTarget::Member {
                    stable_declaration, ..
                } => *stable_declaration,
                ImplicitPropertyWriteTarget::Extension { access, .. } => {
                    access.property.stable_declaration
                }
            },
            _ => None,
        };
        let used = declaration.and_then(|declaration| {
            self.property_deprecated_use(declaration, PropertyAccess::Write)
        });
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
        let declaration = match self.expr_lowers.get(&expression) {
            None => self.constant_property_reads.get(&expression).copied(),
            Some(lowering) => match lowering {
                ExprLowering::MemberPropertyRead {
                    stable_declaration, ..
                }
                | ExprLowering::AssociatedPropertyRead {
                    stable_declaration, ..
                }
                | ExprLowering::MemberExtensionPropertyRead {
                    stable_declaration, ..
                } => *stable_declaration,
                ExprLowering::TopLevelPropertyGet(access)
                | ExprLowering::ExtensionPropertyGet { access } => {
                    access.property.stable_declaration
                }
                ExprLowering::SingletonValue(singleton) => {
                    return self.classifier_deprecated_use(singleton.classifier)
                }
                _ => None,
            },
        };
        self.property_deprecated_use(declaration?, PropertyAccess::Read)
    }

    fn reference_deprecated_use(&self, lowering: &ExprLowering) -> Option<DeprecatedUse> {
        let declaration = match lowering {
            ExprLowering::ConstructorRef { constructor, .. } => constructor.stable_declaration,
            ExprLowering::TopLevelFunctionRef(reference) => reference.stable_declaration,
            ExprLowering::AdaptedRef {
                stable_declaration, ..
            } => *stable_declaration,
            ExprLowering::CallableReference { target, .. }
            | ExprLowering::AdaptedCallableReference { target, .. } => match target {
                CallableReferenceTarget::Classifier(member)
                | CallableReferenceTarget::Member { member, .. } => member.stable_declaration,
                CallableReferenceTarget::Extension {
                    stable_declaration, ..
                } => *stable_declaration,
                CallableReferenceTarget::Property(property) => property.stable_declaration,
                CallableReferenceTarget::TopLevelProperty(property) => property.stable_declaration,
                CallableReferenceTarget::ClassifierProperty { .. } => None,
            },
            _ => None,
        }?;
        self.declaration_deprecated_use(declaration)
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
            | ResolvedConstructor::PlainSlots { member, .. } => member.stable_declaration,
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
        let declaration = self.resolved_index?.classifier_declaration(classifier)?;
        self.declaration_deprecated_use(declaration)
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
        let Some(DeprecatedUse {
            symbol,
            deprecation,
        }) = used
        else {
            return;
        };
        let message = format!(
            "'{symbol}' is deprecated.{}",
            optional_sentence(deprecation.message.as_deref())
        );
        match deprecation.level {
            DeprecationLevel::Warning => {
                if !self.suppresses_diagnostic("DEPRECATION") {
                    self.diags.warning_named(
                        span,
                        crate::diag::CompilerWarningName::Deprecation,
                        message,
                    );
                }
            }
            DeprecationLevel::Error | DeprecationLevel::Hidden => {
                if !self.suppresses_diagnostic("DEPRECATION_ERROR") {
                    self.diags.error(span, message);
                }
            }
        }
    }
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

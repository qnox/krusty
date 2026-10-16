//! kotlinc's `FirTypeArgumentsNotAllowedExpressionChecker` for a static member reached through a
//! classifier qualifier written with type arguments (`E<Int>.X`, `JavaUtils<String>.foo()`).
//!
//! The ordinary selector has already bound both sides of the access when this module runs. It
//! normalizes those recorded decisions into one qualifier-access fact and consumes the retained
//! parser syntax. It never reopens qualifier, scope, or provider resolution.

use super::*;

const MESSAGE: &str = "type arguments are not allowed when static member is accessed.";
const PROPERTY_ACCESS_MESSAGE: &str = "property access cannot have explicit type arguments.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QualifierAccessKind {
    Static,
    CompanionAssociated,
}

/// One normalized result of resolving a qualified member access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelectedQualifierAccess {
    qualifier: ExprId,
    kind: QualifierAccessKind,
}

impl Checker<'_> {
    /// Report explicit type arguments on a value binding after the ordinary scope tower has
    /// committed that binding. The caller supplies the selected-property fact by entering this
    /// operation only from the committed value-binding arm; retained syntax supplies only the
    /// diagnostic range and never reclassifies the source spelling.
    pub(super) fn check_selected_property_type_arguments(&mut self, expression: ExprId) {
        if self.discover_anonymous_captures {
            return;
        }
        let Some(arguments) = self.file.call_type_args.get(&expression.0) else {
            return;
        };
        self.diags
            .error(arguments.span, PROPERTY_ACCESS_MESSAGE.to_string());
    }

    fn member_qualifier(&self, expression: ExprId) -> Option<ExprId> {
        let member = match self.file.expr(expression) {
            Expr::Call { callee, .. } => *callee,
            Expr::Member { .. } => expression,
            _ => return None,
        };
        match self.file.expr(member) {
            Expr::Member { receiver, .. } => Some(*receiver),
            _ => None,
        }
    }

    /// Associated-property selection still owns its declaration facts here. Publish the normalized
    /// qualified-access result before the provider record is consumed by lowering.
    pub(super) fn record_associated_property_read(
        &mut self,
        expression: ExprId,
        property: &crate::libraries::PropertyInfo,
    ) {
        if property.associated_classifier.is_none() || property.getter.singleton_dispatch.is_some()
        {
            return;
        }
        let Some(qualifier) = self.member_qualifier(expression) else {
            return;
        };
        let kind = if property.companion_associated {
            QualifierAccessKind::CompanionAssociated
        } else {
            QualifierAccessKind::Static
        };
        self.selected_qualifier_accesses
            .insert(expression, SelectedQualifierAccess { qualifier, kind });
    }

    /// Retain qualifier-access classification while the exact selected declaration still carries
    /// its normalized namespace facts. General call records deliberately omit facts that lowering
    /// does not need.
    pub(super) fn record_selected_qualifier_call(
        &mut self,
        expression: ExprId,
        selected: &crate::libraries::FunctionInfo,
    ) {
        let kind = if selected.associated_classifier.is_some() {
            QualifierAccessKind::CompanionAssociated
        } else if selected.classifier_static || selected.implicit_classifier_callable.is_some() {
            QualifierAccessKind::Static
        } else {
            return;
        };
        let Some(qualifier) = self.member_qualifier(expression) else {
            return;
        };
        self.selected_qualifier_accesses
            .insert(expression, SelectedQualifierAccess { qualifier, kind });
    }

    /// Normalize enum/SAM/synthetic-property decisions already recorded by ordinary selection.
    fn derived_qualifier_access(&self, expression: ExprId) -> Option<SelectedQualifierAccess> {
        let qualifier = self.member_qualifier(expression)?;
        if matches!(
            self.expr_lowers.get(&qualifier),
            Some(ExprLowering::SingletonValue(_))
        ) {
            return None;
        }
        let kind = if self.resolved_enum_entries.contains_key(&expression)
            || matches!(
                self.expr_lowers.get(&expression),
                Some(ExprLowering::ClassifierPropertyRead { .. })
                    | Some(ExprLowering::SamConstructor { .. })
            ) {
            QualifierAccessKind::Static
        } else {
            return None;
        };
        Some(SelectedQualifierAccess { qualifier, kind })
    }

    pub(super) fn check_static_access_type_arguments(&mut self, expression: ExprId) {
        if self.discover_anonymous_captures {
            return;
        }
        if let Some(access) = self.derived_qualifier_access(expression) {
            self.selected_qualifier_accesses
                .entry(expression)
                .or_insert(access);
        }
        let Some(access) = self.selected_qualifier_accesses.get(&expression).copied() else {
            return;
        };
        let Some(arguments) = self.file.call_type_args.get(&access.qualifier.0) else {
            return;
        };
        match access.kind {
            QualifierAccessKind::Static => self.report_deprecation_error(
                arguments.span,
                self.file.forbid_useless_type_arguments,
                self.file.type_arguments_not_allowed_warning.as_deref(),
                crate::diag::CompilerWarningName::TypeArgumentsNotAllowed,
                MESSAGE,
            ),
            QualifierAccessKind::CompanionAssociated => {
                self.diags.error(arguments.span, MESSAGE.to_string());
            }
        }
    }
}

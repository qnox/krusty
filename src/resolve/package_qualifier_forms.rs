//! kotlinc's `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier` for a package
//! qualifier in expression position.
//!
//! - With the feature, a written segment that carries annotations or explicit type arguments
//!   (`(@Ann pkg).f()`, `pkg<Int>.Obj.x`) cannot name a package, as kotlinc's
//!   `canBePackagePartOfQualifier`: the qualifier walk fails at that segment, which is then
//!   unresolved. A segment that names a classifier is unaffected.
//! - A parenthesized package qualifier (`(pkg).f()`, `((pkg)).f()`) is reported once per qualified
//!   access at its outermost parentheses (`FirParenthesizedPackageQualifierChecker`): an error with
//!   the feature, a deprecation warning without it, and nothing in a release that predates it.
//! - Before the feature, type arguments on an earlier package segment of a qualifier that reaches a
//!   classifier (`pkg<Int>.sub.Obj.x`) are a deprecation warning at the leftmost list.

use super::*;

pub(crate) const PARENTHESIZED: &str = "wrapping package qualifiers in parentheses is not allowed.";
pub(crate) const TYPE_ARGUMENTS: &str = "type arguments are not allowed for packages.";

/// Whether the written qualifier segment `expression` may not bind as a package under the current
/// language settings.
pub(super) fn segment_cannot_name_a_package(file: &File, expression: ExprId) -> bool {
    file.forbid_package_qualifier_forms
        && (file.expression_annotations.contains_key(&expression)
            || file.call_type_args.contains_key(&expression.0))
}

/// The first written segment that may not bind as a package.
pub(super) fn first_segment_that_cannot_name_a_package(
    file: &File,
    segments: &[(Option<ExprId>, String)],
) -> Option<usize> {
    segments.iter().position(|(expression, _)| {
        expression.is_some_and(|expression| segment_cannot_name_a_package(file, expression))
    })
}

/// Walk the segments after a package root, failing at `barrier` (an index into the complete
/// `segments`, root included) when that segment would bind a package. The walk is the ordinary
/// left-to-right one; the barrier only removes the package facet of one written segment.
pub(super) fn walk_package_root_with_barrier<S: crate::symbol_source::SymbolSource + ?Sized>(
    source: &S,
    root_expression: Option<ExprId>,
    root_name: &str,
    segments: &[(Option<ExprId>, String)],
    barrier: usize,
) -> Result<ResolvedQualifier, QualifierError> {
    debug_assert!(barrier >= 1, "a root barrier is applied before the walk");
    let prefix = walk_qualifier_namespace_facets(
        source,
        None,
        root_expression,
        root_name,
        &segments[1..=barrier],
    )?;
    if let ResolvedQualifier::Package(_) = prefix {
        let (expression, name) = &segments[barrier];
        return Err(QualifierError::UnresolvedSegment {
            expression: *expression,
            name: name.clone(),
        });
    }
    walk_qualifier(source, prefix, &segments[barrier + 1..])
}

/// What the type-argument lists on a qualifier's package segments mean before the feature.
enum PackageTypeArguments {
    None,
    /// The leftmost list, on a package segment a later package segment follows.
    Deprecated(ExprId),
    /// Reported by kotlinc's own type-argument checks, not as this feature's deprecation.
    OuterClassError,
}

impl Checker<'_> {
    /// Check the package prefix of the qualifier that `expression` accesses a member of
    /// (`(pkg).f()`, `(pkg).Obj.x`, `pkg<Int>.sub.Cls::class`).
    pub(super) fn check_package_qualifier_forms(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) {
        let receiver = match self.file.expr(expression) {
            Expr::Member { receiver, .. }
            | Expr::CallableRef {
                receiver: Some(receiver),
                ..
            } => *receiver,
            Expr::Call { callee, .. } => match self.file.expr(*callee) {
                Expr::Member { receiver, .. } => *receiver,
                _ => return,
            },
            _ => return,
        };
        self.check_package_receiver_forms(scope, receiver);
    }

    /// Report a parenthesized package prefix of `receiver`, or else, before the feature, the
    /// type arguments written on one of its package segments. A prefix shared by several accesses
    /// (`(pkg).Obj.x` reaches it through `Obj` and through `x`) is reported once.
    pub(super) fn check_package_receiver_forms(
        &mut self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
    ) {
        if self.discover_anonymous_captures {
            return;
        }
        let mut segments = Vec::new();
        if qualifier_expression_segments(self.file, receiver, &mut segments).is_err() {
            return;
        }
        let segments = segments
            .into_iter()
            .filter_map(|(expression, _)| expression)
            .collect::<Vec<_>>();
        let type_arguments = if self.file.forbid_package_qualifier_forms
            || !segments
                .iter()
                .any(|expression| self.file.call_type_args.contains_key(&expression.0))
        {
            None
        } else {
            match self.package_type_argument_list(scope, &segments) {
                PackageTypeArguments::None => None,
                PackageTypeArguments::Deprecated(list) => Some(list),
                // kotlinc reports `TYPE_ARGUMENTS_FOR_OUTER_CLASS_WHEN_NESTED_REFERENCED` or
                // `TYPE_ARGUMENTS_NOT_ALLOWED` here and none of this feature's deprecations;
                // krusty does not model those errors yet.
                PackageTypeArguments::OuterClassError => return,
            }
        };
        if !self.report_parenthesized_package_prefix(scope, &segments) {
            if let Some(list) = type_arguments {
                self.report_package_segment_type_arguments(list);
            }
        }
    }

    /// Classify the type-argument lists written on the package segments of `segments`. Before the
    /// feature, a list on a package segment that a later package segment follows is a deprecation
    /// warning when the qualifier reaches a classifier (`pkg<Int>.sub.Obj.x`), reported once at the
    /// leftmost list. A list on the last package segment, or on a qualifier whose packages are not
    /// followed by a classifier, is an error of kotlinc's own type-argument checks instead.
    fn package_type_argument_list(
        &self,
        scope: &CheckerScope<'_>,
        segments: &[ExprId],
    ) -> PackageTypeArguments {
        let Some(last_package) = segments
            .iter()
            .rposition(|&expression| self.binds_package(scope, expression))
        else {
            return PackageTypeArguments::None;
        };
        let lists = || {
            segments[..=last_package]
                .iter()
                .filter(|expression| self.file.call_type_args.contains_key(&expression.0))
        };
        let Some(&leftmost) = lists().next() else {
            return PackageTypeArguments::None;
        };
        let reaches_classifier = segments.get(last_package + 1).is_some_and(|&expression| {
            matches!(
                self.qualifier(scope, QualifierInput::Expression(expression)),
                Ok(ResolvedQualifier::Classifier(_))
            )
        });
        if !reaches_classifier || lists().any(|&list| list == segments[last_package]) {
            return PackageTypeArguments::OuterClassError;
        }
        PackageTypeArguments::Deprecated(leftmost)
    }

    /// The outermost parenthesized prefix that binds as a package; a package inside it is not
    /// reported again (`((pkg).sub).f()`). Returns whether one was found.
    fn report_parenthesized_package_prefix(
        &mut self,
        scope: &CheckerScope<'_>,
        segments: &[ExprId],
    ) -> bool {
        if self.file.parenthesized_expression_spans.is_empty() {
            return false;
        }
        let parenthesized = segments.iter().rev().find_map(|&expression| {
            let span = *self
                .file
                .parenthesized_expression_spans
                .get(&expression.0)?;
            self.binds_package(scope, expression)
                .then_some((expression, span))
        });
        let Some((expression, span)) = parenthesized else {
            return false;
        };
        if !self.reported_package_qualifier_forms.insert(expression) {
            return true;
        }
        if self.file.forbid_package_qualifier_forms {
            self.diags.error(span, PARENTHESIZED.to_string());
        } else if let Some(warning) = self.file.parenthesized_package_qualifier_warning.clone() {
            self.report_package_qualifier_warning(
                span,
                crate::diag::CompilerWarningName::ParenthesizedPackageQualifier,
                warning,
            );
        }
        true
    }

    fn report_package_segment_type_arguments(&mut self, segment: ExprId) {
        let Some(warning) = self.file.package_qualifier_type_arguments_warning.clone() else {
            return;
        };
        let Some(span) = self
            .file
            .call_type_args
            .get(&segment.0)
            .map(|arguments| arguments.span)
        else {
            return;
        };
        if self.reported_package_qualifier_forms.insert(segment) {
            self.report_package_qualifier_warning(
                span,
                crate::diag::CompilerWarningName::TypeArgumentsNotAllowedInPackageQualifier,
                warning,
            );
        }
    }

    fn binds_package(&self, scope: &CheckerScope<'_>, expression: ExprId) -> bool {
        matches!(
            self.qualifier(scope, QualifierInput::Expression(expression)),
            Ok(ResolvedQualifier::Package(_))
        )
    }

    fn report_package_qualifier_warning(
        &mut self,
        span: Span,
        name: crate::diag::CompilerWarningName,
        warning: String,
    ) {
        if !self.suppresses_diagnostic(name.as_str()) {
            self.diags.warning_named(span, name, warning);
        }
    }
}

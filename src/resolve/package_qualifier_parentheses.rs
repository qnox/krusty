//! kotlinc's checks on how a package qualifier is written. A package qualifier is a name chain:
//! parentheses around part of one (`(p1.p2).p3.foo()`), type arguments on one of its segments
//! (`p1<Int>.p2.foo()`), or an annotation on part of one (`(@A p1).p2.foo()`) are deprecated until
//! `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier`.
//!
//! While the feature is off, `FirParenthesizedPackageQualifierChecker` reports each package
//! qualifier, the longest package prefix of a selection chain, once:
//! - type arguments on its last segment are kotlinc's `TYPE_ARGUMENTS_NOT_ALLOWED` error, from the
//!   checker of the selection made through it;
//! - otherwise, when the whole prefix is a parenthesized receiver (`(p1.p2.p3).foo()`), the
//!   deprecation `PARENTHESIZED_PACKAGE_QUALIFIER` covers its outermost parentheses;
//! - otherwise the first parenthesized part or segment type-argument list in source order is
//!   reported: the parentheses as above, the type arguments as the warning
//!   `TYPE_ARGUMENTS_NOT_ALLOWED_IN_PACKAGE_QUALIFIER_WARNING`, which carries the deprecation suffix.
//!
//! With the feature on, qualifier resolution does not continue a package through a segment with
//! type arguments or an annotated part, so that segment is unresolved, and parentheses are the
//! error `PARENTHESIZED_PACKAGE_QUALIFIER`.

use super::*;

const FEATURE: &str = "ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier";
const PARENTHESIZED: &str = "wrapping package qualifiers in parentheses is not allowed.";
const TYPE_ARGUMENTS: &str = "type arguments are not allowed for packages.";

/// What makes a package qualifier's spelling deprecated.
enum Finding {
    Parenthesized(Span),
    TypeArguments(Span),
}

impl Checker<'_> {
    /// Check the package qualifier inside `receiver`, the explicit receiver of a selection.
    pub(super) fn check_parenthesized_package_qualifier(
        &mut self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
    ) {
        if self.file.parenthesized_expression_spans.is_empty()
            && self.file.qualifier_type_argument_spans.is_empty()
            && self.file.expression_annotations.is_empty()
        {
            return;
        }
        // Classifier segments are receivers of the next segment; the package prefix ends below them.
        let mut package = receiver;
        loop {
            match self.qualifier(scope, QualifierInput::Expression(package)) {
                Ok(ResolvedQualifier::Package(_)) => break,
                Ok(ResolvedQualifier::Classifier(_)) => match self.file.expr(package) {
                    Expr::Member { receiver, .. } => package = *receiver,
                    _ => return,
                },
                Ok(ResolvedQualifier::Value) | Err(_) => return,
            }
        }
        let forbidden = self
            .file
            .forbid_annotations_type_arguments_and_parentheses_for_package_qualifier;
        if forbidden {
            if let Some(segment) = self.first_unresolvable_package_segment(package) {
                // Resolution stops at `segment`; the package before it is a qualifier of its own.
                if let Expr::Member { receiver, .. } = self.file.expr(segment) {
                    let receiver = *receiver;
                    if let Some(Finding::Parenthesized(span)) =
                        self.package_finding(receiver, false)
                    {
                        self.report_once(span, PARENTHESIZED.to_string(), true);
                    }
                }
                let (span, name) = self.package_segment_name(segment);
                self.report_once(
                    span,
                    crate::diagnostic_wording::unresolved_reference_on(&name, None),
                    true,
                );
                return;
            }
        } else if self
            .file
            .qualifier_type_argument_spans
            .contains_key(&package)
        {
            self.report_once(self.span(package), TYPE_ARGUMENTS.to_string(), true);
            return;
        }
        match self.package_finding(package, !forbidden) {
            Some(Finding::Parenthesized(span)) => {
                let message = if forbidden {
                    PARENTHESIZED.to_string()
                } else if self.suppresses_diagnostic("PARENTHESIZED_PACKAGE_QUALIFIER_WARNING") {
                    return;
                } else {
                    crate::features::deprecation_warning_message(FEATURE, PARENTHESIZED)
                };
                self.report_once(span, message, forbidden);
            }
            Some(Finding::TypeArguments(span))
                if !self.suppresses_diagnostic(
                    "TYPE_ARGUMENTS_NOT_ALLOWED_IN_PACKAGE_QUALIFIER_WARNING",
                ) =>
            {
                let message = crate::features::deprecation_warning_message(FEATURE, TYPE_ARGUMENTS);
                self.report_once(span, message, false);
            }
            Some(Finding::TypeArguments(_)) | None => {}
        }
    }

    /// What `FirParenthesizedPackageQualifierChecker` reports for the package qualifier `package`,
    /// the explicit receiver of a selection.
    fn package_finding(&self, package: ExprId, type_arguments: bool) -> Option<Finding> {
        match self.file.parenthesized_expression_spans.get(&package) {
            Some(span) => Some(Finding::Parenthesized(*span)),
            None => self.first_package_finding(package, true, type_arguments),
        }
    }

    /// The first parenthesized part or segment type-argument list of the name chain `expression`
    /// in source order. `outermost` excludes `expression`'s own parentheses, which the caller reads.
    fn first_package_finding(
        &self,
        expression: ExprId,
        outermost: bool,
        type_arguments: bool,
    ) -> Option<Finding> {
        if !outermost {
            if let Some(span) = self.file.parenthesized_expression_spans.get(&expression) {
                return Some(Finding::Parenthesized(*span));
            }
        }
        if let Expr::Member { receiver, .. } = self.file.expr(expression) {
            if let Some(finding) = self.first_package_finding(*receiver, false, type_arguments) {
                return Some(finding);
            }
        }
        if type_arguments {
            if let Some(span) = self.file.qualifier_type_argument_spans.get(&expression) {
                return Some(Finding::TypeArguments(*span));
            }
        }
        None
    }

    /// The segment nearest the root of the package chain `expression` that names its package
    /// through type arguments or an annotation, which resolution does not continue through.
    fn first_unresolvable_package_segment(&self, expression: ExprId) -> Option<ExprId> {
        if let Expr::Member { receiver, .. } = self.file.expr(expression) {
            if let Some(segment) = self.first_unresolvable_package_segment(*receiver) {
                return Some(segment);
            }
        }
        (self
            .file
            .qualifier_type_argument_spans
            .contains_key(&expression)
            || self.file.expression_annotations.contains_key(&expression))
        .then_some(expression)
    }

    /// The span and spelling of the name a package-chain segment selects.
    fn package_segment_name(&self, segment: ExprId) -> (Span, String) {
        match self.file.expr(segment).clone() {
            Expr::Member { name, .. } => (self.member_name_span(segment, &name), name),
            Expr::Name(name) => (self.span(segment), name),
            _ => unreachable!("a package qualifier is a name chain"),
        }
    }

    /// A selection chain is checked once per value-producing segment; its package qualifier is
    /// reported once.
    fn report_once(&mut self, span: Span, message: String, error: bool) {
        let file = self.diags.current_file();
        if self.diags.diags.iter().any(|diagnostic| {
            diagnostic.file == file && diagnostic.span == span && diagnostic.msg == message
        }) {
            return;
        }
        if error {
            self.diags.error(span, message);
        } else {
            self.diags.warning(span, message);
        }
    }
}

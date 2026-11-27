//! Declaration-side checks of a companion `operator fun of`, the factory a collection literal
//! calls.
//!
//! A checked `of` is recorded once, with its result type, as its declaration is visited: an
//! inferred result exists only after the body has been checked. The facts that compare overloads
//! of one companion with each other are reported together after the whole file is checked, so
//! every overload's result is known by then.

use super::*;
use crate::diag::names;
use crate::fir::{DeclarationFlags, DeclarationId};

/// One companion `operator fun of`, as its declaration was checked.
pub(super) struct OperatorOfOverload {
    companion: DeclarationId,
    /// The function name, where kotlinc reports a fact about an inferred result.
    name_span: Span,
    /// The written result type, where kotlinc reports a fact about an explicit result.
    result_span: Option<Span>,
    result: Ty,
    /// The overload whose only parameter is a `vararg`: the one every other overload must agree with.
    vararg: bool,
    /// Declares type parameters of its own, so its result is not comparable to another overload's
    /// by identity: each overload's `T` is a distinct parameter.
    generic: bool,
}

impl OperatorOfOverload {
    fn report_span(&self) -> Span {
        self.result_span.unwrap_or(self.name_span)
    }
}

/// Whether two results are the same type, a platform type standing for either nullability.
fn same_result(a: Ty, b: Ty) -> bool {
    a == b
        || (matches!(a.kind(), TyKind::PlatformNullable(_))
            || matches!(b.kind(), TyKind::PlatformNullable(_)))
            && a.non_null() == b.non_null()
}

impl Checker<'_> {
    /// Record `function` when it is a companion `operator fun of`. A revisit of the same
    /// declaration replaces its record, so a result refined by a later pass is the one reported.
    pub(super) fn record_operator_of_overload(
        &mut self,
        function: &FunDecl,
        result: Ty,
        declaration: Option<DeclarationId>,
    ) {
        if !self.file.collection_literals_enabled
            || !function.is_operator()
            || function.name != "of"
            || result == Ty::Error
        {
            return;
        }
        let Some(index) = self.resolved_index else {
            return;
        };
        let Some(companion) = declaration
            .and_then(|declaration| index.declaration_header(declaration)?.owner)
            .filter(|&owner| {
                index
                    .declaration_header(owner)
                    .is_some_and(|header| header.flags.has(DeclarationFlags::COMPANION))
            })
        else {
            return;
        };
        let record = OperatorOfOverload {
            companion,
            name_span: function.name_span,
            result_span: function.ret.as_ref().map(|written| written.span),
            result,
            vararg: function.receiver.is_none()
                && matches!(function.params.as_slice(), [only] if only.is_vararg),
            generic: !function.type_params.is_empty(),
        };
        match self
            .operator_of_overloads
            .iter_mut()
            .find(|known| known.name_span == record.name_span)
        {
            Some(known) => *known = record,
            None => self.operator_of_overloads.push(record),
        }
    }

    pub(super) fn report_operator_of_overloads(&mut self) {
        let overloads = std::mem::take(&mut self.operator_of_overloads);
        for overload in &overloads {
            let mut siblings = overloads
                .iter()
                .filter(|other| other.companion == overload.companion && other.vararg);
            let (Some(vararg), None) = (siblings.next(), siblings.next()) else {
                continue;
            };
            if overload.vararg
                || overload.generic
                || vararg.generic
                || same_result(overload.result, vararg.result)
            {
                continue;
            }
            self.diags.error_named(
                overload.report_span(),
                names::INCONSISTENT_RETURN_TYPES_IN_OF_OVERLOADS,
                format!(
                    "return type of operator 'of' must match return type of overload with \
                     'vararg' parameter: '{}' expected.",
                    vararg.result.source_name()
                ),
            );
        }
        for overload in &overloads {
            let span = overload.report_span();
            if overload.result.is_nullable() {
                self.diags.error_named(
                    span,
                    names::NULLABLE_RETURN_TYPE_OF_OPERATOR_OF,
                    "return type of 'operator fun of' cannot be nullable.",
                );
            } else if overload.result_span.is_none()
                && matches!(overload.result.kind(), TyKind::PlatformNullable(_))
            {
                self.diags.error_named(
                    span,
                    names::POTENTIALLY_NULLABLE_RETURN_TYPE_OF_OPERATOR_OF,
                    "return type of operator 'of' is inferred from a platform call, which can \
                     lead to unchecked nullability issues. Specify return type explicitly.",
                );
            }
        }
    }
}

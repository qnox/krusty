//! kotlinc's `WRONG_NUMBER_OF_TYPE_ARGUMENTS` for a classifier written in a type position: the
//! arguments written on the classifier's own segment must match its own type parameters
//! (`fun f(b: Box)` and `Box<Int, Int>` against `class Box<T>` report "one type argument expected
//! for 'class Box<T> : Any'.").
//!
//! The checker checks each classifier application it resolves itself. A declared type that Pass 1
//! finalized is not resolved again: its written syntax is walked beside the finalized type, and
//! each classifier application found there is checked by the same operation.
//!
//! The outermost type of an `is`/`as` target may be written bare (`x is Box`): its arguments come
//! from the operand's type, so only the arguments nested inside it are checked here.

use super::{candidate_display, function_type_ref_shape, Checker};
use crate::ast::TypeRef;
use crate::diag::Span;
use crate::spelling::Spelled;
use crate::symbol_source::SymbolSource as _;
use crate::types::{Ty, TyKind, TypeName};

/// kotlinc's message for an application of a declaration with `expected` type parameters whose
/// header renders as `declaration`: `one type argument expected for 'class Box<T> : Any'.`
pub(super) fn wrong_type_argument_count_message(expected: usize, declaration: &str) -> String {
    format!(
        "{} for '{declaration}'.",
        candidate_display::type_argument_count_wording(expected)
    )
}

/// Report a wrong argument count at `span`. Pass 1 reports a declaration type's application under
/// the same identity, so the checker revisiting that type reports it once.
pub(super) fn report_wrong_type_argument_count(
    diags: &mut crate::diag::DiagSink,
    span: Span,
    message: String,
) {
    diags.error_with_identity(
        span,
        crate::diag::DiagnosticIdentity::TypeArgumentCount { reference: span },
        message,
    );
}

/// The signature pass's report for an application of a typealias declared as `identity` with
/// `formals` and `expansion`, at its own written `<…>` list or its name.
pub(super) fn alias_arity_report(
    (headers, source): (&crate::fir::HeaderSyntaxArena, crate::fir::SourceFileId),
    reference: &TypeRef,
    identity: TypeName,
    formals: &[String],
    expansion: Ty,
) -> (Span, crate::diag::DiagnosticIdentity, String) {
    let span = headers
        .type_argument_list(source, reference.span)
        .unwrap_or(reference.span);
    let declaration = candidate_display::type_alias_header_display(
        identity.nested_segment_ref(),
        formals,
        expansion,
    );
    (
        span,
        crate::diag::DiagnosticIdentity::TypeArgumentCount { reference: span },
        wrong_type_argument_count_message(formals.len(), &declaration),
    )
}

/// The source range `reference` covers, `(lo, hi)`: its own span, each component's, and each
/// written `<…>` list, which ends past its last argument.
pub(super) fn written_extent(
    headers: &crate::fir::HeaderSyntaxArena,
    source: crate::fir::SourceFileId,
    reference: &TypeRef,
) -> (u32, u32) {
    let list_hi = headers
        .type_argument_list(source, reference.span)
        .map_or(0, |list| list.hi);
    reference
        .fun_params
        .iter()
        .chain(reference.arg.iter().map(|argument| &**argument))
        .chain(reference.targs.iter())
        .map(|component| written_extent(headers, source, component))
        .fold(
            (reference.span.lo, reference.span.hi.max(list_hi)),
            |(lo, hi), (component_lo, component_hi)| (lo.min(component_lo), hi.max(component_hi)),
        )
}

/// The arguments written on the last segment of `reference`. The parser keeps the lists written on
/// earlier qualified segments apart and flattens all of them into `targs`, last segment first.
fn own_segment_argument_count(reference: &TypeRef) -> usize {
    let qualifier = reference
        .qualifier_arguments
        .iter()
        .map(|list| list.arguments.len())
        .sum::<usize>();
    reference.targs.len().saturating_sub(qualifier)
}

/// Where kotlinc reports a wrong argument count: the written `<…>` list, or the classifier's name
/// when nothing is written.
fn application_span(file: &crate::ast::File, reference: &TypeRef) -> Span {
    file.type_argument_list_spans
        .get(&reference.span)
        .copied()
        .unwrap_or(reference.span)
}

/// Transient context owned by arity checking while one type reference is resolved.
#[derive(Default)]
pub(super) struct TypeArgumentArityState {
    /// The type reference whose outermost application may be written bare: an `is`/`as` target,
    /// or the receiver of a companion extension, which names the classifier's companion scope.
    bare_reference: Option<Span>,
}

impl Checker<'_> {
    /// Check the arguments written on `reference`, which the checker resolved to an application
    /// of `classifier`.
    pub(super) fn check_type_reference_arity(&mut self, reference: &TypeRef, classifier: TypeName) {
        self.check_classifier_application_arity(reference, classifier);
    }

    /// Run `resolve` with the outermost application of `reference` allowed to omit its arguments.
    pub(super) fn with_bare_outermost_type<R>(
        &mut self,
        reference: &TypeRef,
        resolve: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let previous = self.allow_bare_outermost_type(Some(reference));
        let result = resolve(self);
        self.restore_bare_outermost_type(previous);
        result
    }

    /// Allow the outermost application of `reference`, when there is one, to omit its arguments
    /// until [`Self::restore_bare_outermost_type`] restores the returned state.
    pub(super) fn allow_bare_outermost_type(
        &mut self,
        reference: Option<&TypeRef>,
    ) -> Option<Span> {
        match reference {
            Some(reference) => self
                .type_argument_arity
                .bare_reference
                .replace(reference.span),
            None => self.type_argument_arity.bare_reference,
        }
    }

    pub(super) fn restore_bare_outermost_type(&mut self, previous: Option<Span>) {
        self.type_argument_arity.bare_reference = previous;
    }

    /// Check a declared type that Pass 1 finalized: `reference` is its written syntax, `resolved`
    /// the finalized type, and `spelled` the alias spellings Pass 1 recorded for it.
    pub(super) fn check_finalized_type_arity(
        &mut self,
        reference: &TypeRef,
        resolved: Ty,
        spelled: &Spelled,
    ) {
        if reference.is_star_projection() {
            return;
        }
        let resolved = match resolved.kind() {
            TyKind::InProjection(inner) | TyKind::OutProjection(inner) => inner,
            _ => resolved,
        }
        .non_null();
        if spelled.alias.is_some() {
            // The written arguments of an alias application are applications of their own.
            for (argument, (resolved, spelled)) in reference.targs.iter().zip(&spelled.alias_args) {
                self.check_finalized_type_arity(argument, *resolved, spelled);
            }
            return;
        }
        if let Some(shape) = function_type_ref_shape(reference) {
            let TyKind::Fun(signature) = resolved.kind() else {
                return;
            };
            let components = shape.params.iter().zip(signature.params.iter().copied());
            let ret = shape.ret.map(|ret| (ret, signature.ret));
            for (index, (component, resolved)) in components.chain(ret).enumerate() {
                self.check_finalized_type_arity(component, resolved, spelled.arg(index));
            }
            return;
        }
        let TyKind::Obj(classifier, arguments) = resolved.kind() else {
            return;
        };
        if !self.check_classifier_application_arity(reference, classifier) {
            return;
        }
        for (index, (argument, resolved)) in reference.targs.iter().zip(arguments).enumerate() {
            self.check_finalized_type_arity(argument, *resolved, spelled.arg(index));
        }
    }

    /// Report `reference` when the arguments written on its own segment do not match the own type
    /// parameters of `classifier`. Returns whether they match.
    fn check_classifier_application_arity(
        &mut self,
        reference: &TypeRef,
        classifier: TypeName,
    ) -> bool {
        // An import path or a class literal's receiver names a declaration; neither is a type
        // that applies arguments (class literal arguments have their own rule).
        if reference.is_import()
            || reference.is_class_literal_lhs()
            || self.type_argument_arity.bare_reference == Some(reference.span)
        {
            return true;
        }
        let Some(declaration) = self.fed_source().classifier(classifier) else {
            return true;
        };
        let expected = declaration.own_type_parameter_count;
        if own_segment_argument_count(reference) == expected {
            return true;
        }
        let display =
            candidate_display::classifier_access_display_from_shape(classifier, &declaration);
        report_wrong_type_argument_count(
            self.diags,
            application_span(self.file, reference),
            wrong_type_argument_count_message(expected, &display),
        );
        false
    }
}

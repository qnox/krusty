//! kotlinc's `TypeArgumentsInPackagesTypeRefChecker`: type arguments written on a package segment
//! of a qualified type or annotation reference (`part1<Int>.part2.Cls`, `@part1.part2<Int>.Anno`).
//!
//! A package has no type parameters, so the arguments select nothing: "type arguments are not
//! allowed for packages." on the segment's `<…>` list. kotlinc always rejected a list on the last
//! package segment; one on an earlier segment keeps a deprecation warning until
//! `ForbidUselessTypeArgumentsIn25`. Resolution records the exact written segment where its
//! left-to-right namespace walk first selected a classifier; later checks consume that boundary
//! without inspecting source spelling or the selected classifier's expanded nesting.

use super::*;

const MESSAGE: &str = "type arguments are not allowed for packages.";

/// The lists `reference` wrote before its already-selected classifier segment.
fn package_argument_lists(
    reference: &TypeRef,
    classifier_segment: u32,
) -> (Vec<QualifierSegmentArguments>, u32) {
    if reference.qualifier_arguments.is_empty() {
        return (Vec::new(), 0);
    }
    let lists = reference
        .qualifier_arguments
        .iter()
        .filter(|list| list.segment < classifier_segment)
        .copied()
        .collect();
    (lists, classifier_segment)
}

/// `reference` without the type arguments written on its package segments, when it wrote any.
/// The classifier's own and captured arguments are unchanged.
pub(super) fn without_package_arguments(
    reference: &TypeRef,
    classifier_segment: u32,
) -> Option<TypeRef> {
    let (lists, _) = package_argument_lists(reference, classifier_segment);
    if lists.is_empty() {
        return None;
    }
    // Earlier segments' arguments follow every later segment's in `targs`, so the package
    // segments' arguments are its tail.
    let stripped = lists.iter().map(|list| list.count as usize).sum::<usize>();
    let mut reference = reference.clone();
    let kept = reference.targs.len().saturating_sub(stripped);
    reference.targs.truncate(kept);
    reference
        .qualifier_arguments
        .retain(|list| !lists.contains(list));
    Some(reference)
}

impl Checker<'_> {
    /// Validate that a successful classifier selection retained its path boundary, then report and
    /// remove package-owned arguments before applying the selected classifier or alias.
    pub(super) fn check_selected_package_type_arguments(
        &mut self,
        reference: &TypeRef,
        classifier_segment: Option<u32>,
    ) -> Option<TypeRef> {
        self.check_package_type_arguments(
            reference,
            classifier_segment
                .expect("a selected classifier path must retain its written boundary"),
        )
    }

    /// Report the type arguments `reference` wrote on package segments, and return it without
    /// them when it wrote any.
    pub(super) fn check_package_type_arguments(
        &mut self,
        reference: &TypeRef,
        classifier_segment: u32,
    ) -> Option<TypeRef> {
        let (lists, packages) = package_argument_lists(reference, classifier_segment);
        for list in &lists {
            // The last package segment never accepted type arguments.
            let forbidden = self.file.forbid_useless_type_arguments || list.segment + 1 == packages;
            let warning = self
                .file
                .useless_type_arguments_warning_format
                .as_ref()
                .map(|format| format.message(MESSAGE));
            self.report_deprecation_error(
                list.span,
                forbidden,
                warning.as_deref(),
                crate::diag::CompilerWarningName::TypeArgumentsNotAllowed,
                MESSAGE,
            );
        }
        without_package_arguments(reference, classifier_segment)
    }

    /// Report the package-segment type arguments of a declaration type Pass 1 finalized as
    /// `resolved`, and of each type argument written in it.
    pub(super) fn check_finalized_package_type_arguments(
        &mut self,
        reference: &TypeRef,
        resolved: Ty,
        spelling: &crate::spelling::Spelled,
    ) {
        let resolved = resolved.non_null();
        let without_package_arguments = spelling
            .classifier_segment
            .and_then(|segment| self.check_package_type_arguments(reference, segment));
        let reference = without_package_arguments.as_ref().unwrap_or(reference);
        if spelling.alias.is_some() {
            for (argument, (resolved, spelling)) in reference.targs.iter().zip(&spelling.alias_args)
            {
                if !argument.is_star_projection() {
                    self.check_finalized_package_type_arguments(argument, *resolved, spelling);
                }
            }
            return;
        }
        for ((argument, resolved), spelling) in reference
            .targs
            .iter()
            .zip(resolved.type_args())
            .zip(&spelling.args)
        {
            if !argument.is_star_projection() {
                self.check_finalized_package_type_arguments(
                    argument,
                    resolved.projection_read_ty(),
                    spelling,
                );
            }
        }
    }
}

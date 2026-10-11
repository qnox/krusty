//! kotlinc's `TypeArgumentsInPackagesTypeRefChecker`: type arguments written on a package segment
//! of a qualified type or annotation reference (`part1<Int>.part2.Cls`, `@part1.part2<Int>.Anno`).
//!
//! A package has no type parameters, so the arguments select nothing: "type arguments are not
//! allowed for packages." on the segment's `<…>` list. kotlinc always rejected a list on the last
//! package segment; one on an earlier segment keeps a deprecation warning until
//! `ForbidUselessTypeArgumentsIn25`. Which segments are packages follows from the classifier the
//! complete name resolved to: the segments before its outermost enclosing classifier.

use super::*;

const MESSAGE: &str = "type arguments are not allowed for packages.";

/// The lists `reference` wrote on package segments, given the classifier it resolved to, with the
/// number of segments that are packages.
fn package_argument_lists(
    reference: &TypeRef,
    classifier: TypeName,
) -> (Vec<QualifierSegmentArguments>, u32) {
    if reference.qualifier_arguments.is_empty() {
        return (Vec::new(), 0);
    }
    let written = reference.name.split('.').count() as u32;
    let classifier_segments =
        std::iter::successors(Some(classifier), |classifier| classifier.nested_owner()).count()
            as u32;
    let packages = written.saturating_sub(classifier_segments);
    let lists = reference
        .qualifier_arguments
        .iter()
        .filter(|list| list.segment < packages)
        .copied()
        .collect();
    (lists, packages)
}

/// `reference` without the type arguments written on its package segments, when it wrote any.
/// The classifier's own and captured arguments are unchanged.
pub(super) fn without_package_arguments(
    reference: &TypeRef,
    classifier: TypeName,
) -> Option<TypeRef> {
    let (lists, _) = package_argument_lists(reference, classifier);
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
    /// Report the type arguments `reference` wrote on package segments, and return it without
    /// them when it wrote any.
    pub(super) fn check_package_type_arguments(
        &mut self,
        reference: &TypeRef,
        classifier: TypeName,
    ) -> Option<TypeRef> {
        let (lists, packages) = package_argument_lists(reference, classifier);
        for list in &lists {
            // The last package segment never accepted type arguments.
            let forbidden = self.file.forbid_useless_type_arguments || list.segment + 1 == packages;
            self.report_deprecation_error(
                list.span,
                "ForbidUselessTypeArgumentsIn25",
                forbidden,
                "TYPE_ARGUMENTS_NOT_ALLOWED_WARNING",
                MESSAGE,
            );
        }
        without_package_arguments(reference, classifier)
    }

    /// Report the package-segment type arguments of a declaration type Pass 1 finalized as
    /// `resolved`, and of each type argument written in it.
    pub(super) fn check_finalized_package_type_arguments(
        &mut self,
        reference: &TypeRef,
        resolved: Ty,
    ) {
        let resolved = resolved.non_null();
        if let Some(classifier) = resolved.kotlin_class_internal() {
            self.check_package_type_arguments(reference, classifier);
        }
        for (argument, resolved) in reference.targs.iter().zip(resolved.type_args()) {
            if !argument.is_star_projection() {
                self.check_finalized_package_type_arguments(
                    argument,
                    resolved.projection_read_ty(),
                );
            }
        }
    }
}

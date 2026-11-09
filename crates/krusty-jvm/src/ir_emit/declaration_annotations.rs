//! JVM emission helpers for declaration annotations whose normalized spelling changes while their
//! checked semantic facts and retention remain attached to the source application.

/// Replace the emitted annotation payload without losing the checked declaration facts carried by
/// the original application.
pub(super) fn replace(
    retained: &krusty::ir::RetainedAnnotation,
    annotation: krusty::ir::AppliedAnnotation,
) -> krusty::ir::RetainedAnnotation {
    krusty::ir::RetainedAnnotation {
        retention: retained.retention,
        annotation,
        facts: retained.facts,
    }
}

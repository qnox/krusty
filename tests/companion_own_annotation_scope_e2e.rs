//! A classifier's own annotations are resolved outside it (KT-77137). A companion object is an
//! implicit receiver inside its enclosing class, but not in its own annotations, so they don't see
//! its members. A sibling nested class's annotations still see the companion.

use super::common;
use super::diagnostics_parity_support::errors;

#[test]
fn a_companions_own_annotations_do_not_see_its_members_like_kotlinc() {
    let source = "@Target(AnnotationTarget.CLASS)\n\
                  annotation class Anno(val value: Int)\n\
                  const val outer = 2\n\
                  class TopLevelClass {\n\
                  \x20   @Anno(inner)\n\
                  \x20   companion object {\n\
                  \x20       const val inner = 1\n\
                  \x20   }\n\
                  \x20   @Anno(inner) class Sibling\n\
                  }\n\
                  class Other {\n\
                  \x20   @Anno(outer) companion object Named {\n\
                  \x20       const val own = 3\n\
                  \x20   }\n\
                  \x20   @Anno(own) class Sibling\n\
                  }\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("CompanionAnnotations.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| (error.line, error.column, error.message.as_str()))
            .collect::<Vec<_>>(),
        [(5, 11, "unresolved reference 'inner'.")]
    );
}

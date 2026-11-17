//! Complete declaration diagnostics are independent of inferred result approximation and body
//! retention. The shared legal expect/member program also runs through the portable inventory.

use super::common;

const DECLARATIONS: &str = include_str!("fixtures/diagnostics/nonMemberFunctionBodies.kt");
const EXEMPTIONS: &str =
    include_str!("box_regressions/multimodule/nonMemberFunctionBodyExemptions.kt");

#[test]
fn declaration_errors_do_not_suppress_an_independent_following_body_error() {
    let error_first = include_str!("fixtures/diagnostics/nonMemberFunctionBodiesErrorFirst.kt");
    for (source, expected) in [
        (
            DECLARATIONS,
            [
                "Declarations.kt:4:1: function 'missing' must have a body.",
                "Declarations.kt:5:1: function 'generic' must have a body.",
                "Declarations.kt:6:1: function 'extension' must have a body.",
                "Declarations.kt:8:30: initializer type mismatch: expected 'String', actual 'Int'.",
            ],
        ),
        (
            error_first,
            [
                "Declarations.kt:1:30: initializer type mismatch: expected 'String', actual 'Int'.",
                "Declarations.kt:5:1: function 'missing' must have a body.",
                "Declarations.kt:6:1: function 'generic' must have a body.",
                "Declarations.kt:7:1: function 'extension' must have a body.",
            ],
        ),
    ] {
        let sources = [("Declarations.kt", source)];
        common::assert_error_blocks_match_kotlinc(&sources, &[]);
        assert_eq!(common::reference_error_blocks(&sources, &[]), expected);
    }
}

#[test]
fn ordinary_member_and_actualized_expect_bodies_retain_their_declaration_contracts() {
    let prepared =
        krusty::conformance::prepare_test_source(EXEMPTIONS, krusty::conformance::TestTarget::Jvm);
    let modules = krusty::conformance::split_modules(&prepared).expect("whole source-set program");
    let units = krusty::conformance::module_units(&modules);
    let [unit] = units.as_slice() else {
        panic!("one folded common/platform source unit")
    };
    assert_eq!(unit.files.len(), 2, "both whole source files are retained");
    assert_eq!(unit.common_file_count, 1);
    let files = unit
        .files
        .iter()
        .map(|(stem, source)| (format!("{stem}.kt"), source.as_str()))
        .collect::<Vec<_>>();
    let sources = files
        .iter()
        .map(|(name, source)| (name.as_str(), *source))
        .collect::<Vec<_>>();
    let classes = common::classes_against_kotlinc_source_set(&sources, unit.common_file_count);
    let stdlib = common::stdlib_jar();
    for (compiler, files) in [("kotlinc", &classes.reference), ("krusty", &classes.krusty)] {
        let files = files
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect::<Vec<_>>();
        let entry = common::find_box_class(&files).expect("whole program's box entry");
        assert_eq!(
            common::run_box(&files, &entry, std::slice::from_ref(&stdlib)).expect("run box"),
            "OK",
            "{compiler} whole source-set result"
        );
    }
}

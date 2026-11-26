//! An intersection that inference fixes a bounded type parameter to approximates to that bound
//! where a declaration publishes it. kotlinc records the variable's upper type on the intersection
//! when the intersection's own public approximation is not a subtype of it, and every later
//! approximation reads it: through another generic call, a classifier argument, a local inside a
//! lambda, or a private declaration. An unbounded parameter keeps the common supertype. An
//! approximated intersection passed on to another bounded parameter keeps its approximation when
//! that reaches the new bound and takes the new bound otherwise.
use super::common;

const PROGRAM: &str = include_str!("box_regressions/IntersectionUpperBoundPublication.kt");

/// The program's `// FILE:` sections as named sources.
fn program_files() -> Vec<(&'static str, &'static str)> {
    PROGRAM
        .split("// FILE: ")
        .filter(|section| !section.is_empty())
        .map(|section| {
            section
                .split_once('\n')
                .expect("a FILE directive names its file on its own line")
        })
        .collect()
}

#[test]
fn a_bounded_intersection_publishes_the_bound() {
    let usage = "\
fun check() {
    val a: Int = picked()
    val b: Int = unbounded()
    val c: Int = boxedPick()
    val d: Int = property
    val e: Int = kept()
    val f: Int = wrapped()
    val g: Int = local()
    val h: Int = marked()
    val i: Int = named()
}
";
    let declarations = program_files()
        .into_iter()
        .find(|(name, _)| *name == "Declarations.kt")
        .expect("the program declares its functions in Declarations.kt");
    let sources = [declarations, ("Usage.kt", usage)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    let mismatch = |line: u32, actual: &str| {
        format!("Usage.kt:{line}:16: initializer type mismatch: expected 'Int', actual '{actual}'.")
    };
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            mismatch(2, "Named"),
            mismatch(3, "Any"),
            mismatch(4, "Box<Named>"),
            mismatch(5, "Named"),
            mismatch(6, "Named"),
            mismatch(7, "Box<Named>"),
            mismatch(8, "Named"),
            mismatch(9, "Marked"),
            mismatch(10, "Named"),
        ]
    );
}

#[test]
fn a_published_bound_is_the_declaration_abi() {
    let sources = program_files();
    assert_eq!(common::kotlinc_box_files_result(&sources, "BoxKt"), "OK");
    common::expect_box_ok_files_with_stdlib(&sources, "IntersectionUpperBoundPublication");
}

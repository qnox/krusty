//! An inferred declaration that calls or reads a declaration spelled through a typealias carries
//! that abbreviation in its own metadata type, whatever the selected declaration's origin.
// PLATFORM: complete JVM class inventories over a krusty-built dependency.

use super::common;

const CALLS: &str = include_str!("box_regressions/multimodule/inferredResultThroughAliasCall.kt");
const READS: &str = include_str!("box_regressions/multimodule/inferredResultThroughAliasRead.kt");
const ARGUMENTS: &str =
    include_str!("box_regressions/multimodule/inferredResultThroughAliasArguments.kt");
const NULLABLE: &str =
    include_str!("box_regressions/multimodule/inferredResultThroughNullableAlias.kt");

/// The `lib` and `main(lib)` modules of a two-module fixture.
fn modules(program: &str) -> (&str, &str) {
    let (library, main) = program
        .split_once("// MODULE: main(lib)\n")
        .expect("the fixture declares a main module");
    let library = library
        .strip_prefix("// MODULE: lib\n")
        .expect("the fixture starts with its library module");
    (library, main)
}

fn assert_complete_program(tag: &str, program: &str) {
    let (library, main) = modules(program);
    let library =
        common::compile_libs(tag, &[("Lib.kt", library)]).expect("krusty builds the library");
    let classes = common::classes_against_kotlinc_module_cp(&[("Main.kt", main)], &[library]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>(),
        "complete class inventory"
    );
    let differences = classes
        .reference
        .iter()
        .filter_map(|(name, reference)| {
            let ours = &classes.krusty[name];
            (reference != ours).then(|| common::exact_class_difference(name, reference, ours))
        })
        .collect::<Vec<_>>();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn the_complete_alias_call_program_matches_kotlin() {
    assert_complete_program("inferred_result_alias_calls", CALLS);
}

#[test]
fn the_complete_alias_read_program_matches_kotlin() {
    assert_complete_program("inferred_result_alias_reads", READS);
}

#[test]
fn the_complete_redundant_alias_argument_program_matches_kotlin() {
    assert_complete_program("inferred_result_alias_arguments", ARGUMENTS);
}

#[test]
fn the_complete_nullable_alias_expansion_program_matches_kotlin() {
    assert_complete_program("inferred_result_nullable_alias", NULLABLE);
}

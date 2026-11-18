//! A callable reference to a generic `vararg` function, passed where the enclosing call declares
//! the function's input as one of its own type variables (`f: (T) -> Int`). kotlinc releases with
//! `Quirk::VarargReferenceTakesTypeVariableInputAsElement` (KT-90169) bind that input as one
//! collected element; later ones bind the whole array, as they already do for a concrete or
//! array-shaped input.
//!
//! The program is the shared box regression, whose `box()` only asserts that the cases agree on
//! one binding. A nullable input (`f: (T?) -> Int`) is applicable only under the element binding,
//! so the original combined program is retained in `varargReferenceTypeVariableInputBefore25.kt`
//! for those releases, alongside the isolated nullable control. These tests run each whole program
//! and also return its `describe()`, pinning which binding the reference release chooses.

use krusty::kotlin_version::{Quirk, Quirks};

use super::common;

const PORTABLE: &str = include_str!("box_regressions/varargReferenceTypeVariableInput.kt");
const BEFORE_25: &str = include_str!("box_regressions/varargReferenceTypeVariableInputBefore25.kt");

/// The shared program with its `box()` renamed `portableBox()`, and a `box()` that answers
/// `portableBox()`'s result followed by `describe()`.
fn source(program: &str) -> String {
    let portable = "fun box(): String {";
    assert_eq!(
        program.matches(portable).count(),
        1,
        "one box() in the fixture"
    );
    let renamed = program.replacen(portable, "fun portableBox(): String {", 1);
    format!("{renamed}\nfun box(): String = portableBox() + \";\" + describe()\n")
}

/// `box()`'s answer followed by every case's value, where `element` means an input from a call
/// type variable binds one element.
fn expected(element: bool, nullable: bool) -> String {
    let (count, first) = if element { (1, true) } else { (2, false) };
    let mut cases = vec![
        format!("top={count}"),
        format!("explicit={count}"),
        format!("let={count}"),
    ];
    if nullable {
        cases.push(format!("nullable={count}"));
    }
    cases.extend([
        format!("result={first}"),
        format!("list={count}"),
        format!("member={count}"),
        format!("extension={count}"),
        format!("local={count}"),
        format!("ownFormal={count}"),
        "receiverFormal=2".to_string(),
        "arrayInput=2".to_string(),
        "concreteVararg=2".to_string(),
        "primitive=3".to_string(),
        "declared=2".to_string(),
    ]);
    let cases = cases.join(",");
    format!("OK;{cases}")
}

#[test]
fn a_type_variable_input_binds_the_vararg_as_the_reference_release_does() {
    assert_reference_binding(PORTABLE, false);
}

#[test]
fn the_original_nullable_and_nonnullable_inputs_keep_the_reference_binding_together() {
    if !krusty::conformance::kotlinc_release_applicable(BEFORE_25, krusty::kotlin_version::target())
        .expect("the shared fixture declares a valid release range")
    {
        return;
    }
    assert_reference_binding(BEFORE_25, true);
}

fn assert_reference_binding(program: &str, nullable: bool) {
    let element = Quirks::target().has(Quirk::VarargReferenceTakesTypeVariableInputAsElement);
    let source = source(program);
    let reference = common::kotlinc_box_result(&source);
    assert_eq!(reference, expected(element, nullable), "kotlinc");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty = common::compile_and_run_box(
        &source,
        "Main",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
    assert_eq!(krusty.as_deref(), Some(reference.as_str()), "krusty");
}

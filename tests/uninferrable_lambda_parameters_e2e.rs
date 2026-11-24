//! A lambda parameter that names no type and has none to infer from.
//!
//! Standing alone it needs an explicit type; passed to a callee that gives it no function shape
//! it cannot be inferred, and when that parameter is a bare type variable the call cannot infer
//! the variable either. Each fixture goes through both compilers and the complete reports are
//! compared: message, position and order.

use super::common;

fn assert_same(stem: &str, source: &str, expected_count: usize) {
    let dir = common::scratch_dir().expect("scratch dir");
    let file = dir.join("Main.kt");
    std::fs::write(&file, source).expect("write the fixture");
    let reference_out = dir.join(format!("{stem}-reference"));
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (status, reference) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        file.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    let out = std::process::Command::new(common::krusty_binary())
        .args(["-no-stdlib", "-cp"])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(dir.join(format!("{stem}-krusty")))
        .arg(&file)
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        status != 0,
        !out.status.success(),
        "the compilers disagree on whether this source is rejected\nreference:\n{reference}\nkrusty:\n{krusty}"
    );
    let (reference, krusty) = (common::reported(&reference), common::reported(&krusty));
    assert_eq!(
        reference.len(),
        expected_count,
        "reference ledger: {reference:#?}"
    );
    assert_eq!(reference, krusty);
}

#[test]
fn a_standalone_lambda_needs_an_explicit_parameter_type() {
    assert_same("standalone", "fun f() {\n    val a = { x -> }\n}\n", 1);
}

#[test]
fn a_bare_type_variable_parameter_gives_the_lambda_no_shape() {
    assert_same(
        "bare_variable",
        "fun <T> id(x: T): T = x\n\
         fun f() {\n\
         \x20   val a = id { x -> }\n\
         \x20   val b = id({ x -> })\n\
         \x20   val c = id({ x: Int -> })\n\
         }\n",
        4,
    );
}

#[test]
fn a_library_callee_with_a_bare_type_variable_parameter_gives_the_lambda_no_shape() {
    assert_same(
        "library_callee",
        "fun f() {\n\
         \x20   val a = listOf({ x -> })\n\
         }\n",
        2,
    );
}

#[test]
fn a_library_callee_with_no_argument_cannot_infer_its_type_variable() {
    assert_same(
        "library_no_argument",
        "fun f() {\n\
         \x20   val a = mutableSetOf()\n\
         }\n",
        1,
    );
}

#[test]
fn a_variable_bound_by_an_explicit_type_argument_or_receiver_needs_no_lambda_inference() {
    assert_same(
        "bound_elsewhere",
        "class MyList<T>\n\
         operator fun <T> MyList<T>.plusAssign(element: T) {}\n\
         fun f(functions: MyList<(Int) -> Int>) {\n\
         \x20   functions.plusAssign({ it -> it })\n\
         \x20   val a = listOf<(Any) -> Unit>({ x -> })\n\
         }\n",
        0,
    );
}

#[test]
fn a_function_shape_of_any_that_the_receiver_supplies_is_not_a_missing_inference() {
    assert_same(
        "receiver_any_shape",
        "class MyList<T> {\n\
         \x20   operator fun plusAssign(element: T) {}\n\
         }\n\
         class Plain<T>\n\
         operator fun <T> Plain<T>.plusAssign(element: T) {}\n\
         fun f(members: MyList<(Any) -> Unit>, extensions: Plain<(Any) -> Unit>) {\n\
         \x20   members.plusAssign({ x -> })\n\
         \x20   extensions.plusAssign({ x -> })\n\
         }\n",
        0,
    );
}

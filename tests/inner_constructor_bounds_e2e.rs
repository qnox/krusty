use super::common;

const DECLARATIONS: &str = "class Outer<O : Number> { inner class Inner<T : O>(val value: T) }\n";

#[test]
fn an_inner_constructor_preserves_its_fixed_outer_in_the_inferred_result() {
    let source = format!(
        "{DECLARATIONS}\n\
         fun make(outer: Outer<Int>) = outer.Inner<Int>(1)\n\
         fun box(): String = if (make(Outer<Int>()).value == 1) \"OK\" else \"wrong value\"\n"
    );
    let compared =
        common::compile_with_kotlinc("Main", &source, &[], &["Outer", "Outer$Inner", "MainKt"]);
    for ((expected, actual), name) in compared.into_iter().zip(["Outer", "Outer$Inner", "MainKt"]) {
        assert_eq!(
            common::member_table(&actual),
            common::member_table(&expected),
            "{name}"
        );
    }
    // JVM byte-for-byte boxing-cast parity is tracked independently in #2590. This regression
    // proves the complete method ABI (including the inferred captured application) and behavior.
    common::expect_box_same_as_kotlinc(&source, "inner_constructor_bounds");
}

const LONG_CALLS: &str = "typealias LongInner = Outer<Long>.Inner<Long>\n\
    fun explicit(outer: Outer<Long>) = outer.Inner<Long>(1)\n\
    fun inferred(outer: Outer<Long>) = outer.Inner(1)\n\
    fun declared(outer: Outer<Long>): Outer<Long>.Inner<Long> = outer.Inner<Long>(1)\n\
    fun alias(outer: Outer<Long>): Outer<Long>.Inner<Long> = outer.LongInner(1)\n";

const EAGER_DECLARATIONS: &str = "fun interface Mapper<T> { fun run(x: T): T }\n\
    class Outer<O : Number> {\n\
        inner class Inner {\n\
            val tag: String\n\
            constructor(f: (O) -> Unit) { tag = \"U\" }\n\
            constructor(f: Mapper<O>) { tag = \"M\" }\n\
        }\n\
    }\n";
const EAGER_CALL: &str = "// LANGUAGE: +EagerLambdaAnalysis\n\
    fun box(): String = if (Outer<Int>().Inner { it }.tag == \"M\") \"OK\" else \"wrong constructor\"\n";

#[test]
fn eager_lambda_constructor_selection_sees_the_fixed_outer_parameter() {
    common::expect_box_same_as_kotlinc(
        &format!("{EAGER_CALL}{EAGER_DECLARATIONS}"),
        "inner_constructor_eager_bounds",
    );
}

#[test]
fn eager_dependency_constructor_selection_sees_the_fixed_outer_parameter() {
    let library = common::kotlinc_lib_out(&[("Declarations.kt", EAGER_DECLARATIONS)])
        .expect("reference provisions the overload family");
    let classpath = vec![library.clone(), common::stdlib_jar()];
    let joined = std::env::join_paths(&classpath).expect("fixture classpath");
    let reference = common::kotlinc_lib::kotlinc_lib_out_with(
        &[("Use.kt", EAGER_CALL)],
        &["-cp".into(), joined.to_string_lossy().into_owned()],
    )
    .expect("reference selects the non-coercing mapper constructor");
    let expected = common::run_box(&[], "UseKt", &[reference, library, common::stdlib_jar()])
        .expect("run reference dependency consumer");
    assert_eq!(expected, "OK");
    assert_eq!(
        common::compile_and_run_box(
            EAGER_CALL,
            "Use",
            &classpath,
            Some(common::jdk_modules().as_path()),
        ),
        Some(expected)
    );
}

#[test]
fn a_receiver_bound_supplies_literal_context_for_own_and_alias_construction() {
    let source = format!(
        "{DECLARATIONS}{LONG_CALLS}\n\
         fun box(): String {{\n\
             val outer = Outer<Long>()\n\
             return if (explicit(outer).value + inferred(outer).value + declared(outer).value + alias(outer).value == 4L) \"OK\" else \"wrong values\"\n\
         }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "inner_constructor_long_bounds");
}

#[test]
fn dependency_construction_keeps_complete_own_and_enclosing_signatures() {
    // Inferred alias-call lookup is a separate signature-graph gap tracked in #2592. The declared
    // alias and both direct inference forms here exercise the complete bound application.
    let classes = common::classes_against_kotlinc_lib(
        "InnerConstructorLongBounds",
        &[("Declarations.kt", DECLARATIONS)],
        LONG_CALLS,
    )
    .expect("reference provisions the bounded inner declaration");
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for (name, expected) in classes.reference {
        assert_eq!(
            common::member_table(&classes.krusty[&name]),
            common::member_table(&expected),
            "{name}"
        );
    }
}

fn assert_outer_bound_rejected(source: &str, classpath: &[std::path::PathBuf]) {
    let result = common::compiler_diagnostics(&[("Use.kt", source)], classpath);
    let expected = common::compiler_errors(&result.reference_stderr);
    assert!(
        !expected.is_empty(),
        "reference must reject the inner bound"
    );
    assert_eq!(result.reference_code, 1);
    assert_eq!(result.krusty_code, 1);
    assert_eq!(
        common::compiler_errors(&format!("{}{}", result.krusty_stderr, result.krusty_stdout)),
        expected
    );
}

#[test]
fn source_inner_constructor_cannot_infer_away_its_enclosing_binding() {
    for result in [": Any", ""] {
        let source =
            format!("{DECLARATIONS}fun wrong(outer: Outer<Int>){result} = outer.Inner<Long>(1L)\n");
        assert_outer_bound_rejected(&source, &[common::stdlib_jar()]);
    }
}

#[test]
fn dependency_inner_constructor_cannot_infer_away_its_enclosing_binding() {
    let library = common::kotlinc_lib_out(&[("Declarations.kt", DECLARATIONS)])
        .expect("reference provisions the bounded inner declaration");
    for result in [": Any", ""] {
        let source = format!("fun wrong(outer: Outer<Int>){result} = outer.Inner<Long>(1L)\n");
        assert_outer_bound_rejected(&source, &[library.clone(), common::stdlib_jar()]);
    }
}

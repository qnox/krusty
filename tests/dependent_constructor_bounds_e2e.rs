use super::common;

const DECLARATIONS: &str = "package bounds\n\
    class Leaf<T>(val n: Int)\n\
    interface Marker<T>\n\
    class Marked<T> : Marker<T>\n\
    class Compound<A, B : Marker<A>>(val leaf: Leaf<Marker<B>>)\n";

#[test]
fn source_constructor_applied_bounds_use_explicit_and_expected_arguments() {
    let declarations = DECLARATIONS
        .strip_prefix("package bounds\n")
        .expect("fixture package");
    let source = format!(
        "{declarations}\n\
        fun box(): String {{\n\
            val explicit = Compound<Int, Marked<Int>>(Leaf(1))\n\
            val contextual: Compound<Int, Marked<Int>> = Compound(Leaf(2))\n\
            return if (explicit.leaf.n + contextual.leaf.n == 3) \"OK\" else \"wrong values\"\n\
        }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "dependent_constructor_bounds");
}

#[test]
fn dependency_constructor_applied_bounds_match_the_complete_reference_inventory() {
    let classes = common::classes_against_kotlinc_lib(
        "DependentConstructorBounds",
        &[("Declarations.kt", DECLARATIONS)],
        "import bounds.*\nfun explicit() = Compound<Int, Marked<Int>>(Leaf(1))\nfun contextual(): Compound<Int, Marked<Int>> = Compound(Leaf(2))\n",
    ).expect("reference compiler provisions the dependent-bound declarations");
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn contradictory_completed_constructor_arguments_match_every_reference_diagnostic() {
    let library = common::kotlinc_lib_out(&[("Declarations.kt", DECLARATIONS)])
        .expect("reference compiler provisions the dependent-bound declarations");
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import bounds.*\nfun wrong(): Any = Compound<String, Marked<Int>>(Leaf<Marker<Marked<Int>>>(1))\n",
        )],
        &[library, common::stdlib_jar()],
    );
    let expected = common::compiler_errors(&result.reference_stderr);
    assert!(
        !expected.is_empty(),
        "reference rejects the contradictory application"
    );
    assert_eq!(result.reference_code, 1);
    assert_eq!(result.krusty_code, 1);
    assert_eq!(
        common::compiler_errors(&format!("{}{}", result.krusty_stderr, result.krusty_stdout)),
        expected
    );
}

#[test]
fn failed_constructor_context_does_not_complete_a_nested_dependency_producer() {
    let library = common::kotlinc_lib_out(&[("Declarations.kt", DECLARATIONS)])
        .expect("reference compiler provisions the dependent-bound declarations");
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import bounds.*\nfun wrong(): Any = Compound<String, Marked<Int>>(Leaf(1))\n",
        )],
        &[library, common::stdlib_jar()],
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr).len(), 3);
    common::expect_identical_rejection(&result, "failed dependency constructor context");
}

#[test]
fn failed_constructor_context_does_not_complete_a_nested_source_producer() {
    let result = common::compiler_diagnostics(
        &[
            ("Declarations.kt", DECLARATIONS),
            (
                "Use.kt",
                "import bounds.*\nfun wrong(): Any = Compound<String, Marked<Int>>(Leaf(1))\n",
            ),
        ],
        &[common::stdlib_jar()],
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr).len(), 3);
    common::expect_identical_rejection(&result, "failed source constructor context");
}

#[test]
fn failed_constructor_context_preserves_independent_nested_argument_inference() {
    let source = "class Leaf<T>(val value: T)\n\
        class Compound<T : Number>(val leaf: Leaf<T>)\n\
        fun wrong() = Compound<String>(Leaf(\"K\"))\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "independent nested argument inference");
}

#[test]
fn failed_constructor_context_handles_a_fixed_nested_parameter_declaration() {
    let source = "class Leaf<T>(val n: Int)\n\
        class Compound<T : Number>(val leaf: Leaf<String>)\n\
        fun wrong() = Compound<String>(Leaf(1))\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "fixed nested parameter declaration");
}

#[test]
fn an_explicit_constructor_bound_failure_keeps_the_lambda_argument_diagnostic() {
    let source = "class Callback<T : Number>(block: (T) -> String)\n\
        fun wrong() = Callback<String> { value -> value.toString() }\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        vec![
            common::CompilerError {
                file: "Use.kt".to_owned(),
                line: 2,
                column: 24,
                message: "type argument is not within its bounds: type parameter 'T (of class Callback<T : Number>)' must be subtype of 'Number', but actual: 'String'.".to_owned(),
            },
            common::CompilerError {
                file: "Use.kt".to_owned(),
                line: 2,
                column: 32,
                message: "argument type mismatch: actual type is '(String) -> String', but '(String) -> String' was expected.".to_owned(),
            },
        ]
    );
    common::expect_identical_rejection(&result, "explicit constructor bound and inferred lambda");
}

#[test]
fn an_explicit_constructor_bound_failure_preserves_a_written_lambda_input() {
    let source = "class Callback<T : Number>(block: (T) -> String)\n\
        fun wrong() = Callback<String> { value: String -> value.toString() }\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(
        &result,
        "explicit constructor bound and written lambda input",
    );
}

#[test]
fn an_explicit_bound_failure_keeps_each_arguments_declaration_constraints() {
    for source in [
        "class Value<T : Number>(val value: T)\n\
            fun wrong() = Value<String>(\"value\")\n",
        "class Independent<T : Number>(block: (Int) -> String)\n\
            fun wrong() = Independent<String> { value -> value.toString() }\n",
        "class Producer<T : Number>(block: () -> T)\n\
            fun wrong() = Producer<String> { \"value\" }\n",
        "class Reordered<T : Number>(before: Int = 0, block: (T) -> String)\n\
            fun wrong() = Reordered<String>(block = { value -> value.toString() }, before = 1)\n",
        "class Multiple<T : Number>(first: (Int) -> String, last: () -> T)\n\
            fun wrong() = Multiple<String>({ value -> value.toString() }, { \"value\" })\n",
    ] {
        let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
        common::expect_identical_rejection(
            &result,
            "declaration-owned rejected argument constraint",
        );
    }
}

#[test]
fn an_explicit_dependency_constructor_bound_failure_keeps_the_lambda_diagnostic() {
    let library = common::kotlinc_lib_out(&[(
        "Declarations.kt",
        "package bounds\nclass Callback<T : Number>(block: (T) -> String)\n",
    )])
    .expect("reference provisions the bounded callback declaration");
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import bounds.Callback\nfun wrong() = Callback<String> { value -> value.toString() }\n",
        )],
        &[library, common::stdlib_jar()],
    );
    common::expect_identical_rejection(&result, "dependency constructor bound and inferred lambda");
}

#[test]
fn an_explicit_bound_failure_keeps_an_anonymous_function_constraint() {
    let source = "class Callback<T : Number>(block: (T) -> String)\n\
        fun wrong() = Callback<String>(fun(value: String): String = value)\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "constructor bound and anonymous function");
}

#[test]
fn an_explicit_bound_failure_preserves_a_pretyped_function_argument() {
    let source = "class Callback<T : Number>(block: (T) -> String)\n\
        val block: (String) -> String = { value -> value }\n\
        fun wrong() = Callback<String>(block)\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "constructor bound and pretyped function value");
}

#[test]
fn an_explicit_bound_failure_keeps_a_callable_reference_constraint() {
    let source = "class Callback<T : Number>(block: (T) -> String)\n\
        fun text(value: String): String = value\n\
        fun wrong() = Callback<String>(::text)\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        vec![
            common::CompilerError {
                file: "Use.kt".to_owned(),
                line: 3,
                column: 15,
                message: "inapplicable candidate(s): constructor<T : Number>(block: (T) -> String): Callback<T>".to_owned(),
            },
            common::CompilerError {
                file: "Use.kt".to_owned(),
                line: 3,
                column: 24,
                message: "type argument is not within its bounds: type parameter 'T (of class Callback<T : Number>)' must be subtype of 'Number', but actual: 'String'.".to_owned(),
            },
            common::CompilerError {
                file: "Use.kt".to_owned(),
                line: 3,
                column: 34,
                message: "inapplicable candidate(s): fun text(value: String): String".to_owned(),
            },
        ]
    );
    common::expect_identical_rejection(&result, "constructor bound and callable reference");
}

#[test]
fn a_dependency_constructor_bound_failure_keeps_the_selected_reference_diagnostic() {
    let library = common::kotlinc_lib_out(&[(
        "Declarations.kt",
        "package bounds\n\
            class Callback<T : Number>(block: (T) -> String)\n\
            fun text(value: String): String = value\n",
    )])
    .expect("reference provisions the bounded constructor and selected function");
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import bounds.*\nfun wrong() = Callback<String>(::text)\n",
        )],
        &[library, common::stdlib_jar()],
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr).len(), 3);
    common::expect_identical_rejection(
        &result,
        "dependency constructor bound and selected reference",
    );
}

fn expect_rejected_reference(source: &str, label: &str) {
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    assert_eq!(
        common::compiler_errors(&result.reference_stderr).len(),
        3,
        "{label}"
    );
    common::expect_identical_rejection(&result, label);
}

#[test]
fn a_constructor_bound_failure_keeps_a_bound_member_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (T) -> String)\n\
            class Text { fun text(value: String): String = value }\n\
            fun wrong() = Callback<String>(Text()::text)\n",
        "bound member reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_an_unbound_member_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (Text, T) -> String)\n\
            class Text { fun text(value: String): String = value }\n\
            fun wrong() = Callback<String>(Text::text)\n",
        "unbound member reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_an_extension_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (T) -> String)\n\
            fun String.text(): String = this\n\
            fun wrong() = Callback<String>(String::text)\n",
        "extension reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_an_adapted_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (T) -> String)\n\
            fun text(value: String, suffix: String = \"\"): String = value + suffix\n\
            fun wrong() = Callback<String>(::text)\n",
        "default-adapted reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_a_local_function_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (T) -> String)\n\
            fun wrong() {\n\
                fun text(value: String): String = value\n\
                Callback<String>(::text)\n\
            }\n",
        "local function reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_a_constructor_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (T) -> Text)\n\
            class Text(value: String)\n\
            fun wrong() = Callback<String>(::Text)\n",
        "constructor reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_a_generic_constructor_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (T) -> Text<String>)\n\
            class Text<U>(value: U)\n\
            fun wrong() = Callback<String>(::Text)\n",
        "generic constructor reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_a_bound_extension_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: () -> String)\n\
            fun String.text(): String = this\n\
            fun wrong() = Callback<String>(\"value\"::text)\n",
        "bound extension reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_a_local_extension_reference() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (T) -> String)\n\
            fun wrong() {\n\
                fun String.text(): String = this\n\
                Callback<String>(String::text)\n\
            }\n",
        "local extension reference after constructor bound failure",
    );
}

#[test]
fn a_constructor_bound_failure_keeps_an_independent_reference_constraint() {
    expect_rejected_reference(
        "class Callback<T : Number>(block: (Int) -> String)\n\
            fun text(value: Int): String = value.toString()\n\
            fun wrong() = Callback<String>(::text)\n",
        "independent reference constraint after constructor bound failure",
    );
}

#[test]
fn a_valid_constructor_bound_preserves_inferred_and_written_lambda_inputs() {
    let source = r#"
class Callback<T : Number>(val block: (T) -> String)
fun box(): String {
    val inferred = Callback<Int> { value -> value.inc().toString() }
    val written = Callback<Int> { value: Int -> (value + 2).toString() }
    return if (inferred.block(41) == "42" && written.block(40) == "42") "OK" else "wrong inputs"
}
"#;
    common::expect_box_same_as_kotlinc(source, "valid_constructor_bound_lambda_inputs");
}

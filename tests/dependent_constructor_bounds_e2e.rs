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

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

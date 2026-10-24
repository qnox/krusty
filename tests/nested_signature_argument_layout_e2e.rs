use super::common;

const DECLARATIONS: &str = "class Outer<O> { inner class Inner<I> }\n\
    class Many<A, B> { inner class Middle<C> { inner class Leaf<D, E> } }\n\
    class Variance<out O, in P> { inner class Cell<in I, out R> }\n";

const USES: &str =
    "fun declared(x: Outer<Number>.Inner<String>): Outer<Number>.Inner<String> = x\n\
    fun inferred(x: Outer<Number>.Inner<String>) = x\n\
    fun three(x: Many<Number, CharSequence>.Middle<Long>.Leaf<String, Int>) = x\n\
    fun variance(x: Variance<Number, String>.Cell<CharSequence, List<String>>) = x\n\
    fun innerBottom(x: Variance<Number, String>.Cell<Nothing, String>) = x\n\
    fun outerBottom(x: Variance<Nothing, String>.Cell<CharSequence, String>) = x\n";

fn assert_complete_abi(classes: common::ClassSets) {
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

#[test]
fn nested_source_signatures_keep_own_and_captured_arguments_on_their_declarations() {
    assert_complete_abi(common::classes_against_kotlinc_module(&[
        ("Declarations.kt", DECLARATIONS),
        ("Uses.kt", USES),
    ]));
}

#[test]
fn nested_dependency_signatures_keep_semantic_argument_and_variance_order() {
    assert_complete_abi(
        common::classes_against_kotlinc_lib(
            "NestedSignatureLayout",
            &[("Declarations.kt", DECLARATIONS)],
            USES,
        )
        .expect("reference provisions the inner declarations"),
    );
}

use super::common;

const DECLARATIONS: &str = "class Outer<O : Number> { inner class Inner<T : O>(val value: T) }\n\
    typealias Fixed = Outer<Long>.Inner<Long>\n\
    typealias Generic<T> = Outer<Number>.Inner<T>\n\
    typealias Flip<T, O> = Outer<O>.Inner<T>\n\
    typealias Ignored<U> = Outer<Long>.Inner<Long>\n";

const CALLBACK_DECLARATIONS: &str =
    "class CallbackOuter<O> { inner class Inner<T>(block: (T) -> String) }\n\
    typealias Callback<T> = CallbackOuter<Number>.Inner<T>\n";

fn assert_failed_callback_context(declarations: &str, call: &str, expected_count: usize) {
    let result = common::compiler_diagnostics(
        &[("Use.kt", &format!("{declarations}{call}"))],
        &[common::stdlib_jar()],
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr).len(),
        expected_count
    );
    common::expect_identical_rejection(&result, "failed alias constructor lambda context");
}

#[test]
fn failed_constructor_context_reports_the_open_formal_on_the_written_lambda_parameter() {
    assert_failed_callback_context(
        CALLBACK_DECLARATIONS,
        "fun callback(o: CallbackOuter<Int>) = o.Callback { value -> value.toString() }\n",
        3,
    );
}

#[test]
fn failed_constructor_context_preserves_an_explicit_lambda_parameter_type() {
    assert_failed_callback_context(
        CALLBACK_DECLARATIONS,
        "fun callback(o: CallbackOuter<Int>) = o.Callback { value: String -> value.toString() }\n",
        2,
    );
}

#[test]
fn failed_constructor_context_preserves_an_explicit_constructor_type_argument() {
    assert_failed_callback_context(
        CALLBACK_DECLARATIONS,
        "fun callback(o: CallbackOuter<Int>) = o.Callback<String> { value -> value.toString() }\n",
        1,
    );
}

#[test]
fn failed_constructor_context_does_not_remove_an_independent_lambda_input() {
    assert_failed_callback_context(
        "class CallbackOuter<O> { inner class Result<T>(block: (Int) -> T) }\n\
        typealias Callback<T> = CallbackOuter<Number>.Result<T>\n",
        "fun callback(o: CallbackOuter<Int>) = o.Callback { value -> value }\n",
        2,
    );
}

#[test]
fn failed_constructor_context_uses_named_argument_mapping_for_lambda_inputs() {
    assert_failed_callback_context(
        "class CallbackOuter<O> { inner class Inner<T>(plain: (Int) -> String, dependent: (T) -> String) }\n\
        typealias Callback<T> = CallbackOuter<Number>.Inner<T>\n",
        "fun callback(o: CallbackOuter<Int>) = o.Callback(dependent = { value -> value.toString() }, plain = { n -> n.toString() })\n",
        3,
    );
}

#[test]
fn failed_constructor_context_preserves_other_inputs_in_the_same_lambda() {
    assert_failed_callback_context(
        "class CallbackOuter<O> { inner class Inner<T>(block: (Int, T) -> String) }\n\
        typealias Callback<T> = CallbackOuter<Number>.Inner<T>\n",
        "fun callback(o: CallbackOuter<Int>) = o.Callback { fixed, open -> fixed.inc().toString() + open.toString() }\n",
        3,
    );
}

#[test]
fn failed_constructor_context_does_not_use_the_rejected_result_to_type_a_lambda() {
    assert_failed_callback_context(
        CALLBACK_DECLARATIONS,
        "fun callback(o: CallbackOuter<Int>): Callback<String> = o.Callback { value -> value.length.toString() }\n",
        4,
    );
}

#[test]
fn failed_constructor_context_tracks_an_anonymous_functions_receiver_parameter() {
    assert_failed_callback_context(
        "class CallbackOuter<O> { inner class Inner<T>(block: T.() -> String) }\n\
        typealias Callback<T> = CallbackOuter<Number>.Inner<T>\n",
        "fun callback(o: CallbackOuter<Int>) = o.Callback(fun(value) = value.toString())\n",
        3,
    );
}

#[test]
fn failed_constructor_context_tracks_vararg_function_elements() {
    assert_failed_callback_context(
        "class CallbackOuter<O> { inner class Inner<T>(vararg blocks: (T) -> String) }\n\
        typealias Callback<T> = CallbackOuter<Number>.Inner<T>\n",
        "fun callback(o: CallbackOuter<Int>) = o.Callback({ value -> value.toString() })\n",
        3,
    );
}

#[test]
fn failed_constructor_context_tracks_declared_function_classifiers() {
    assert_failed_callback_context(
        "class CallbackOuter<O> { inner class Inner<T>(block: kotlin.Function1<T, String>) }\n\
        typealias Callback<T> = CallbackOuter<Number>.Inner<T>\n",
        "fun callback(o: CallbackOuter<Int>) = o.Callback { value -> value.toString() }\n",
        3,
    );
}

#[test]
fn failed_constructor_context_uses_dependency_alias_lambda_formals() {
    let library = common::kotlinc_lib_out(&[("Declarations.kt", CALLBACK_DECLARATIONS)])
        .expect("reference provisions the callback alias");
    for (call, expected_count) in [
        ("fun callback(o: CallbackOuter<Int>) = o.Callback { value -> value.toString() }\n", 3),
        ("fun callback(o: CallbackOuter<Int>) = o.Callback { value: String -> value.toString() }\n", 2),
    ] {
        let result = common::compiler_diagnostics(
            &[("Use.kt", call)],
            &[library.clone(), common::stdlib_jar()],
        );
        assert_eq!(common::compiler_errors(&result.reference_stderr).len(), expected_count);
        common::expect_identical_rejection(&result, "dependency alias lambda context");
    }
}

#[test]
fn admitted_constructor_context_preserves_expected_lambda_input_inference() {
    let source = format!(
        "{CALLBACK_DECLARATIONS}\
        fun callback(o: CallbackOuter<Number>): Callback<String> = o.Callback {{ value -> value.length.toString() }}\n\
        fun box(): String {{ callback(CallbackOuter<Number>()); return \"OK\" }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "admitted_constructor_lambda_context");
}

const CALLS: &str = "fun fixed(o: Outer<Long>) = o.Fixed(1)\n\
    fun explicit(o: Outer<Number>) = o.Generic<Long>(1)\n\
    fun inferred(o: Outer<Number>) = o.Generic(1)\n\
    fun reordered(o: Outer<Long>) = o.Flip<Long, Long>(1)\n\
    fun reorderedInferred(o: Outer<Long>) = o.Flip(1)\n\
    fun unused(o: Outer<Long>) = o.Ignored<String>(1)\n";

#[test]
fn bound_aliases_keep_the_original_constructor_value_class_carriers() {
    let declarations = r#"
@JvmInline value class Token(val value: Int)
class Outer<O> {
    inner class Inner(val value: O)
    inner class Direct(val value: Token)
}
typealias GenericSlot = Outer<Token>.Inner
typealias ConcreteSlot = Outer<Token>.Direct
"#;
    let consumer = r#"
fun inferred(outer: Outer<Token>) = outer.GenericSlot(Token(17))
fun concrete(outer: Outer<Token>) = outer.ConcreteSlot(Token(23))
fun box(): String {
    val outer = Outer<Token>()
    return if (inferred(outer).value.value == 17 && concrete(outer).value.value == 23)
        "OK" else "wrong values"
}
"#;
    common::expect_box_same_as_kotlinc(
        &format!("{declarations}{consumer}"),
        "bound_inner_alias_value_class_carriers",
    );
    assert_eq!(
        common::expect_box_run_against("BoundInnerAliasValueClassCarriers", declarations, consumer,),
        Some("OK".into()),
    );
    assert_eq!(
        common::expect_box_run_against_kotlinc(declarations, consumer),
        Some("OK".into()),
    );
}

#[test]
fn a_lexical_alias_shadows_the_package_alias_for_inferred_calls() {
    let source = "class Outer<O : Number> { inner class Inner<T : O>(val value: T) }\n\
        typealias Fixed = Outer<Int>.Inner<Int>\n\
        class Host {\n\
            typealias Fixed = Outer<Long>.Inner<Long>\n\
            fun inferred(o: Outer<Long>) = o.Fixed(1)\n\
        }\n\
        fun box(): String = if (Host().inferred(Outer<Long>()).value == 1L) \"OK\" else \"wrong alias\"\n";
    common::expect_box_same_as_kotlinc(source, "bound_inner_alias_lexical_priority");
}

#[test]
fn an_object_import_retains_both_alias_and_extension_declarations() {
    let source = "import Names.Pick\n\
        class Outer<O> { inner class Inner<T>(val value: T) }\n\
        object Names {\n\
            typealias Pick = Outer<Long>.Inner<Long>\n\
            fun Outer<Long>.Pick(value: String): String = value\n\
        }\n\
        fun inferred(o: Outer<Long>) = o.Pick(1)\n\
        fun box(): String {\n\
            val outer = Outer<Long>()\n\
            return if (inferred(outer).value == 1L && outer.Pick(\"K\") == \"K\") \"OK\" else \"wrong facet\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(source, "bound_inner_alias_object_import_facets");
}

#[test]
fn bound_alias_calls_infer_complete_results_from_the_selected_alias_application() {
    let source = format!(
        "{DECLARATIONS}{CALLS}\n\
        fun box(): String {{\n\
            val longs = Outer<Long>()\n\
            val numbers = Outer<Number>()\n\
            return if (fixed(longs).value + explicit(numbers).value + inferred(numbers).value + reordered(longs).value + reorderedInferred(longs).value + unused(longs).value == 6L) \"OK\" else \"wrong values\"\n\
        }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "bound_inner_alias_inference");
}

#[test]
fn dependency_bound_alias_calls_preserve_all_inferred_method_signatures() {
    let classes = common::classes_against_kotlinc_lib(
        "BoundInnerAliases",
        &[("Declarations.kt", DECLARATIONS)],
        CALLS,
    )
    .expect("reference provisions aliases and inner declarations");
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

const MEMBER_DECLARATIONS: &str = "class Receiver {\n\
    inner class Inner(val value: Long)\n\
    fun Pick(value: Number): String = \"member\"\n\
    fun Fallback(value: String): String = value\n\
}\n\
typealias Pick = Receiver.Inner\n\
typealias Fallback = Receiver.Inner\n";

const MEMBER_CALLS: &str = "fun selected(receiver: Receiver) = receiver.Pick(1L)\n\
    fun fallback(receiver: Receiver) = receiver.Fallback(2L)\n\
    fun box(): String {\n\
        val receiver = Receiver()\n\
        return if (selected(receiver) == \"member\" && fallback(receiver).value == 2L) \"OK\" else \"wrong priority\"\n\
    }\n";

#[test]
fn an_applicable_member_precedes_an_alias_with_a_more_specific_parameter() {
    common::expect_box_same_as_kotlinc(
        &format!("{MEMBER_DECLARATIONS}{MEMBER_CALLS}"),
        "bound_inner_alias_member_priority",
    );
}

#[test]
fn dependency_member_priority_preserves_the_complete_selected_abi() {
    let classes = common::classes_against_kotlinc_lib(
        "BoundInnerAliasPriority",
        &[("Declarations.kt", MEMBER_DECLARATIONS)],
        MEMBER_CALLS,
    )
    .expect("reference provisions the member and alias declarations");
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

fn assert_alias_errors(source: &str, classpath: &[std::path::PathBuf]) {
    let result = common::compiler_diagnostics(&[("Use.kt", source)], classpath);
    let expected = common::compiler_errors(&result.reference_stderr);
    assert!(!expected.is_empty(), "reference must reject the alias call");
    assert_eq!(result.reference_code, 1);
    assert_eq!(result.krusty_code, 1);
    assert_eq!(
        common::compiler_errors(&format!("{}{}", result.krusty_stderr, result.krusty_stdout)),
        expected
    );
}

const WRONG_CALLS: [&str; 8] = [
    "fun wrong(o: Outer<Int>) = o.Fixed(1L)\n",
    "fun wrong(o: Outer<Long>) = o.Flip<String, Long>(\"wrong\")\n",
    "fun wrong(o: Outer<Long>) = o.Ignored<String, Int>(1)\n",
    "fun wrong(o: Outer<Number>) = o.Generic<String>(\"wrong\")\n",
    "fun wrong(o: Outer<Long>) = o.Flip<Long, String>(1L)\n",
    "fun wrong(o: Outer<Number>) = o.Generic<Long>(\"wrong\")\n",
    "fun wrong(o: Outer<Int>) = o.Generic(1)\n",
    "fun wrong(o: Outer<Int>) = o.Generic<Long>(1)\n",
];

#[test]
fn source_alias_calls_preserve_receiver_bounds_and_written_alias_arity() {
    for call in WRONG_CALLS {
        assert_alias_errors(&format!("{DECLARATIONS}{call}"), &[common::stdlib_jar()]);
    }
}

#[test]
fn dependency_alias_calls_preserve_receiver_bounds_and_written_alias_arity() {
    let library = common::kotlinc_lib_out(&[("Declarations.kt", DECLARATIONS)])
        .expect("reference provisions aliases and inner declarations");
    for call in WRONG_CALLS {
        assert_alias_errors(call, &[library.clone(), common::stdlib_jar()]);
    }
}

#[test]
fn rejected_aliases_infer_only_from_the_receiver_not_arguments_or_results() {
    let declarations = "class Outer<O : Number> { inner class Inner<T : O>(val value: T) }\n\
        typealias Generic<T> = Outer<Number>.Inner<T>\n\
        class PairOuter<A, B> { inner class Inner<T>(val value: T) }\n\
        typealias Partial<T, A> = PairOuter<A, Number>.Inner<T>\n";
    let calls = "fun declared(o: Outer<Int>): Outer<Number>.Inner<Int> = o.Generic(1)\n\
        fun partial(o: PairOuter<Long, Int>) = o.Partial(1)\n\
        fun invalidArguments(o: Outer<Int>) = o.Generic(\"wrong\")\n\
        fun invalidArgumentsWithResult(o: Outer<Int>): Outer<Number>.Inner<Long> = o.Generic(\"wrong\")\n";
    assert_alias_errors(&format!("{declarations}{calls}"), &[common::stdlib_jar()]);
    let library = common::kotlinc_lib_out(&[("Declarations.kt", declarations)])
        .expect("reference provisions both alias receiver shapes");
    assert_alias_errors(calls, &[library, common::stdlib_jar()]);
}

const PAIR_DECLARATIONS: &str =
    "class Owner<O> { inner class Pair<A, B>(val first: A, val second: B) }\n\
    typealias Reordered<B, A> = Owner<Long>.Pair<A, B>\n";

const PAIR_CALLS: &str = "fun explicit(o: Owner<Long>) = o.Reordered<String, Int>(second = \"K\", first = 1)\n\
    fun inferred(o: Owner<Long>) = o.Reordered(second = \"O\", first = 2)\n\
    fun box(): String {\n\
        val owner = Owner<Long>()\n\
        return if (explicit(owner).first == 1 && explicit(owner).second == \"K\" && inferred(owner).first == 2 && inferred(owner).second == \"O\") \"OK\" else \"wrong mapping\"\n\
    }\n";

#[test]
fn named_arguments_and_reordered_alias_formals_share_the_selected_substitution() {
    common::expect_box_same_as_kotlinc(
        &format!("{PAIR_DECLARATIONS}{PAIR_CALLS}"),
        "bound_inner_alias_named_arguments",
    );
}

#[test]
fn dependency_named_arguments_preserve_the_complete_selected_alias_abi() {
    let classes = common::classes_against_kotlinc_lib(
        "BoundInnerAliasNamedArguments",
        &[("Declarations.kt", PAIR_DECLARATIONS)],
        PAIR_CALLS,
    )
    .expect("reference provisions the reordered alias");
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
fn independent_java_constructor_formals_remain_rigid_through_an_alias() {
    // Kotlin's alias constructor does not infer the underlying constructor's independent U.
    let java = "public class OuterJava<O extends Number> {\n\
        public class Inner<T extends O> {\n\
            public <U extends CharSequence> Inner(T value, U tag) {}\n\
        }\n\
    }\n";
    let library = common::javac_compile(&[("OuterJava.java".into(), java.into())], &[])
        .expect("reference Java declarations compile")
        .0;
    let source = "typealias Fixed = OuterJava<Long>.Inner<Long>\n\
        typealias Generic<T> = OuterJava<Number>.Inner<T>\n\
        fun fixed(o: OuterJava<Long>) = o.Fixed(1, \"tag\")\n\
        fun explicit(o: OuterJava<Number>) = o.Generic<Long>(1, \"tag\")\n\
        fun inferred(o: OuterJava<Number>) = o.Generic(1, \"tag\")\n\
        typealias Flip<T, O> = OuterJava<O>.Inner<T>\n\
        fun reordered(o: OuterJava<Long>) = o.Flip<Long, Long>(1, \"tag\")\n\
        fun reorderedInferred(o: OuterJava<Long>) = o.Flip(1, \"tag\")\n\
        typealias Unused<U> = OuterJava<Long>.Inner<Long>\n\
        fun sameSpelling(o: OuterJava<Long>) = o.Unused<String>(1, \"tag\")\n";
    assert_alias_errors(source, &[library, common::stdlib_jar()]);
}

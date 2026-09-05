//! Definitely-non-null intersection types `T & Any` (KT DefinitelyNonNullableTypes). The `& Any`
//! folds into the left operand as a non-null type; `T & Any` erases identically to `T`, so it appears
//! in property/parameter/return positions and type arguments. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn diagnostics(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

#[test]
fn dnn_property_type() {
    // A generic data class whose property is `T & Any` — non-null even when T is nullable.
    const SRC: &str = "data class Some<T>(val data: T & Any)\n\
        fun box(): String {\n\
        \x20 val x = Some<String?>(\"OK\")\n\
        \x20 return x.data\n\
        }\n";
    assert_eq!(run(SRC).expect("dnn property"), "OK");
}

#[test]
fn dnn_function_return_and_param() {
    // `T & Any` in parameter and return positions.
    const SRC: &str = "fun <T> firstNonNull(a: T & Any): T & Any = a\n\
        fun box(): String {\n\
        \x20 return firstNonNull<String?>(\"OK\")\n\
        }\n";
    assert_eq!(run(SRC).expect("dnn param/return"), "OK");
}

#[test]
fn nullable_arguments_infer_source_function_type_parameters() {
    const SRC: &str = "fun <T> id(x: T): T = x\n\
        fun <T> take(vararg x: T): Int = x.size\n\
        fun <T> choose(a: T, b: T): T = a\n\
        fun expected(): String? = choose(null, null)\n\
        fun box(): String {\n\
        \x20 if (id(null) != null) return \"id\"\n\
        \x20 if (id(null).hashCode() != 0) return \"hash\"\n\
        \x20 if (expected().hashCode() != 0) return \"expected\"\n\
        \x20 return if (take(null) == 1) \"OK\" else \"vararg\"\n\
        }\n";
    assert_eq!(run(SRC).expect("nullable source generic calls"), "OK");
}

#[test]
fn bounded_generic_vararg_before_trailing_lambda_is_packed() {
    const SRC: &str =
        "fun <T : Comparable<T>> collect(vararg values: T, block: Array<T>.() -> Unit) {}\n\
        fun box(): String {\n\
        \x20 collect(42, 43) { }\n\
        \x20 return \"OK\"\n\
        }\n";
    let result = run(SRC).unwrap_or_else(|| panic!("{:?}", diagnostics(SRC)));
    assert_eq!(result, "OK");

    let errors = diagnostics(
        "fun <T : Any> collect(vararg values: T, block: Array<T>.() -> Unit) {}\n\
         fun bad() { collect(42, null) { } }\n",
    );
    assert!(
        errors.iter().any(|message| {
            message.contains("argument type mismatch")
                || message.contains("null cannot be a value of a non-null type")
        }),
        "{errors:?}"
    );
}

#[test]
fn dnn_constructor_parameter_uses_explicit_type_argument() {
    let diagnostics =
        diagnostics("data class Some<T>(val data: T & Any)\nfun f() { Some<String?>(1) }");
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("argument type mismatch")),
        "{diagnostics:?}"
    );
}

#[test]
fn inferred_constructor_type_argument_respects_upper_bound() {
    let diagnostics = diagnostics("class C<T : Number>(val value: T)\nfun f() { C(\"bad\") }");
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("argument type mismatch")),
        "{diagnostics:?}"
    );
}

#[test]
fn source_function_type_arguments_respect_dnn_and_upper_bounds() {
    for source in [
        "fun <T> requireValue(value: T & Any) = value\nfun f() { requireValue<String?>(null) }",
        "fun <T> requireValue(value: T & Any) = value\nfun f() { requireValue(null) }",
        "data class Some<T>(val data: T & Any)\nfun f() { Some(null) }",
        "data class Some<T>(val data: T & Any)\nfun f() { Some(data = null) }",
        "data class Some<T>(val data: T & Any)\nfun f() { Some<String?>(data = null) }",
        "fun <T : Number> requireNumber(value: T) = value\nfun f() { requireNumber(\"bad\") }",
        "fun <T> exact(value: T) = value\nfun f() { exact<String>(1) }",
        "class NonNull<T : Any>(val value: T)\nfun f() { NonNull(null) }",
    ] {
        let diagnostics = diagnostics(source);
        if diagnostics
            .iter()
            .any(|message| message == "<skip: no stdlib>")
        {
            continue;
        }
        assert!(
            diagnostics.iter().any(|message| {
                message.contains("argument type mismatch")
                    || message.contains("null cannot be a value")
                    || message.contains("cannot infer type for type parameter")
            }),
            "{source}: {diagnostics:?}"
        );
    }
}

#[test]
fn null_cannot_infer_a_definitely_non_null_function_parameter() {
    let diagnostics = diagnostics(
        "fun <T> f(x: T & Any): T & Any = x\n\
         fun g() { f(null) }",
    );
    assert_eq!(
        diagnostics,
        ["cannot infer type for type parameter 'T'. Specify it explicitly."]
    );
}

#[test]
fn intersection_requires_type_parameter_and_any_rhs() {
    for source in [
        "fun f(value: String & Any) = value",
        "fun <T> f(value: T & String) = value",
        "fun <T> f(value: T? & Any) = value",
        "fun <T> f(value: T<String> & Any) = value",
        "fun f(value: String & Int) = value",
    ] {
        let diagnostics = diagnostics(source);
        if diagnostics
            .iter()
            .any(|message| message == "<skip: no stdlib>")
        {
            continue;
        }
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("definitely non-null")),
            "{source}: {diagnostics:?}"
        );
    }
}

#[test]
fn nullable_any_upper_bound_accepts_null() {
    const SRC: &str = "fun <T : Any?> id(x: T): T = x\n\
        fun <A : Any?, T : A> chainedId(x: T): T = x\n\
        class C<T : Any?>(val x: T)\n\
        fun box(): String {\n\
        \x20 if (id(null) != null) return \"function\"\n\
        \x20 if (id(null).hashCode() != 0) return \"function-hash\"\n\
        \x20 if (chainedId(null).hashCode() != 0) return \"chained-hash\"\n\
        \x20 if (C(null).x.hashCode() != 0) return \"constructor-hash\"\n\
        \x20 if (C(x = null).x.hashCode() != 0) return \"named-constructor-hash\"\n\
        \x20 if (C<String?>(x = null).x.hashCode() != 0) return \"named-explicit-hash\"\n\
        \x20 return if (C(null).x == null) \"OK\" else \"constructor\"\n\
        }\n";
    assert_eq!(run(SRC).expect("nullable Any upper bounds"), "OK");
}

#[test]
fn constructor_inference_merges_repeated_type_parameter_occurrences() {
    const SRC: &str = "class PairBox<T>(val a: T, val b: T)\n\
        fun box(): String {\n\
        \x20 val pair = PairBox(\"x\", null)\n\
        \x20 val named = PairBox(b = null, a = \"x\")\n\
        \x20 return if (pair.a == \"x\" && pair.b == null && named.a == \"x\" && named.b == null) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("merged constructor inference"), "OK");
}

#[test]
fn symbolic_getter_inference_keeps_merged_nullable_constructor_binding() {
    let source = "class PairBox<T>(val a: T, val b: T)\n\
        class Host {\n\
        \x20 val String.pair get() = PairBox(\"x\", null)\n\
        \x20 fun bad(): Int = \"\".pair.b.length\n\
        }\n";
    let diagnostics = diagnostics(source);
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("only safe") || message.contains("nullable receiver")),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .all(|message| !message.contains("unresolved reference")),
        "the inferred PairBox<String?> shape must reach member lookup: {diagnostics:?}"
    );
}

#[test]
fn constructor_inference_merges_bare_and_nested_type_parameter_occurrences() {
    let source = "class Mixed<T>(val value: T, val xs: List<T>) {\n\
        \x20 fun first(): T = xs[0]\n\
        }\n\
        class Host {\n\
        \x20 val String.mixed get() = Mixed(\"x\", listOf(null))\n\
        \x20 fun bad(): Int = \"\".mixed.first().length\n\
        }\n";
    let diagnostics = diagnostics(source);
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        diagnostics.iter().any(|message| {
            message.contains("only safe")
                || message.contains("nullable receiver")
                || message.contains("unresolved reference")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn repeated_constructor_constraints_can_meet_at_semantic_upper_bound() {
    let source = "class C<T : CharSequence>(val a: T, val b: T)\n\
        fun f() = C(\"x\", StringBuilder(\"y\"))\n";
    let diagnostics = diagnostics(source);
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn generic_function_constructor_still_requires_a_function_argument() {
    // The return component `T` is inferred from the argument, but that must not erase the enclosing
    // function contract. In particular, bypassing the whole `(Int) -> T` parameter because it mentions
    // `T` would admit an arbitrary object and defer a verifier/runtime failure to the generated call.
    let source = "class C<T>(val transform: (Int) -> T)\n\
        fun bad() { C<String>(\"not a function\") }\n";
    let diagnostics = diagnostics(source);
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        diagnostics
            .iter()
            .any(|message| { message.contains("type mismatch") || message.contains("candidate") }),
        "{diagnostics:?}"
    );
}

#[test]
fn concrete_secondary_beats_an_incompatible_generic_function_primary() {
    // The primary mentions `T`, but its concrete outer shape is still a function. It must not enter
    // overload competition for a String argument and make the valid String secondary ambiguous.
    const SOURCE: &str = "class Constant<T>(val value: T) : (Int) -> T {\n\
        \x20 override fun invoke(index: Int): T = value\n\
        }\n\
        class C<T>(val transform: (Int) -> T) {\n\
        \x20 constructor(value: String) : this(Constant(value) as (Int) -> T)\n\
        }\n\
        fun box(): String = C<String>(\"OK\").transform(0)\n";
    let front_end = diagnostics(SOURCE);
    assert!(front_end.is_empty(), "{front_end:?}");
    assert_eq!(
        run(SOURCE).expect("generic function primary versus concrete secondary"),
        "OK"
    );
}

#[test]
fn contravariant_constructor_function_input_does_not_widen_binding() {
    let source = "class C<T : CharSequence>(val value: T, val consume: (T) -> Unit)\n\
        fun f() = C(\"x\", { _: Any -> })\n";
    let diagnostics = diagnostics(source);
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn contravariant_only_constructor_constraint_is_retained() {
    let source = "class C<T>(val consume: (T) -> Unit)\n\
        fun bad() {\n\
        \x20 val consumeString: (String) -> Unit = { }\n\
        \x20 val c = C(consumeString)\n\
        \x20 c.consume(1)\n\
        }\n";
    let diagnostics = diagnostics(source);
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("type mismatch")),
        "{diagnostics:?}"
    );

    const RUN_SRC: &str = "class C<T>(val consume: (T) -> Unit)\n\
        fun box(): String {\n\
        \x20 val consumeString: (String) -> Unit = { value -> if (value != \"OK\") throw RuntimeException(value) }\n\
        \x20 val c = C(consumeString)\n\
        \x20 c.consume(\"OK\")\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(
        run(RUN_SRC).expect("contravariant-only constructor inference"),
        "OK"
    );
}

#[test]
fn nullable_contravariant_only_function_property_is_substituted() {
    let source = "class C<T>(val consume: ((T) -> Unit)?)\n\
        fun bad() {\n\
        \x20 val consumeString: (String) -> Unit = { }\n\
        \x20 val c = C(consumeString)\n\
        \x20 c.consume!!(1)\n\
        }\n";
    let diagnostics = diagnostics(source);
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("type mismatch")),
        "{diagnostics:?}"
    );

    const RUN_SRC: &str = "class C<T>(val consume: ((T) -> Unit)?)\n\
        fun box(): String {\n\
        \x20 val consumeString: (String) -> Unit = { value -> if (value != \"OK\") throw RuntimeException(value) }\n\
        \x20 val c = C(consumeString)\n\
        \x20 c.consume!!(\"OK\")\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(
        run(RUN_SRC).expect("nullable contravariant-only constructor inference"),
        "OK"
    );
}

#[test]
fn non_null_top_level_extension_property_rejects_nullable_receiver() {
    let source = "val String.first: Char get() = this[0]\nfun bad(s: String?): Char = s.first\n";
    let diagnostics = diagnostics(source);
    if diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        diagnostics.iter().any(|message| {
            message.contains("only safe")
                || message.contains("nullable receiver")
                || message.contains("unresolved reference")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn inferred_nullable_constructor_type_is_retained_for_member_reads() {
    let unsafe_member = "class C<T : CharSequence?>(val x: T)\nfun f() { C(null).x.length }";
    let unsafe_diagnostics = diagnostics(unsafe_member);
    if unsafe_diagnostics
        .iter()
        .any(|message| message == "<skip: no stdlib>")
    {
        return;
    }
    assert!(
        unsafe_diagnostics
            .iter()
            .any(|message| message.contains("only safe")
                || message.contains("nullable receiver")
                || message.contains("unresolved reference")),
        "{unsafe_member}: {unsafe_diagnostics:?}"
    );

    // `C(null).x` is `Nothing?`, which is a subtype of the nullable receiver of Kotlin's
    // `String?.plus(Any?)` extension. Kotlinc 2.4.10 accepts this expression; retaining the inferred
    // nullable-bottom type must therefore keep that real extension applicable, not manufacture a
    // numeric nullable-receiver diagnostic.
    let nullable_plus = "class C<T : Int?>(val x: T)\nfun f() { C(null).x + 1 }";
    assert!(diagnostics(nullable_plus).is_empty(), "{nullable_plus}");
}

#[test]
fn dnn_cast_throws_npe_on_null() {
    // `t as (T & Any)` on an unbounded (nullable-bound) `T` null-checks like kotlinc: `null` throws
    // NullPointerException, a non-null value passes through. (The Kotlin box-corpus case
    // `casts/castToDefinitelyNotNullType.kt`.)
    const SRC: &str = "fun <T> test(t: T) = t as (T & Any)\n\
        fun box(): String =\n\
        \x20 try {\n\
        \x20   test<Any?>(null)\n\
        \x20   \"FAIL: expected NPE\"\n\
        \x20 } catch (ex: NullPointerException) {\n\
        \x20   test(\"OK\")\n\
        \x20 }\n";
    assert_eq!(run(SRC).expect("dnn cast NPE"), "OK");
}

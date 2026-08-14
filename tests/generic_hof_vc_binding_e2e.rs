//! A NON-inline generic HOF whose type parameter is bound to a VALUE CLASS at the call site
//! (`fun <T, R> bar(v: T, f: (T) -> R): R` called as `bar(IC(40)) { it.value }`). The resolver
//! declined value-class bindings in `user_generic_call` ("needs unboxing, not a cast"), so the
//! lambda's `it` stayed erased `kotlin/Any` and any member read failed to resolve
//! ("unresolved member 'value' on 'kotlin/Any'" — the corpus `unboxGenericParameter/*` bucket).
//! The erased lambda boundary carries the value BOXED, so `it` must be typed as the value class
//! with a boxed-slot representation, and each read must unbox — the same machinery a DECLARED
//! value-class function type (`(IC) -> R`) already uses.
use super::common;

fn run(tag: &str, main: &str) -> Option<String> {
    let _ = tag;
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    common::compile_and_run_box(main, "Main", &[sl, jdk.clone()], Some(jdk.as_path()))
}

#[test]
fn vc_any_underlying_lambda_reads_value() {
    // Reference (Any) underlying: `it.value as T` inside the lambda, cast drives the generic return.
    const MAIN: &str = "@JvmInline value class IC(val value: Any)\n\
        fun <T, R> bar(v: T, f: (T) -> R): R = f(v)\n\
        @Suppress(\"UNCHECKED_CAST\")\n\
        fun <T> underlying(a: IC): T = bar(a) { it.value as T }\n\
        fun box(): String {\n\
            val res = underlying<Int>(IC(40)) + 2\n\
            return if (res == 42) \"OK\" else \"FAIL: $res\"\n\
        }\n";
    assert_eq!(
        run("vc_any", MAIN).expect("vc any-underlying generic lambda"),
        "OK"
    );
}

#[test]
fn vc_int_underlying_lambda_reads_value() {
    // Scalar (Int) underlying: the boxed IC crossing the erased lambda boundary must unbox to read.
    const MAIN: &str = "@JvmInline value class IC(val value: Int)\n\
        fun <T, R> bar(v: T, f: (T) -> R): R = f(v)\n\
        fun box(): String {\n\
            val res = bar(IC(40)) { it.value } + 2\n\
            return if (res == 42) \"OK\" else \"FAIL: $res\"\n\
        }\n";
    assert_eq!(
        run("vc_int", MAIN).expect("vc int-underlying generic lambda"),
        "OK"
    );
}

#[test]
fn vc_string_underlying_lambda_reads_value() {
    const MAIN: &str = "@JvmInline value class IC(val value: String)\n\
        fun <T, R> bar(v: T, f: (T) -> R): R = f(v)\n\
        fun box(): String = bar(IC(\"OK\")) { it.value }\n";
    assert_eq!(
        run("vc_string", MAIN).expect("vc string-underlying generic lambda"),
        "OK"
    );
}

#[test]
fn generic_extension_result_uses_the_enclosing_lambda_expectation() {
    const MAIN: &str = "@JvmInline value class IC(val value: String)\n\
        fun <T, R> bar(v: T, f: (T) -> R): R = f(v)\n\
        @Suppress(\"UNCHECKED_CAST\")\n\
        fun <T> IC.extensionValue(): T = value as T\n\
        fun <T> extension(a: IC): T = bar(a) { it.extensionValue() }\n\
        fun box(): String = extension<String>(IC(\"OK\"))\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("GenericHofExtensionResultReference", MAIN);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected expected-result inference through the lambda: {reference_stderr}"
    );
    assert_eq!(
        run("vc_extension_result", MAIN).expect("generic extension result through lambda"),
        "OK"
    );
}

#[test]
fn generic_extension_result_uses_a_library_lambda_expectation() {
    const MAIN: &str = "@JvmInline value class IC(val value: String)\n\
        @Suppress(\"UNCHECKED_CAST\")\n\
        fun <T> IC.extensionValue(): T = value as T\n\
        fun <T> extension(values: List<IC>): List<T> =\n\
            values.map { it.extensionValue() }\n\
        fun box(): String = extension<String>(listOf(IC(\"OK\"))).single()\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("LibraryLambdaExtensionResultReference", MAIN);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected the library-lambda expected-result fixture: {reference_stderr}"
    );
    assert_eq!(
        run("library_lambda_extension_result", MAIN)
            .expect("generic extension result through library lambda"),
        "OK"
    );
}

#[test]
fn postponed_callee_formal_does_not_capture_enclosing_result_parameter() {
    const MAIN: &str = "fun <A, R> select(f: (A) -> R): R = TODO()\n\
        fun <T> bad(): T = select { _: String -> \"bad\" }\n";
    let (reference_code, _) =
        common::kotlinc_source_result("PostponedCalleeFormalOwnershipReference", MAIN);
    assert_ne!(
        reference_code, 0,
        "kotlinc unexpectedly accepted a concrete String as universally quantified T"
    );

    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics =
        common::front_end_diagnostics(MAIN, &[stdlib, jdk.clone()], Some(jdk.as_path()));
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("type mismatch")
                && diagnostic.contains("String")
                && diagnostic.contains("T")
        }),
        "callee inference must not solve the enclosing T: {diagnostics:?}"
    );
}

#[test]
fn nested_postponed_hof_frames_keep_their_formal_owners() {
    const MAIN: &str = "interface Provider<A, R> { fun value(): A }\n\
        interface Scope<S> { fun <B> get(provider: Provider<S, B>): B }\n\
        @Suppress(\"UNCHECKED_CAST\", \"UNUSED_PARAMETER\")\n\
        fun <A, R> build(block: Scope<A>.() -> R): Provider<A, R> =\n\
            object : Provider<A, R> { override fun value(): A = \"OK\" as A }\n\
        @Suppress(\"UNCHECKED_CAST\", \"UNUSED_PARAMETER\")\n\
        fun <A, R> build2(block: Scope<A>.() -> R): Provider<A, R> =\n\
            object : Provider<A, R> { override fun value(): A = \"OK\" as A }\n\
        val anyProvider: Provider<Any, Any> =\n\
            object : Provider<Any, Any> { override fun value(): Any = \"unused\" }\n\
        val nested = build { get(build2 { get(anyProvider) }) }\n\
        fun box(): String = nested.value().toString()\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("NestedPostponedHofOwnershipReference", MAIN);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected nested postponed HOF inference: {reference_stderr}"
    );
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            MAIN,
            "nested_postponed_hof_ownership",
            &[stdlib, jdk.clone()],
            Some(jdk.as_path()),
        ),
        "OK"
    );
}

#[test]
fn overloaded_library_lambda_result_is_selected_from_its_body() {
    const MAIN: &str = "fun box(): String {\n\
        val result = listOf(1, 2).sumOf { value -> run { value } }\n\
        return if (result == 3) \"OK\" else \"FAIL: $result\"\n\
    }\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("OverloadedLambdaBodySelectionReference", MAIN);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected lambda-return overload selection: {reference_stderr}"
    );
    assert_eq!(
        run("overloaded_lambda_body_selection", MAIN)
            .expect("lambda body selects the sumOf overload"),
        "OK"
    );
}

#[test]
fn nullable_generic_return_keeps_null() {
    // A declared-nullable generic return (`fun <T> ...: T?`) with a primitive binding stays BOXED
    // (`Int?`): the erased result may be `null`, so the call result must NOT be eagerly unboxed
    // (NPE) nor round-tripped through unbox+rebox on the way into an `Int?` context.
    const MAIN: &str = "@Suppress(\"UNCHECKED_CAST\")\n\
        fun <T> uncheckedNull(): T = null as T\n\
        fun <T> orNull(x: T): T? = null\n\
        fun box(): String {\n\
            val a: Int? = uncheckedNull<Int>()\n\
            if (a != null) return \"FAIL a: $a\"\n\
            val b: Int? = orNull(5)\n\
            if (b != null) return \"FAIL b: $b\"\n\
            return \"OK\"\n\
        }\n";
    assert_eq!(
        run("nullable_ret", MAIN).expect("nullable generic return keeps null"),
        "OK"
    );
}

#[test]
fn vc_member_and_forward_through_lambda() {
    // A member call on `it` and passing `it` onward to a value-class-typed parameter.
    const MAIN: &str = "@JvmInline value class IC(val value: Int) {\n\
            fun twice(): Int = value * 2\n\
        }\n\
        fun take(ic: IC): Int = ic.value\n\
        fun <T, R> bar(v: T, f: (T) -> R): R = f(v)\n\
        fun box(): String {\n\
            val a = bar(IC(21)) { it.twice() }\n\
            if (a != 42) return \"FAIL member: $a\"\n\
            val b = bar(IC(7)) { take(it) }\n\
            if (b != 7) return \"FAIL forward: $b\"\n\
            return \"OK\"\n\
        }\n";
    assert_eq!(
        run("vc_member", MAIN).expect("vc member/forward generic lambda"),
        "OK"
    );
}

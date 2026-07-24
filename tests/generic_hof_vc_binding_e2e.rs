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
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    common::compile_and_run_box(main, "Main", &[sl, jdk.clone()], Some(&jdk))
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

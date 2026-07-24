//! An operator EXTENSION on a nullable PRIMITIVE receiver (`operator fun Int?.inc()`,
//! `operator fun Long?.compareTo(Long?)`) is dispatched by the CALL-SITE receiver's static
//! nullability: a `T?` receiver cannot use the builtin operator (it needs a non-null receiver), so
//! the extension wins; a non-null receiver keeps the builtin. Sound because `Nullable(prim)` keys
//! apart from the plain primitive in `Ty::erased_recv` (boxed wrapper class), so the two can never
//! collide. Nullable REFERENCE receivers (`String?.plus`) stay rejected — reference nullability is
//! not modeled at call sites. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn postfix_inc_on_nullable_int() {
    // corpus classes/kt723.kt shape: the body smart-casts `this` and uses the BUILTIN inc —
    // no self-recursion through the extension.
    const SRC: &str = "operator fun Int?.inc(): Int { if (this != null) return this.inc() else throw NullPointerException() }\n\
fun box(): String {\n\
    var i: Int? = 10\n\
    val j = i++\n\
    return if (j == 10 && 11 == i) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("Int?.inc extension"), "OK");
}

#[test]
fn postfix_inc_on_nullable_int_nullable_ret() {
    // corpus increment/postfixNullableIncrement.kt shape: the extension RETURNS `Int?`.
    const SRC: &str = "operator fun Int?.inc(): Int? = if (this == null) null else this + 1\n\
fun init(): Int? { return 10 }\n\
fun box(): String {\n\
    var i: Int? = init()\n\
    val j = i++\n\
    return if (j == 10 && 11 == i) \"OK\" else \"fail i = $i j = $j\"\n\
}\n";
    assert_eq!(run(SRC).expect("Int?.inc nullable-return extension"), "OK");
}

#[test]
fn arith_on_nullable_int_receiver() {
    // `n + 2` with `n: Int?` routes to the extension; `a + 2` with a non-null `a: Int` keeps the
    // builtin (also asserted separately below).
    const SRC: &str = "operator fun Int?.times(p: Int): Int = (this ?: 0) * p * 10\n\
fun nn(): Int? = 4\n\
fun box(): String {\n\
    val n: Int? = nn()\n\
    return if (n * 2 == 80) \"OK\" else \"fail: ${n * 2}\"\n\
}\n";
    assert_eq!(run(SRC).expect("Int?.times extension"), "OK");
}

#[test]
fn compare_to_on_nullable_long() {
    // corpus operatorConventions/compareTo/customCompareTo.kt shape (Long? slice): `<`/`>` on two
    // `Long?` values route through the extension compareTo; the `diff < 0L` INSIDE the body keeps
    // the builtin primitive comparison. (The corpus file also counts invocations via a top-level
    // `var invocations++` — statement inc/dec on a top-level property is a separate, pre-existing
    // gap, so this test asserts the comparison results only.)
    const SRC: &str = "private operator fun Long?.compareTo(other: Long?): Int {\n\
    val diff = (this ?: 0L) - (other ?: 0L)\n\
    return when {\n\
        diff < 0L -> -1\n\
        diff > 0L -> 1\n\
        else -> 0\n\
    }\n\
}\n\
fun box(): String {\n\
    val a: Long? = null\n\
    val b: Long? = 42L\n\
    if (a > b) return \"Fail >\"\n\
    if (a >= b) return \"Fail >=\"\n\
    if (!(a < b)) return \"Fail <\"\n\
    if (!(a <= b)) return \"Fail <=\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("Long?.compareTo extension"), "OK");
}

#[test]
fn non_null_receiver_keeps_builtin_operator() {
    // A NON-null receiver must keep the member/builtin operator even when a nullable-receiver
    // extension of the same name exists.
    const SRC: &str = "operator fun Int?.plus(p: Int): Int = 999\n\
fun nn(): Int? = 40\n\
fun box(): String {\n\
    val a: Int = 40\n\
    val n: Int? = nn()\n\
    return if (a + 2 == 42 && n + 2 == 999) \"OK\" else \"fail: ${a + 2} ${n + 2}\"\n\
}\n";
    assert_eq!(run(SRC).expect("non-null keeps builtin"), "OK");
}

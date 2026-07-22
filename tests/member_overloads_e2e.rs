//! Same-name member overloads with different erased signatures — kotlinc dispatches by argument
//! types at the call site (`ClassSig.methods` is a per-name overload list; `method_matching`
//! mirrors the top-level `pick_overload` scoring). Previously any class declaring such overloads
//! was wholesale rejected ("multiple overloads with different erased signatures").

use super::common;

fn run(src: &str) {
    let Some(got) = common::compile_and_run_with_stdlib(src, "MainKt") else {
        panic!("expected the box to compile and run");
    };
    assert_eq!(got, "OK");
}

/// Arity-distinguished overloads (the corpus `listIterator()`/`listIterator(Int)` shape).
#[test]
fn member_overloads_by_arity() {
    run(r#"
class C {
    fun f(): String = "zero"
    fun f(n: Int): String = "one:" + n
}

fun box(): String {
    val c = C()
    if (c.f() != "zero") return "FAIL0"
    if (c.f(7) != "one:7") return "FAIL1"
    return "OK"
}
"#);
}

/// Type-distinguished overloads at the same arity.
#[test]
fn member_overloads_by_param_type() {
    run(r#"
class C {
    fun g(n: Int): String = "int:" + n
    fun g(s: String): String = "str:" + s
}

fun box(): String {
    val c = C()
    if (c.g(1) != "int:1") return "FAIL0"
    if (c.g("x") != "str:x") return "FAIL1"
    return "OK"
}
"#);
}

/// Overloads called through an interface-typed receiver and from inside the class.
#[test]
fn member_overloads_internal_and_chained_calls() {
    run(r#"
class C {
    fun h(): Int = 10
    fun h(n: Int): Int = n * 2
    fun sum(): Int = h() + h(5)
}

fun box(): String = if (C().sum() == 20) "OK" else "FAIL"
"#);
}

/// Overloads split across the hierarchy: the subclass adds an arity, the base's original stays
/// callable (Kotlin resolves `d.f()` to `Base.f` even though `Derived` declares only `f(Int)`).
#[test]
fn member_overloads_across_hierarchy() {
    run(r#"
open class Base {
    fun f(): String = "base"
}

class Derived : Base() {
    fun f(n: Int): String = "derived:" + n
    fun both(): String = f() + "/" + f(1)
}

fun box(): String {
    val d = Derived()
    if (d.f() != "base") return "FAIL0"
    if (d.f(2) != "derived:2") return "FAIL1"
    if (d.both() != "base/derived:1") return "FAIL2"
    return "OK"
}
"#);
}

/// Exact erased duplicates stay rejected (JVM ClassFormatError otherwise).
#[test]
fn exact_erased_duplicate_still_rejected() {
    let src = r#"
class C {
    fun d(x: List<Int>): Int = 1
    fun d(x: List<String>): Int = 2
}

fun box(): String = "OK"
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "MainKt").is_none(),
        "same-erasure duplicate must be rejected, not emitted"
    );
}

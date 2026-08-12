//! Unqualified `companion object` member reads (`result` instead of `Outer.result` /
//! `Companion.result`) from the outer class, nested classes, init blocks, lambdas, and the
//! companion's own members.
//!
//! Companion properties are static fields on the outer class. The checker already traverses lexical
//! source owners in language-level precedence order, so a successful property selection records that
//! exact semantic owner for the generic static-field lowering path. This keeps nested classes and
//! closures independent of backend-generated class names, while preserving real-member shadowing.
//! A private property remains a conservative skip because direct cross-class field access needs an
//! access bridge that Krusty does not yet synthesize.

use super::common;

fn run_box(src: &str, stem: &str) {
    let out = common::expect_box_run_with_stdlib(src, stem);
    assert_eq!(out, "OK", "{stem}");
}

/// From an outer-class method.
#[test]
fn companion_read_from_outer_method() {
    run_box(
        r#"
class Outer {
    companion object {
        val result = "OK"
    }
    fun test() = result
}
fun box() = Outer().test()
"#,
        "CompOuterMethod",
    );
}

/// From the companion's own method (and init-adjacent contexts).
#[test]
fn companion_read_from_companion_method() {
    run_box(
        r#"
class Outer {
    companion object {
        val result = "OK"
        fun get() = result
    }
}
fun box() = Outer.get()
"#,
        "CompOwnMethod",
    );
}

#[test]
fn primitive_companion_classifier_and_value_are_one_symbol() {
    run_box(
        r#"
fun Double.Companion.answer(): String = "OK"

fun box(): String {
    val implicit = Double
    val explicit = Double.Companion
    if (implicit !== explicit) return "different companion values"
    return explicit.answer()
}
"#,
        "PrimitiveCompanionValue",
    );
}

#[test]
fn primitive_double_companion_extensions_and_constants() {
    run_box(
        r#"
fun <T> assertEquals(a: T, b: T) { if (a != b) throw AssertionError("$a != $b") }

fun Double.Companion.MAX() = MAX_VALUE
fun Double.Companion.MIN() = MIN_VALUE

fun <T> test(o: T) { assertEquals(o === Double.Companion, true) }

fun box(): String {
    assertEquals(1.7976931348623157E308, Double.MAX_VALUE)
    assertEquals(Double.MIN_VALUE, Double.MIN())
    assertEquals(Double.MAX_VALUE, Double.Companion.MAX())
    test(Double)
    test(Double.Companion)
    return "OK"
}
"#,
        "PrimitiveDoubleCompanionApi",
    );
}

#[test]
fn primitive_int_companion_extensions_and_constants() {
    run_box(
        r#"
fun Int.Companion.maximum() = MAX_VALUE
fun Int.Companion.minimum() = MIN_VALUE

fun box(): String {
    if (Int.maximum() != 2147483647) return "maximum"
    if (Int.minimum() != -2147483648) return "minimum"
    return "OK"
}
"#,
        "PrimitiveIntCompanionApi",
    );
}

/// From a nested class (the corpus shape: `private companion object` with a public member).
#[test]
fn companion_read_from_nested_class() {
    run_box(
        r#"
class Outer {
    private companion object {
        val result = "OK"
    }

    class Nested {
        fun foo() = result
    }

    fun test() = Nested().foo()
}

fun box() = Outer().test()
"#,
        "CompNested",
    );
}

/// The innermost lexical companion owns the selected field. This exercises semantic owner
/// selection directly: both declarations have the same property name, so a backend heuristic that
/// merely searches enclosing physical classes could silently read the wrong field.
#[test]
fn innermost_companion_property_wins() {
    run_box(
        r#"
class Outer {
    companion object { val result = "outer" }

    class Nested {
        companion object { val result = "OK" }
        fun read() = result
    }
}

fun box() = Outer.Nested().read()
"#,
        "CompInnermostOwner",
    );
}

/// From an `init` block and a lambda inside a nested class.
#[test]
fn companion_read_from_init_and_lambda() {
    run_box(
        r#"
class Outer {
    companion object {
        val result = "OK"
    }

    val test: String

    init {
        test = result
    }
}

fun box() = Outer().test
"#,
        "CompInit",
    );
    run_box(
        r#"
class Outer {
    companion object {
        val result = "OK"
    }

    class Nested {
        fun foo(): String {
            val r = Runnable { result }
            return r.get()
        }
    }

    fun test() = Nested().foo()
}

fun interface Runnable { fun get(): String }

fun box() = Outer().test()
"#,
        "CompLambdaNested",
    );
}

/// The nearest instance member wins over the enclosing companion member.
#[test]
fn instance_member_wins_over_companion_member() {
    let src = r#"
class Outer {
    val result = "member"

    companion object {
        val result = "companion"
    }

    fun test() = result
}

fun box() = Outer().test()
"#;
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "CompCollision").as_deref(),
        Some("member"),
    );
}

#[test]
fn kotlinc_member_companion_property_field_shape() {
    let src = r#"
class Outer {
    val result = "member"

    companion object {
        val result = "companion"
    }
}
"#;
    let Some(out) = common::compile_lib("CompCollisionReference", src) else {
        return;
    };
    let bytes = std::fs::read(out.join("Outer.class")).expect("read kotlinc Outer.class");
    let class = krusty::jvm::classreader::parse_class(&bytes).expect("parse kotlinc Outer.class");
    let instance = class
        .fields
        .iter()
        .find(|field| field.name == "result$1")
        .expect("kotlinc mangles the instance backing field");
    let companion = class
        .fields
        .iter()
        .find(|field| field.name == "result")
        .expect("kotlinc keeps the companion static's source name");
    assert_eq!(instance.access & 0x0008, 0, "instance field is not static");
    assert_ne!(companion.access & 0x0008, 0, "companion field is static");
}

#[test]
fn instance_function_wins_over_companion_function() {
    let src = r#"
class Outer {
    fun result() = "member"

    companion object {
        fun result() = "companion"
    }

    fun test() = result()
}

fun box() = Outer().test()
"#;
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "CompFunctionCollision").as_deref(),
        Some("member"),
    );
}

/// From a receiver-lambda / scope function spliced into a member (`x.run { result }`).
#[test]
fn companion_read_in_scope_lambda() {
    run_box(
        r#"
class Outer {
    companion object { val result = "OK" }
    fun test(): String {
        val x = "x"
        return x.run { result }
    }
}
fun box() = Outer().test()
"#,
        "CompScopeLambda",
    );
}

/// BOUNDARY: in an inner class, an OUTER member currently beats the inner class's own companion
/// member on a name collision (kotlinc's receiver priority puts the companion first) — pre-existing
/// ordering; pinned so a future fix promotes it.
#[test]
fn inner_companion_vs_outer_member_boundary() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let src = r#"
class Outer {
    val x = "outer"
    inner class Nested {
        companion object { val x = "companion" }
        fun foo() = x
    }
    fun test() = Nested().foo()
}
fun box() = Outer().test()
"#;
    let out = common::compile_and_run_with_stdlib(src, "CompPrecedence");
    assert_ne!(
        out.as_deref(),
        Some("companion"),
        "the outer member must never silently lose to the inner companion"
    );
}

#[test]
fn private_companion_property_is_visible_to_a_nested_class() {
    let src = r#"
class Outer {
    companion object {
        private val result = "OK"
    }

    class Nested {
        fun foo() = result
    }

    fun test() = Nested().foo()
}

fun box() = Outer().test()
"#;
    common::expect_box_ok_with_stdlib(src, "CompPrivateCross");
}

#[test]
fn nested_initializers_and_anonymous_objects_read_the_companion() {
    const SOURCE: &str = r#"
interface Text { fun read(): String }
class Outer {
    companion object { private val token = "OK" }
    val initialized = token
    class Nested {
        val initialized = token
        fun anonymous(): Text = object : Text { override fun read() = token }
    }
}
fun box(): String {
    val outer = Outer()
    val nested = Outer.Nested()
    return if (outer.initialized == "OK" && nested.initialized == "OK") nested.anonymous().read() else "FAIL"
}
"#;
    common::expect_box_ok_with_stdlib(SOURCE, "NestedCompanionReads");
}

#[test]
fn nested_class_calls_private_companion_methods_directly_and_in_a_lambda() {
    const SOURCE: &str = r#"
inline fun <T> eval(block: () -> T): T = block()
class Outer {
    companion object { private fun part() = "O" }
    class Nested {
        fun result(): String = part() + part() + eval { part() }
    }
}
fun box(): String = if (Outer.Nested().result() == "OOO") "OK" else "FAIL"
"#;
    common::expect_box_ok_with_stdlib(SOURCE, "NestedCompanionMethods");
}

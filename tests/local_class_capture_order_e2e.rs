//! A local class takes its captured values in kotlinc's first-use order, not in spelling order.
//!
//! kotlinc lists the values a closure reads itself, in source order, then the values of the
//! closures it includes. A class reads in its property initializers and `init` blocks, then
//! includes its primary constructor (defaults, then super-constructor arguments) and its members in
//! source order. Inside a body, a lambda is included where it is written, a local function where it
//! is called, and a local class where it is constructed. The constructor parameters follow that
//! order, and so do the arguments of every construction, which the enclosing file class holds.
use super::common;

fn assert_same_class(name: &str, source: &str, class: &str) {
    common::byte_diff_against_kotlinc_cp(name, source, class, &[common::stdlib_jar()])
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|why| panic!("{why}"));
}

/// The members read `d`, `b`, then `c`, `a`, so the constructor is `($d, $b, $c, $a, p)`.
#[test]
fn local_class_captures_follow_first_use_like_kotlinc() {
    let source = "\
fun capture(a: Int, b: Int, c: Int, d: Int): Int {
    class W(val p: Int) {
        fun run(): Int = d + b + p
        val g: Int get() = c + a
        fun more(): Int = a + d
    }
    val w = W(1)
    return w.run() + w.g + w.more()
}
";
    assert_same_class("CaptureOrder", source, "CaptureOrderKt$capture$W");
}

/// A member named like a captured value is not a use of it: `fun b() = b()` calls itself, so the
/// first use of `b` is in `first`, after `a`.
#[test]
fn a_same_named_recursive_member_is_not_a_capture_use() {
    let source = "\
fun recursive(a: Int, b: Int): Int {
    class R {
        fun b(): Int = b()
        fun first(): Int = a + b
    }
    return R().first()
}
";
    assert_same_class("RecursiveCapture", source, "RecursiveCaptureKt$recursive$R");
    assert_same_class("RecursiveCapture", source, "RecursiveCaptureKt");
}

const TIERS: &str = "\
open class Base(val x: Int)

// Construction code reads `d` then `c` before any member: (d, c, a, b).
fun construction(a: Int, b: Int, c: Int, d: Int): Int {
    class K {
        fun m(): Int = a + b + c + d
        val p = d
        init { println(c) }
    }
    return K().m() + K().p
}

// The primary constructor reads its default `c`, then its super argument `b`: (c, b, a, d).
fun primary(a: Int, b: Int, c: Int, d: Int): Int {
    class P(val y: Int = c) : Base(b) {
        fun m(): Int = a + d + c + b
    }
    val p = P(0)
    return p.m() + p.y + p.x
}

// Members in source order: getter, function, secondary constructor, function: (c, b, d, a).
fun members(a: Int, b: Int, c: Int, d: Int): Int {
    class M(val z: Int) {
        val g: Int get() = c
        fun f(): Int = b
        constructor() : this(1) { println(d) }
        fun h(): Int = a + d
    }
    val m = M()
    return m.g + m.f() + m.h() + m.z
}

// The member's own read of `a` comes first, though it is written last; then the lambda where it
// is written, the local function where it is called, and the local class where it is constructed:
// (a, d, c, b).
fun nested(a: Int, b: Int, c: Int, d: Int): Int {
    class N {
        fun m(): Int {
            val l = { d }
            fun local(): Int = c
            class Inner { fun i(): Int = b }
            return local() + Inner().i() + l() + a
        }
    }
    return N().m()
}
";

/// Every construction passes the captures in its class's layout, so the file class holding them
/// matches kotlinc's for each ordering tier.
#[test]
fn constructions_pass_captures_in_each_tiers_first_use_order() {
    assert_same_class("CaptureTiers", TIERS, "CaptureTiersKt");
}

/// A class included by construction takes its own captures in first-use order too.
#[test]
fn an_included_local_class_takes_its_own_capture_order() {
    assert_same_class("CaptureTiers", TIERS, "CaptureTiersKt$nested$N$m$Inner");
}

/// A class that calls a local function captures the values the function uses in the function's
/// first-use order, not spelling order: `helper` reads `b` before `a`, so `C` captures `($b, $a)`.
/// Both captures are `Int`, so this says nothing about the order of `helper`'s own lifted
/// parameters.
#[test]
fn a_class_calling_a_local_function_captures_in_its_first_use_order() {
    let source = "\
fun outer(a: Int, b: Int): Int {
    fun helper() = b + a
    class C {
        fun f() = helper()
    }
    return C().f() + helper()
}
";
    assert_same_class("FunctionCaptureOrder", source, "FunctionCaptureOrderKt");
    assert_same_class(
        "FunctionCaptureOrder",
        source,
        "FunctionCaptureOrderKt$outer$C",
    );
}

/// Captures of different types whose first use reverses their identity order: `run` reads `b`
/// (a `Long`) before `a` (a `String`), so `<init>` is `(JLjava/lang/String;I)V`, and the
/// constructor signature `@Metadata` records must spell the same permuted descriptor.
const MIXED_TYPES: &str = "\
fun mixed(a: String, b: Long): String {
    class Q(val p: Int) {
        fun run(): String = b.toString() + a + p
    }
    return Q(1).run()
}

// A value-class parameter makes the recorded constructor the public marker `<init>`, which spells
// the captures in the same permuted order.
fun marked(a: String, b: Long): String {
    class U(u: UInt) {
        val v = u.toInt()
        fun run(): String = b.toString() + a + v
    }
    return U(2u).run()
}
";

/// The emitted `<init>` and the `@Metadata` constructor signature agree on the permuted order of
/// differently typed captures.
#[test]
fn differently_typed_captures_record_their_physical_constructor_signature() {
    for class in ["MixedCapturesKt$mixed$Q", "MixedCapturesKt$marked$U"] {
        common::metadata_diff_against_kotlinc_cp(
            "MixedCaptures",
            MIXED_TYPES,
            class,
            &[common::stdlib_jar()],
        )
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|why| panic!("{why}"));
        assert_same_class("MixedCaptures", MIXED_TYPES, class);
    }
}

/// An implicit context argument is a use of the value it passes, at the call: `local` reads
/// `other` before it passes `registry` to `contextualValue()`, so a class that calls it captures
/// `($other, $registry)`; in `renderFirst`, which passes `registry` first, the class captures
/// `($registry, $other)`. A same-named nearer local does not stand in for the context value the
/// call passes.
#[test]
fn an_implicit_context_argument_is_a_first_use_at_its_call() {
    let source = "\
class Registry(val value: String)

context(registry: Registry)
fun contextualValue(): String = registry.value

context(registry: Registry)
fun render(other: String): String {
    fun local(): String = other + contextualValue()
    class C { fun f() = local() }
    return C().f()
}

context(registry: Registry)
fun renderFirst(other: String): String {
    fun local(): String = contextualValue() + other
    class C { fun f() = local() }
    return C().f()
}

// A nearer `val registry` shadows the context parameter's name but is not a `Registry`: the call
// still passes the context receiver, so `local` captures `(other, registry)` of type `Registry`.
context(registry: Registry)
fun renderShadowed(other: String): String {
    val registry = \"shadow\"
    fun local(): String = other + contextualValue()
    class C { fun f() = local() }
    return C().f() + registry
}
";
    assert_same_class("ContextCaptureOrder", source, "ContextCaptureOrderKt");
    assert_same_class(
        "ContextCaptureOrder",
        source,
        "ContextCaptureOrderKt$render$C",
    );
    assert_same_class(
        "ContextCaptureOrder",
        source,
        "ContextCaptureOrderKt$renderFirst$C",
    );
    assert_same_class(
        "ContextCaptureOrder",
        source,
        "ContextCaptureOrderKt$renderShadowed$C",
    );
}

/// Captures are told apart by binding, not by name: `local` reads the nearer `val registry` by name
/// and also passes the shadowed context parameter `registry` to `contextualValue()`. Those are two
/// bindings, so the class that calls `local` keeps two captures, `($registry, $registry$1, $other)`
/// in first-use order, as kotlinc does.
#[test]
fn a_shadowing_read_and_a_context_value_of_one_name_are_two_captures() {
    let source = "\
class Registry(val value: String)

context(registry: Registry)
fun contextualValue(): String = registry.value

context(registry: Registry)
fun renderBoth(other: String): String {
    val registry = \"shadow\"
    fun local(): String = registry + contextualValue() + other
    class C { fun f() = local() }
    return C().f()
}
";
    assert_same_class(
        "ContextShadowCapture",
        source,
        "ContextShadowCaptureKt$renderBoth$C",
    );
}

/// With the context value used first, kotlinc gives it the plain spelling and numbers the
/// shadowing local after it: `($registry: Registry, $registry$1: String, $other)`.
#[test]
fn a_context_value_used_first_takes_the_plain_capture_name() {
    let source = "\
class Registry(val value: String)

context(registry: Registry)
fun contextualValue(): String = registry.value

context(registry: Registry)
fun renderBoth(other: String): String {
    val registry = \"shadow\"
    fun local(): String = contextualValue() + registry + other
    class C { fun f() = local() }
    return C().f()
}
";
    assert_same_class(
        "ContextFirstShadowCapture",
        source,
        "ContextFirstShadowCaptureKt$renderBoth$C",
    );
}

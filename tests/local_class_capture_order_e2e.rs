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

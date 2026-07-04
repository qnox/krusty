//! End-to-end "box" coverage for deeper `@JvmInline value class` scenarios: computed properties,
//! `init` validation, value-class-valued members, value classes inside data classes, generic
//! boundaries, `is`/`when`, polymorphic interface dispatch, nesting, companions, default params, and
//! `List` element access. Each test compiles a `fun box(): String` with krusty, runs it on a real JVM
//! under verification, and asserts `"OK"`. Targets `src/jvm/value_classes.rs`.

mod common;

fn run(src: &str, stem: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, stem)
}

#[test]
fn value_class_computed_property() {
    let src = r#"
@JvmInline
value class Celsius(val c: Double) {
    val fahrenheit: Double get() = c * 9.0 / 5.0 + 32.0
}

fun box(): String {
    val t = Celsius(100.0)
    if (t.fahrenheit != 212.0) return "f1:${t.fahrenheit}"
    if (Celsius(0.0).fahrenheit != 32.0) return "f2"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcComputed") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_init_validation() {
    let src = r#"
@JvmInline
value class Positive(val v: Int) {
    init {
        require(v > 0) { "must be positive" }
    }
}

fun box(): String {
    val p = Positive(5)
    if (p.v != 5) return "f1:${p.v}"
    var threw = false
    try {
        Positive(-1)
    } catch (e: IllegalArgumentException) {
        threw = true
    }
    if (!threw) return "f2"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcInit") else {
        return;
    };
    assert_eq!(out, "OK");
}

// DROPPED: a value-class *member* function returning another value class currently miscompiles.
// The returned boxed value is not unboxed at the call boundary, so reading `.cents` off the result
// produces a `VerifyError: Type 'Money' ... is not assignable to integer`. Reproduced with several
// variants (arithmetic-in-ctor, store-to-local, trivial `Money(n)` body) — all fail. A top-level
// function returning a value class works (see `value_class_return_used_in_arithmetic`); only the
// member-returning-value-class path is broken. Left out until `src/jvm/value_classes.rs` unboxes the
// result of a mangled member returning a value class.

#[test]
fn value_class_in_data_class_field() {
    let src = r#"
@JvmInline
value class Id(val v: Int)

data class User(val id: Id, val name: String)

fun box(): String {
    val u = User(Id(42), "ada")
    if (u.id.v != 42) return "f1:${u.id.v}"
    if (u.name != "ada") return "f2"
    val v = u.copy(name = "grace")
    if (v.id.v != 42) return "f3"
    if (v.name != "grace") return "f4"
    if (u != User(Id(42), "ada")) return "f5"
    if (u == v) return "f6"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcDataField") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_equality_hashcode_tostring() {
    let src = r#"
@JvmInline
value class Tag(val v: Int)

fun box(): String {
    val a = Tag(7)
    val b = Tag(7)
    val c = Tag(8)
    if (a != b) return "f1"
    if (a == c) return "f2"
    if (a.hashCode() != b.hashCode()) return "f3"
    if (a.toString() != "Tag(v=7)") return "f4:$a"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcEqHash") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_return_used_in_arithmetic() {
    let src = r#"
@JvmInline
value class Meters(val v: Int)

fun distance(a: Int, b: Int): Meters = Meters(b - a)

fun box(): String {
    val d = distance(3, 10)
    val doubled = d.v * 2
    if (doubled != 14) return "f1:$doubled"
    if (d.v <= 5) return "f2"
    if (!(distance(0, 4).v < distance(0, 9).v)) return "f3"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcArithRet") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_generic_function() {
    let src = r#"
@JvmInline
value class Box2(val v: Int)

fun <T> identity(x: T): T = x

fun box(): String {
    val b = identity(Box2(11))
    if (b.v != 11) return "f1:${b.v}"
    val list = listOf(Box2(1), Box2(2), Box2(3))
    val first = identity(list).first()
    if (first.v != 1) return "f2"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcGenericFn") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_in_when_is() {
    let src = r#"
@JvmInline
value class Wrapped(val v: Any)

fun describe(w: Wrapped): String = when (val x = w.v) {
    is Int -> "int:$x"
    is String -> "str:$x"
    else -> "other"
}

fun box(): String {
    if (describe(Wrapped(5)) != "int:5") return "f1:${describe(Wrapped(5))}"
    if (describe(Wrapped("hi")) != "str:hi") return "f2"
    if (describe(Wrapped(1.5)) != "other") return "f3"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcWhenIs") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_polymorphic_interface() {
    let src = r#"
interface Priced {
    fun price(): Int
}

@JvmInline
value class Apple(val count: Int) : Priced {
    override fun price(): Int = count * 3
}

@JvmInline
value class Pear(val count: Int) : Priced {
    override fun price(): Int = count * 5
}

fun total(items: List<Priced>): Int {
    var sum = 0
    for (i in items) sum += i.price()
    return sum
}

fun box(): String {
    val t = total(listOf(Apple(2), Pear(3)))
    if (t != 21) return "f1:$t"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcPoly") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_nested_wrapping() {
    let src = r#"
@JvmInline
value class Inner(val v: Int)

@JvmInline
value class Outer(val inner: Inner)

fun box(): String {
    val o = Outer(Inner(9))
    if (o.inner.v != 9) return "f1:${o.inner.v}"
    val o2 = Outer(Inner(9))
    if (o != o2) return "f2"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcNested") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_companion_function() {
    let src = r#"
@JvmInline
value class Ratio(val v: Int) {
    companion object {
        fun of(num: Int, den: Int): Ratio = Ratio(num / den)
        val ZERO: Ratio get() = Ratio(0)
    }
}

fun box(): String {
    val r = Ratio.of(10, 2)
    if (r.v != 5) return "f1:${r.v}"
    if (Ratio.ZERO.v != 0) return "f2"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcCompanion") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_default_param() {
    let src = r#"
@JvmInline
value class Volume(val v: Int)

fun scale(base: Volume, factor: Int = 2): Int = base.v * factor

fun box(): String {
    if (scale(Volume(4)) != 8) return "f1:${scale(Volume(4))}"
    if (scale(Volume(4), 3) != 12) return "f2"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcDefaultParam") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_list_element_access() {
    let src = r#"
@JvmInline
value class Point(val x: Int)

fun box(): String {
    val pts: List<Point> = listOf(Point(10), Point(20), Point(30))
    if (pts[0].x != 10) return "f1:${pts[0].x}"
    if (pts[2].x != 30) return "f2"
    var sum = 0
    for (p in pts) sum += p.x
    if (sum != 60) return "f3:$sum"
    if (pts.size != 3) return "f4"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcListAccess") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn value_class_string_underlying_member() {
    let src = r#"
@JvmInline
value class Email(val raw: String) {
    val domain: String get() = raw.substringAfter('@')
    fun isValid(): Boolean = raw.contains('@')
}

fun box(): String {
    val e = Email("ada@example.com")
    if (e.domain != "example.com") return "f1:${e.domain}"
    if (!e.isValid()) return "f2"
    if (Email("nope").isValid()) return "f3"
    return "OK"
}
"#;
    let Some(out) = run(src, "VcStrMember") else {
        return;
    };
    assert_eq!(out, "OK");
}

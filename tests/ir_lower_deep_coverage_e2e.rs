//! Deep end-to-end codegen coverage for `src/ir_lower.rs` and `src/jvm/ir_emit.rs`, round-tripped on
//! a real JVM. Each test is a self-contained `fun box(): String` returning "OK", compiled with the
//! in-process krusty pipeline and run under the persistent JVM runner (`common::compile_and_run_box`).
//!
//! These deliberately exercise lowering/emit arms that the `feature_coverage_*` and `ir_edge_coverage`
//! suites leave under-covered: secondary constructors / init-block ordering, custom property
//! accessors, lateinit, inner/nested classes, open/override dispatch, interface default methods,
//! generics (bounded, multi-param, star-projection, variance), exception control flow (multi-catch,
//! try/finally with non-local exit, nested try, try-as-expression), casts & smart-casts, operator
//! overloads with runtime effect, augmented assignment on properties/array/map, destructuring,
//! nullable-receiver chains, `when` on complex subjects, sealed exhaustiveness, vararg forwarding,
//! labeled loops, and local functions with closures.
//!
//! Every test skips cleanly (returns) when the JDK / kotlin-stdlib toolchain is unavailable. Only
//! the kotlin-stdlib is required — no user classpath deps.

use std::path::PathBuf;

mod common;

/// (stdlib jar, jdk `lib/modules` jimage) for the `box()` harness, or `None` → skip.
fn env() -> Option<(PathBuf, PathBuf)> {
    let stdlib = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    Some((stdlib, jdk))
}

/// Compile `src` (a `fun box(): String`) and assert it round-trips to "OK" on the JVM.
fn run_box(src: &str, stem: &str) {
    let Some((stdlib, jdk)) = env() else {
        eprintln!("skipping {stem}: no JDK/stdlib toolchain");
        return;
    };
    assert_eq!(
        common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)).as_deref(),
        Some("OK"),
        "{stem}"
    );
}

// ---------------------------------------------------------------------------
// Class features: constructors, init blocks, accessors, lateinit, nesting
// ---------------------------------------------------------------------------

#[test]
fn secondary_ctors_delegating_this() {
    // Multiple secondary constructors, each delegating via this(...) to the primary.
    run_box(
        r#"
class Box(val a: Int, val b: Int) {
    constructor(a: Int) : this(a, 0)
    constructor() : this(1, 0)
    val sum: Int get() = a + b
}
fun box(): String {
    val x = Box(3, 4)
    val y = Box(9)
    val z = Box()
    return if (x.sum == 7 && y.sum == 9 && z.sum == 1) "OK" else "f:${x.sum},${y.sum},${z.sum}"
}
"#,
        "SecCtorThis",
    );
}

#[test]
fn secondary_ctor_super_call() {
    // Secondary constructor delegating to super(...).
    run_box(
        r#"
open class Base(val tag: String)
class Sub : Base {
    var n: Int = 0
    constructor(n: Int) : super("s") { this.n = n }
}
fun box(): String {
    val s = Sub(5)
    return if (s.tag == "s" && s.n == 5) "OK" else "f:${s.tag},${s.n}"
}
"#,
        "SecCtorSuper",
    );
}

#[test]
fn init_block_ordering() {
    // Multiple init blocks run in source order, interleaved with property initializers.
    run_box(
        r#"
class Order {
    val log = StringBuilder()
    init { log.append("a") }
    val mid = run { log.append("b"); 1 }
    init { log.append("c") }
    fun result(): String = log.toString()
}
fun box(): String {
    val s = Order().result()
    return if (s == "abc") "OK" else "f:$s"
}
"#,
        "InitOrder",
    );
}

#[test]
fn custom_getter_setter_backing_field() {
    // Custom getter+setter with a backing field accessed via `field`.
    run_box(
        r#"
class Celsius {
    var value: Int = 0
        get() = field
        set(v) { field = if (v < -273) -273 else v }
}
fun box(): String {
    val c = Celsius()
    c.value = -500
    val a = c.value
    c.value = 20
    val b = c.value
    return if (a == -273 && b == 20) "OK" else "f:$a,$b"
}
"#,
        "CustomAccessor",
    );
}

#[test]
fn computed_property_no_backing_field() {
    // Getter computed from other state; no backing field allocated.
    run_box(
        r#"
class Rect(val w: Int, val h: Int) {
    val area: Int get() = w * h
    val isSquare: Boolean get() = w == h
}
fun box(): String {
    val r = Rect(3, 3)
    return if (r.area == 9 && r.isSquare) "OK" else "f:${r.area}"
}
"#,
        "ComputedProp",
    );
}

#[test]
fn lateinit_property() {
    // lateinit var: assigned after construction, then read back (backing field + null-guard getter).
    run_box(
        r#"
class Holder {
    lateinit var name: String
    fun read(): String = name
}
fun box(): String {
    val h = Holder()
    h.name = "x"
    val a = h.name
    val b = h.read()
    return if (a == "x" && b == "x") "OK" else "f:$a,$b"
}
"#,
        "Lateinit",
    );
}

#[test]
fn companion_const_and_fun() {
    // Companion object with a const val and a function, accessed via the enclosing class name.
    run_box(
        r#"
class Counter {
    companion object {
        const val MAX = 10
        fun triple(x: Int): Int = x * 3
    }
}
fun box(): String {
    val a = Counter.MAX
    val b = Counter.triple(4)
    return if (a == 10 && b == 12) "OK" else "f:$a,$b"
}
"#,
        "CompanionConst",
    );
}

#[test]
fn nested_class_instantiation() {
    // Static nested class instantiated by qualified name.
    run_box(
        r#"
class Outer {
    class Nested(val v: Int) {
        fun doubled(): Int = v * 2
    }
}
fun box(): String {
    val n = Outer.Nested(21)
    return if (n.doubled() == 42) "OK" else "f:${n.doubled()}"
}
"#,
        "NestedClass",
    );
}

#[test]
fn object_expression_capturing() {
    // Anonymous object (object expression) implementing an interface and capturing enclosing state.
    run_box(
        r#"
interface Supplier { fun get(): Int }
fun makeSupplier(base: Int): Supplier = object : Supplier {
    override fun get(): Int = base + 1
}
fun box(): String {
    val s = makeSupplier(41)
    return if (s.get() == 42) "OK" else "f:${s.get()}"
}
"#,
        "ObjectExpr",
    );
}

#[test]
fn abstract_class_override() {
    // Abstract method overridden in a concrete subclass, called via the abstract type.
    run_box(
        r#"
abstract class Shape {
    abstract fun area(): Int
    fun describe(): String = "area=" + area()
}
class Square(val s: Int) : Shape() {
    override fun area(): Int = s * s
}
fun box(): String {
    val sh: Shape = Square(4)
    return if (sh.area() == 16 && sh.describe() == "area=16") "OK" else "f:${sh.describe()}"
}
"#,
        "AbstractOverride",
    );
}

#[test]
fn open_override_virtual_dispatch() {
    // Virtual dispatch: overridden method selected by runtime type through a base reference.
    run_box(
        r#"
open class A { open fun name(): String = "A" }
open class B : A() { override fun name(): String = "B" }
class C : B() { override fun name(): String = "C" }
fun pick(a: A): String = a.name()
fun box(): String {
    val r = pick(A()) + pick(B()) + pick(C())
    return if (r == "ABC") "OK" else "f:$r"
}
"#,
        "VirtualDispatch",
    );
}

#[test]
fn interface_default_method() {
    // Interface with a default method invoked through an implementer.
    run_box(
        r#"
interface Greeter {
    fun name(): String
    fun greet(): String = "hi " + name()
}
class Bob : Greeter {
    override fun name(): String = "bob"
}
fun box(): String {
    val g: Greeter = Bob()
    return if (g.greet() == "hi bob") "OK" else "f:${g.greet()}"
}
"#,
        "IfaceDefault",
    );
}

// ---------------------------------------------------------------------------
// Generics
// ---------------------------------------------------------------------------

#[test]
fn generic_class_using_type_param() {
    // Generic class stores and returns a T.
    run_box(
        r#"
class Cell<T>(private var value: T) {
    fun get(): T = value
    fun set(v: T) { value = v }
}
fun box(): String {
    val c = Cell(3)
    c.set(7)
    val s = Cell("hi")
    return if (c.get() == 7 && s.get() == "hi") "OK" else "f:${c.get()}"
}
"#,
        "GenericClass",
    );
}

#[test]
fn generic_fn_multiple_type_params() {
    // Generic function with two independent type parameters.
    run_box(
        r#"
fun <A, B> pairUp(a: A, b: B): String = "$a:$b"
fun box(): String {
    val r = pairUp(1, "x") + "|" + pairUp(true, 9)
    return if (r == "1:x|true:9") "OK" else "f:$r"
}
"#,
        "GenericMultiParam",
    );
}

#[test]
fn bounded_generic_number() {
    // Bounded type parameter <T : Number> whose bound method (toInt) is called in the body.
    run_box(
        r#"
fun <T : Number> sumAsInt(xs: List<T>): Int {
    var s = 0
    for (n in xs) s += n.toInt()
    return s
}
fun box(): String {
    val a = sumAsInt(listOf(1, 2, 3))
    val b = sumAsInt(listOf(1.5, 2.5))
    return if (a == 6 && b == 3) "OK" else "f:$a,$b"
}
"#,
        "BoundedGeneric",
    );
}

#[test]
fn generic_factory_returning_t() {
    // Generic factory function whose return is inferred from an argument.
    run_box(
        r#"
fun <T> firstNonNull(a: T?, b: T): T = a ?: b
fun box(): String {
    val r1 = firstNonNull<Int>(null, 5)
    val r2 = firstNonNull("x", "y")
    return if (r1 == 5 && r2 == "x") "OK" else "f:$r1,$r2"
}
"#,
        "GenericFactory",
    );
}

#[test]
fn star_projection_use() {
    // Star projection read from a heterogeneous list.
    run_box(
        r#"
fun sizeOf(c: Collection<*>): Int = c.size
fun box(): String {
    val l: List<*> = listOf(1, 2, 3)
    return if (sizeOf(l) == 3) "OK" else "f:${sizeOf(l)}"
}
"#,
        "StarProjection",
    );
}

#[test]
fn use_site_variance_out() {
    // Use-site variance (out projection) covariant read.
    run_box(
        r#"
fun sum(nums: List<out Number>): Int {
    var t = 0
    for (n in nums) t += n.toInt()
    return t
}
fun box(): String {
    val ints: List<Int> = listOf(1, 2, 3)
    return if (sum(ints) == 6) "OK" else "f:${sum(ints)}"
}
"#,
        "VarianceOut",
    );
}

#[test]
fn nested_generic_instantiation() {
    // Nested generic type instantiated and used at runtime.
    run_box(
        r#"
fun box(): String {
    val m: MutableMap<String, MutableList<Int>> = HashMap()
    m.getOrPut("a") { ArrayList() }.add(1)
    m.getOrPut("a") { ArrayList() }.add(2)
    val a = m["a"]!!
    return if (a.size == 2 && a[0] == 1 && a[1] == 2) "OK" else "f:$a"
}
"#,
        "NestedGeneric",
    );
}

// ---------------------------------------------------------------------------
// Exceptions & control flow
// ---------------------------------------------------------------------------

#[test]
fn multi_catch_order() {
    // Multiple catch clauses; the most specific matching clause is chosen (order matters).
    run_box(
        r#"
fun classify(n: Int): String {
    try {
        when (n) {
            0 -> throw IllegalArgumentException("iae")
            1 -> throw IllegalStateException("ise")
            else -> throw RuntimeException("rte")
        }
    } catch (e: IllegalArgumentException) {
        return "iae"
    } catch (e: IllegalStateException) {
        return "ise"
    } catch (e: RuntimeException) {
        return "rte"
    }
}
fun box(): String {
    val r = classify(0) + classify(1) + classify(2)
    return if (r == "iaeiserte") "OK" else "f:$r"
}
"#,
        "MultiCatch",
    );
}

#[test]
fn finally_runs_on_return() {
    // finally executes even when try returns; observe the side effect.
    run_box(
        r#"
class Ref { var v = 0 }
fun compute(r: Ref): Int {
    try {
        return 1
    } finally {
        r.v = 42
    }
}
fun box(): String {
    val r = Ref()
    val res = compute(r)
    return if (res == 1 && r.v == 42) "OK" else "f:$res,${r.v}"
}
"#,
        "FinallyReturn",
    );
}

#[test]
fn finally_runs_during_unwind() {
    // finally executes while an exception propagates through it (no local catch), then caught outside.
    run_box(
        r#"
class Ref { var v = 0 }
fun risky(r: Ref) {
    try {
        throw IllegalStateException("boom")
    } finally {
        r.v = 9
    }
}
fun box(): String {
    val r = Ref()
    var caught = false
    try {
        risky(r)
    } catch (e: IllegalStateException) {
        caught = true
    }
    return if (caught && r.v == 9) "OK" else "f:$caught,${r.v}"
}
"#,
        "FinallyUnwind",
    );
}

#[test]
fn nested_try_catch() {
    // Inner try rethrows; outer catch handles it.
    run_box(
        r#"
fun box(): String {
    var stage = ""
    try {
        try {
            throw IllegalStateException("inner")
        } catch (e: IllegalStateException) {
            stage += "i"
            throw RuntimeException("wrapped")
        } finally {
            stage += "f"
        }
    } catch (e: RuntimeException) {
        stage += "o"
    }
    return if (stage == "ifo") "OK" else "f:$stage"
}
"#,
        "NestedTry",
    );
}

#[test]
fn catch_broad_then_is_dispatch() {
    // Catch a broad `Exception`, then dispatch on the caught value's runtime type with `is`.
    run_box(
        r#"
fun handle(n: Int): String = try {
    when (n) {
        0 -> throw IllegalArgumentException("a")
        1 -> throw IllegalStateException("b")
        else -> throw RuntimeException("c")
    }
} catch (e: Exception) {
    when (e) {
        is IllegalArgumentException -> "iae"
        is IllegalStateException -> "ise"
        else -> "rte"
    }
}
fun box(): String {
    val r = handle(0) + handle(1) + handle(2)
    return if (r == "iaeiserte") "OK" else "f:$r"
}
"#,
        "CatchBroadIs",
    );
}

#[test]
fn try_as_expression_value() {
    // try/catch used as an expression producing a value.
    run_box(
        r#"
fun parse(s: String): Int = try { s.toInt() } catch (e: NumberFormatException) { -1 }
fun box(): String {
    val a = parse("123")
    val b = parse("nope")
    return if (a == 123 && b == -1) "OK" else "f:$a,$b"
}
"#,
        "TryExpr",
    );
}

// ---------------------------------------------------------------------------
// Casts & type operations
// ---------------------------------------------------------------------------

#[test]
fn is_and_smart_cast() {
    // `is` check followed by a smart cast in the true branch.
    run_box(
        r#"
fun describe(x: Any): String {
    if (x is String) return "str:" + x.length
    if (x is Int) return "int:" + (x + 1)
    return "other"
}
fun box(): String {
    val r = describe("abc") + "|" + describe(41) + "|" + describe(1.5)
    return if (r == "str:3|int:42|other") "OK" else "f:$r"
}
"#,
        "IsSmartCast",
    );
}

#[test]
fn as_and_safe_cast() {
    // `as` (checked) and `as?` (safe) cast; safe cast yields null on mismatch.
    run_box(
        r#"
fun box(): String {
    val a: Any = "hello"
    val s = a as String
    val n = a as? Int
    val bad = (a as? CharSequence)?.length ?: -1
    return if (s == "hello" && n == null && bad == 5) "OK" else "f:$s,$n,$bad"
}
"#,
        "AsSafeCast",
    );
}

#[test]
fn not_null_smart_cast() {
    // Smart cast after a `!= null` guard.
    run_box(
        r#"
fun lengthOr(s: String?): Int {
    if (s != null) {
        return s.length
    }
    return -1
}
fun box(): String {
    val a = lengthOr("abcd")
    val b = lengthOr(null)
    return if (a == 4 && b == -1) "OK" else "f:$a,$b"
}
"#,
        "NotNullSmartCast",
    );
}

#[test]
fn cast_to_interface_and_hierarchy() {
    // is/!is over a class hierarchy plus a cast to an interface.
    run_box(
        r#"
interface Animal { fun sound(): String }
open class Dog : Animal { override fun sound(): String = "woof" }
class Puppy : Dog()
fun box(): String {
    val p: Any = Puppy()
    val ok1 = p is Dog
    val ok2 = p !is String
    val a = p as Animal
    return if (ok1 && ok2 && a.sound() == "woof") "OK" else "f:$ok1,$ok2"
}
"#,
        "CastInterface",
    );
}

// ---------------------------------------------------------------------------
// Operators & property access
// ---------------------------------------------------------------------------

#[test]
fn augmented_assign_property_and_array() {
    // Compound assignment on a property, an array element, and increment/decrement.
    run_box(
        r#"
class Acc { var total = 0 }
fun box(): String {
    val a = Acc()
    a.total += 5
    a.total *= 3
    a.total--
    val arr = intArrayOf(1, 2, 3)
    arr[1] += 10
    arr[2]++
    return if (a.total == 14 && arr[1] == 12 && arr[2] == 4) "OK" else "f:${a.total},${arr[1]},${arr[2]}"
}
"#,
        "AugAssign",
    );
}

#[test]
fn map_index_get_set() {
    // Map index get/set operators (`m[k]` / `m[k] = v`) with a read-modify-write via getValue/`!!`.
    run_box(
        r#"
fun box(): String {
    val m = HashMap<String, Int>()
    m["x"] = 1
    m["x"] = m["x"]!! + 4
    m["x"] = m.getValue("x") * 2
    m["y"] = 100
    return if (m["x"] == 10 && m["y"] == 100 && m.size == 2) "OK" else "f:${m["x"]},${m["y"]}"
}
"#,
        "MapIndexGetSet",
    );
}

#[test]
fn operator_overloads_arithmetic_compare() {
    // plus/minus/times + compareTo operator overloads producing runtime effects.
    run_box(
        r#"
data class V2(val x: Int, val y: Int) : Comparable<V2> {
    operator fun plus(o: V2) = V2(x + o.x, y + o.y)
    operator fun minus(o: V2) = V2(x - o.x, y - o.y)
    operator fun times(k: Int) = V2(x * k, y * k)
    override fun compareTo(other: V2): Int = (x * x + y * y) - (other.x * other.x + other.y * other.y)
}
fun box(): String {
    val a = V2(1, 2) + V2(3, 4)
    val b = V2(10, 10) - V2(1, 2)
    val c = V2(2, 3) * 2
    val cmp = V2(3, 4) > V2(1, 1)
    return if (a == V2(4, 6) && b == V2(9, 8) && c == V2(4, 6) && cmp) "OK" else "f:$a,$b,$c,$cmp"
}
"#,
        "OpArith",
    );
}

#[test]
fn operator_get_set_invoke_contains() {
    // get/set/invoke/contains operator overloads.
    run_box(
        r#"
class Slots(n: Int) {
    private val data: IntArray = IntArray(n)
    operator fun get(i: Int): Int = data[i]
    operator fun set(i: Int, v: Int) { data[i] = v }
    operator fun contains(v: Int): Boolean = data.any { it == v }
}
class Adder(val base: Int) {
    operator fun invoke(x: Int): Int = base + x
}
fun box(): String {
    val g = Slots(3)
    g[1] = 9
    val add = Adder(100)
    val ok = g[1] == 9 && (9 in g) && (7 !in g) && add(5) == 105
    return if (ok) "OK" else "f"
}
"#,
        "OpGetSetInvoke",
    );
}

#[test]
fn operator_div_rem() {
    // div / rem operator overloads producing runtime effects.
    run_box(
        r#"
data class Money(val cents: Int) {
    operator fun div(k: Int): Money = Money(cents / k)
    operator fun rem(k: Int): Money = Money(cents % k)
}
fun box(): String {
    val b = Money(30) / 4
    val c = Money(30) % 4
    return if (b == Money(7) && c == Money(2)) "OK" else "f:$b,$c"
}
"#,
        "OpDivRem",
    );
}

#[test]
fn infix_function() {
    // User-defined infix function.
    run_box(
        r#"
infix fun Int.pow(e: Int): Int {
    var r = 1
    repeat(e) { r *= this }
    return r
}
fun box(): String {
    val r = 2 pow 10
    return if (r == 1024) "OK" else "f:$r"
}
"#,
        "InfixFn",
    );
}

#[test]
fn destructuring_data_pair_map() {
    // Destructuring from a data class, a Pair, and a Map.Entry in a for-loop.
    run_box(
        r#"
data class Point(val x: Int, val y: Int)
fun box(): String {
    val (px, py) = Point(3, 4)
    val (a, b) = 1 to "one"
    val m = linkedMapOf("k1" to 1, "k2" to 2)
    var acc = 0
    val keys = StringBuilder()
    for ((k, v) in m) {
        keys.append(k)
        acc += v
    }
    return if (px == 3 && py == 4 && a == 1 && b == "one" && acc == 3 && keys.toString() == "k1k2") "OK"
        else "f:$px,$py,$a,$b,$acc,$keys"
}
"#,
        "Destructuring",
    );
}

// ---------------------------------------------------------------------------
// Misc: nullable chains, when, sealed, varargs, local functions, labels
// ---------------------------------------------------------------------------

#[test]
fn nullable_receiver_chain() {
    // Safe-call chain a?.b?.c with elvis fallback.
    run_box(
        r#"
class C(val next: C?, val v: Int)
fun box(): String {
    val chain = C(C(C(null, 3), 2), 1)
    val deep = chain.next?.next?.v ?: -1
    val missing = chain.next?.next?.next?.v ?: -1
    return if (deep == 3 && missing == -1) "OK" else "f:$deep,$missing"
}
"#,
        "NullableChain",
    );
}

#[test]
fn elvis_with_side_effect() {
    // Elvis whose right side runs a side-effecting fallback only when needed.
    run_box(
        r#"
class Log { var n = 0; fun next(): Int { n++; return -n } }
fun box(): String {
    val log = Log()
    val a: Int? = 5
    val r1 = a ?: log.next()
    val b: Int? = null
    val r2 = b ?: log.next()
    return if (r1 == 5 && r2 == -1 && log.n == 1) "OK" else "f:$r1,$r2,${log.n}"
}
"#,
        "ElvisSideEffect",
    );
}

#[test]
fn when_type_subjects() {
    // when over an Any subject with several `is` arms (smart-cast in each) plus an else.
    run_box(
        r#"
fun classify(x: Any): String = when (x) {
    is String -> "s" + x.length
    is Boolean -> if (x) "t" else "f"
    is Int -> "i" + (x + 1)
    else -> "other"
}
fun box(): String {
    val r = classify("hi") + classify(true) + classify(41) + classify(1.5)
    return if (r == "s2ti42other") "OK" else "f:$r"
}
"#,
        "WhenType",
    );
}

#[test]
fn when_string_and_enum() {
    // String when (hashcode+equals switch) and an enum when.
    run_box(
        r#"
enum class Color { RED, GREEN, BLUE }
fun rgb(c: Color): Int = when (c) {
    Color.RED -> 1
    Color.GREEN -> 2
    Color.BLUE -> 3
}
fun word(s: String): Int = when (s) {
    "one" -> 1
    "two" -> 2
    else -> 0
}
fun box(): String {
    val e = rgb(Color.RED) + rgb(Color.GREEN) + rgb(Color.BLUE)
    val s = word("one") + word("two") + word("x")
    return if (e == 6 && s == 3) "OK" else "f:$e,$s"
}
"#,
        "WhenStrEnum",
    );
}

#[test]
fn sealed_exhaustive_when() {
    // Sealed hierarchy with an exhaustive when (no else needed).
    run_box(
        r#"
sealed class Expr
class Num(val v: Int) : Expr()
class Add(val l: Expr, val r: Expr) : Expr()
class Neg(val e: Expr) : Expr()
fun eval(e: Expr): Int = when (e) {
    is Num -> e.v
    is Add -> eval(e.l) + eval(e.r)
    is Neg -> -eval(e.e)
}
fun box(): String {
    val tree = Add(Num(3), Neg(Num(1)))
    return if (eval(tree) == 2) "OK" else "f:${eval(tree)}"
}
"#,
        "SealedWhen",
    );
}

#[test]
fn vararg_forwarding_and_named_default() {
    // Vararg forwarding via a single spread, plus named + default argument resolution at the call.
    run_box(
        r#"
fun sumAll(vararg xs: Int): Int {
    var t = 0
    for (x in xs) t += x
    return t
}
fun forward(vararg xs: Int): Int = sumAll(*xs)
fun config(a: Int = 1, b: Int = 2, c: Int = 3): Int = a * 100 + b * 10 + c
fun box(): String {
    val f = forward(1, 2, 3)
    val direct = sumAll(4, 5)
    val c1 = config(b = 5)
    val c2 = config(9)
    return if (f == 6 && direct == 9 && c1 == 153 && c2 == 923) "OK" else "f:$f,$direct,$c1,$c2"
}
"#,
        "VarargForward",
    );
}

#[test]
fn local_function_closure() {
    // Local function capturing and mutating an enclosing variable.
    run_box(
        r#"
fun box(): String {
    var count = 0
    fun bump(by: Int): Int {
        count += by
        return count
    }
    val a = bump(3)
    val b = bump(4)
    return if (a == 3 && b == 7 && count == 7) "OK" else "f:$a,$b,$count"
}
"#,
        "LocalClosure",
    );
}

#[test]
fn labeled_break_continue_outer() {
    // Labeled break/continue targeting an outer loop.
    run_box(
        r#"
fun box(): String {
    val sb = StringBuilder()
    outer@ for (i in 0..3) {
        for (j in 0..3) {
            if (j == 2) continue@outer
            if (i == 3) break@outer
            sb.append("$i$j ")
        }
    }
    val s = sb.toString().trim()
    return if (s == "00 01 10 11 20 21") "OK" else "f:$s"
}
"#,
        "LabeledLoop",
    );
}

//! Consolidated feature `box()` snippets, compiled by krusty and run on a real JVM. To keep the test
//! suite fast, every accepted snippet runs in ONE JVM via a reflective runner (per-snippet
//! `URLClassLoader`), instead of a `javac`+`java` per snippet — the same trick as `box_vendored_e2e`.
//! Each snippet's `box(): String` must return "OK" under `-Xverify:all`.

use std::fs;
use std::process::Command;

use super::common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn lambda_shadowed_outer_var_does_not_allocate_ref_cell() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(java_home) = common::java_home() else {
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
fun eval(f: () -> Int): Int = f()
fun box(): String {
    var x = 1
    val r = eval {
        var x = 10
        x++
        x
    }
    x = 2
    if (r != 11) return "r=$r"
    return if (x == 2) "OK" else "x=$x"
}
"#;
    let Some(classes) =
        common::compile_in_process(src, "ShadowCell", std::slice::from_ref(&stdlib), Some(&jdk))
    else {
        return;
    };
    let box_class = common::find_box_class(&classes).expect("box class");
    assert_eq!(
        common::run_box(&classes, &box_class, &[stdlib]).as_deref(),
        Some("OK")
    );
    assert!(
        classes
            .iter()
            .all(|(_, bytes)| !bytes_contain(bytes, b"kotlin/jvm/internal/Ref$IntRef")),
        "shadowed lambda-local x must not force the outer x into a Ref cell"
    );
}

/// `(class-stem, source)` — the file is written as `<stem>.kt`, whose facade class is `<stem>Kt`.
const SNIPPETS: &[(&str, &str)] = &[
    // `joinToString` with a TRAILING LAMBDA: the lambda fills the LAST parameter (`transform`), the five
    // middle parameters defaulting (`separator`/`prefix`/… via the `$default` synthetic + bit-mask). The
    // lambda's `it` binds to the receiver's element type. Covers both the lambda-only form and a leading
    // explicit arg + trailing lambda.
    (
        "JoinToStringTrailingLambda",
        r#"// WITH_STDLIB
fun box(): String {
    val xs: List<String> = listOf("a", "bb", "ccc")
    val s1 = xs.joinToString { it.length.toString() }
    if (s1 != "1, 2, 3") return "s1=$s1"
    val s2 = xs.joinToString("-") { it.uppercase() }
    if (s2 != "A-BB-CCC") return "s2=$s2"
    return "OK"
}
"#,
    ),
    // Kotlin BUILTIN members read as plain VALUES (not only fused/safe-call forms): `String.length`
    // (a property over `java.lang.String.length()`) and `List.size`/indexing (`java.util.List`). These
    // resolve generically from the builtins metadata + the kotlin↔JVM class map — no per-member hardcode.
    (
        "BuiltinMemberValues",
        r#"// WITH_STDLIB
fun box(): String {
    val n = "hello".length
    val l = listOf("a", "b", "c")
    if (n != 5) return "len=$n"
    if (l.size != 3) return "size=${l.size}"
    if (l[1] != "b") return "idx=${l[1]}"
    return "OK"
}
"#,
    ),
    // A `Unit`-valued local: `val u = f()` where `f(): Unit`. kotlinc runs the initializer for effect
    // then binds the `kotlin.Unit` singleton — `u` is a `kotlin/Unit` reference, so `u.toString()` and
    // string interpolation yield "kotlin.Unit".
    (
        "UnitAsValue",
        r#"
fun myPrintln(a: Any): Unit {}
fun box(): String {
    val u = myPrintln("First")
    if (u.toString() != "kotlin.Unit") return "f0: $u"
    if ("$u" != "kotlin.Unit") return "f1: $u"
    return "OK"
}
"#,
    ),
    // Callable values use Kotlin's `invoke` operator convention. Direct calls and the member spelling
    // both resolve to the same semantic callable invocation before JVM lowering.
    (
        "FunctionValueInvokeOperator",
        r#"
fun make(): (String) -> String = { s -> s + "K" }
fun box(): String {
    val suffix: (String) -> String = { s -> "O" + s }
    if (suffix("K") != "OK") return "f0"
    if (suffix.invoke("K") != "OK") return "f1"
    if (make().invoke("O") != "OK") return "f2"
    val maybe: (() -> String)? = { "OK" }
    return maybe!!.invoke()
}
"#,
    ),
    // Dynamic `FunctionN.invoke` is an erased Object boundary. Value-class arguments must box before the
    // invoke and the function-reference adapter unboxes for the physical target.
    (
        "FunctionValueInvokeValueClass",
        r#"
value class Value(val value: String)
fun take(value: Value): String = value.value
fun box(): String {
    val f: (Value) -> String = ::take
    if (f(Value("OK")) != "OK") return "f0"
    if (f.invoke(Value("OK")) != "OK") return "f1"
    return "OK"
}
"#,
    ),
    // Kotlin `a(args)` is an `invoke` operator call, not a special case limited to function types.
    (
        "ObjectInvokeOperator",
        r#"
class Joiner(val prefix: String) {
    operator fun invoke(suffix: String): String = prefix + suffix
}
fun box(): String {
    val join = Joiner("O")
    if (join("K") != "OK") return "direct"
    if (join.invoke("K") != "OK") return "member"
    return "OK"
}
"#,
    ),
    // Generic stdlib extensions (`T.let`) must select using receiver-bound logical parameters from
    // metadata, not the erased `Function1` descriptor. A callable reference argument typed as
    // `(Value) -> Unit` then fits `block: (T) -> R` when `T` is the value-class receiver.
    (
        "ValueClassLetCallableRef",
        r#"// WITH_STDLIB
value class Value(val value: String)
object Sink {
    var result: String = "FAIL"
    fun take(value: Value) { result = value.value }
}
fun box(): String {
    Value("OK").let(Sink::take)
    return Sink.result
}
"#,
    ),
    // A valueless `return@lambda` in a `() -> Unit` lambda: the closure method's JVM `invoke` returns
    // the `kotlin/Unit` SINGLETON (a reference, since the generic `R` erases to `Object`), so the local
    // return must `areturn Unit.INSTANCE`, not a `void` `return` (which the verifier rejects). The
    // conditional keeps the continuation reachable (`seen = c` runs only when the guard is false).
    (
        "LabeledUnitReturnInLambda",
        r#"
fun <R> myRun(block: () -> R): R = block()
fun test(c: Int): Int {
    var seen = 0
    myRun { if (c > 0) { return@myRun }; seen = c }
    return seen
}
fun box(): String {
    if (test(5) != 0) return "f1: ${test(5)}"
    if (test(-3) != -3) return "f2: ${test(-3)}"
    return "OK"
}
"#,
    ),
    // A trivial elvis kotlinc folds: a non-null (primitive) lhs makes `x ?: d` == `x` (rhs is dead),
    // and a statically-`null` lhs makes `null ?: d` == `d`. krusty must emit the folded side, keeping
    // the live operand's side effects.
    (
        "ElvisTrivial",
        r#"
fun box(): String {
    if ((42 ?: 239) != 42) return "f0"
    if ((42.toLong() ?: 239.toLong()) != 42L) return "f1"
    val s: String = null ?: null ?: "OK"
    if (s != "OK") return "f2: $s"
    var n = 0
    fun side(): Int { n++; return 7 }
    if ((side() ?: 0) != 7 || n != 1) return "f3: $n"
    return "OK"
}
"#,
    ),
    // Arithmetic operator members called by their METHOD name on a primitive (`a.plus(b)` ≡ `a + b`) —
    // including inside an inline fn body, so an inlined call that uses operator-method syntax works.
    (
        "PrimOpMethod",
        r#"
inline fun combine(a: Int, b: Int): Int = a.plus(b).times(2)
fun box(): String {
    if (10.plus(10) != 20) return "f0"
    if (3.times(4) != 12) return "f1"
    if (1L.plus(2) != 3L) return "f2"
    if (17.div(5) != 3 || 17.rem(5) != 2) return "f3"
    if ('a'.plus(1) != 'b') return "f4"
    if (combine(3, 4) != 14) return "f5: ${combine(3, 4)}"
    var a = 10
    if (a?.plus(10) != 20) return "f6"   // vacuous safe call on a non-null primitive ≡ a.plus(10)
    return "OK"
}
"#,
    ),
    // A callable reference (`::g`, `obj::m`) passed to an INLINE higher-order fn: the reference can't be
    // inline-expanded as a body, so it's bound as a FunctionN value and `f(v)` invokes it. (A lambda
    // literal still inlines directly.)
    (
        "InlineCallableRef",
        r#"
fun g(x: Int): Int = x + 1
class C { fun m(x: Int): Int = x * 2 }
inline fun apply1(f: (Int) -> Int, v: Int): Int = f(v)
fun box(): String {
    if (apply1(::g, 5) != 6) return "f0"
    val c = C()
    if (apply1(c::m, 5) != 10) return "f1"
    if (apply1({ it + 100 }, 5) != 105) return "f2"
    return "OK"
}
"#,
    ),
    // Invoking the result of a call directly (`mk()()`, `mk()(x)`) — the callee is itself a call that
    // returns a function value, invoked through `FunctionN.invoke`. Works for both a plain and an inline
    // producer.
    (
        "InvokeCallResult",
        r#"
fun mk(): () -> Int = { 42 }
inline fun mkI(): () -> Int = { 7 }
fun add(): (Int) -> Int = { x: Int -> x + 1 }
fun box(): String {
    if (mk()() != 42) return "f0"
    if (mkI()() != 7) return "f1"
    if (add()(5) != 6) return "f2"
    return "OK"
}
"#,
    ),
    // An `inline fun` with a trailing `vararg`: the call's trailing arguments are packed into an array
    // bound to the parameter, and the inlined body iterates it (incl. the empty-vararg and fixed+vararg
    // forms).
    (
        "InlineVararg",
        r#"
inline fun sum(vararg xs: Int): Int { var s = 0; for (x in xs) s += x; return s }
inline fun join(sep: String, vararg xs: String): String {
    var s = ""; for (x in xs) s += sep + x; return s
}
fun box(): String {
    if (sum(1, 2, 3) != 6) return "f0"
    if (sum() != 0) return "f1"
    if (join("-", "a", "b", "c") != "-a-b-c") return "f2"
    if (join("/") != "") return "f3"
    return "OK"
}
"#,
    ),
    // A lambda that is the expression body (or `return`) of a function whose declared return type is a
    // function type: the lambda's parameter types come from that return type, so `it`/named params type
    // concretely (`fun mk(): (Int) -> Int = { it + 1 }`).
    (
        "LambdaFromReturnType",
        r#"
fun inc(): (Int) -> Int = { it + 1 }
fun addN(n: Int): (Int) -> Int { return { x -> x + n } }
fun combine(): (Int, Int) -> Int = { a, b -> a * b }
fun box(): String {
    if (inc()(5) != 6) return "f0"
    if (addN(10)(5) != 15) return "f1"
    if (combine()(3, 4) != 12) return "f2"
    return "OK"
}
"#,
    ),
    // An inline fn's lambda parameter used as a VALUE (passed on to another call) rather than only
    // invoked: it's materialized as a FunctionN instead of being spliced. A lambda used purely as a
    // callee still splices.
    (
        "InlineLambdaForwarded",
        r#"
inline fun a(f: () -> Int): Int = f()
inline fun b(f: () -> Int): Int = a(f) + 1
fun callIt(f: () -> Int): Int = f()
inline fun c(f: () -> Int): Int = callIt(f) * 2
fun box(): String {
    if (b { 5 } != 6) return "f0"
    if (c { 7 } != 14) return "f1"
    return "OK"
}
"#,
    ),
    // An `inline fun` with default-value parameters: an omitted parameter is filled with its default
    // expression (inline fns substitute the default directly — no `$default` method). Covers a trailing
    // default, a trailing lambda after a default, and named-argument omission.
    (
        "InlineDefaultParam",
        r#"
inline fun f(a: Int, b: Int = 10): Int = a + b
inline fun cfg(x: Int = 3, g: () -> Int): Int = x + g()
inline fun pick(a: Int = 1, b: Int = 2, c: Int = 3): Int = a * 100 + b * 10 + c
fun box(): String {
    if (f(5) != 15) return "f0"
    if (f(5, 1) != 6) return "f1"
    if (cfg(10) { 4 } != 14) return "f2"
    if (pick(b = 9) != 193) return "f3"   // a=1, b=9, c=3 → 100 + 90 + 3
    if (cfg(g = { 4 }) != 7) return "f4"  // x defaulted, the required `g` passed by name
    return "OK"
}
"#,
    ),
    // A required parameter that FOLLOWS a defaulted one (`h(x: Int = 5, y: Int)`), supplied by name —
    // the checker must validate `y` (not `x`) as the required slot, not assume defaults are trailing.
    (
        "DefaultBeforeRequired",
        r#"
fun h(x: Int = 5, y: Int): Int = x * 10 + y
fun box(): String {
    if (h(y = 2) != 52) return "f0"
    if (h(1, 2) != 12) return "f1"
    return "OK"
}
"#,
    ),
    // A labeled local return from an inline lambda (`return@foreachT`) — the canonical inline
    // non-local-return form. In the spliced lambda it acts like `continue`: it ends the current
    // invocation's body, not the enclosing function.
    (
        "InlineLabeledReturn",
        r#"
inline fun foreachT(xs: List<Int>, f: (Int) -> Unit) { for (x in xs) f(x) }
fun box(): String {
    var s = 0
    foreachT(listOf(1, 2, 3)) { s += it; if (it == 2) return@foreachT }
    if (s != 6) return "f0: $s"
    var t = 0
    foreachT(listOf(1, 2, 3, 4)) { if (it % 2 == 0) return@foreachT; t += it }
    if (t != 4) return "f1: $t"
    return "OK"
}
"#,
    ),
    // Source-level NESTING of the same inline fn (`a { a { 5 } }`) — distinct call sites, finite, legal.
    // The recursion guard keys on the call site (not the fn name), so nesting is allowed while genuine
    // recursion (which re-enters the same site) still skips.
    (
        "InlineNested",
        r#"
inline fun a(f: () -> Int): Int = f()
fun box(): String {
    if (a { a { 5 } } != 5) return "f0"
    if (a { a { a { 7 } } } != 7) return "f1"
    val r = a { val x = a { 5 }; x + 1 }
    if (r != 6) return "f2: $r"
    return "OK"
}
"#,
    ),
    // The standalone `run { … }` scope function — resolved as a top-level `@InlineOnly inline fun` from
    // the classpath (no hardcoded name) and spliced from its real stdlib bytecode. Covers a reference,
    // primitive, and Unit result, and a non-local `return` from inside the (inlined) lambda.
    (
        "ScopeRun",
        r#"
fun box(): String {
    val a = run { "O" + "K" }
    if (a != "OK") return "f0: $a"
    val b = run { val x = 2; x + 3 }
    if (b != 5) return "f1: $b"
    var s = 0
    run { s = 42 }
    if (s != 42) return "f2"
    val c = run { if (b == 5) return "OK"; "x" }
    return "f3: $c"
}
"#,
    ),
    // Receiver lambdas (`run`/`apply`) over an ARBITRARY receiver type — a builtin (`String`), a library
    // type (`List`), a classpath class (`StringBuilder`), and a user class. The lambda's `this` is the
    // receiver, so a bare member (`length`/`size`) and a bare method call (`append`/`bump`) resolve
    // against it through the generic implicit-`this` member resolution (no per-member/-function hardcode).
    (
        "ReceiverLambdaAnyReceiver",
        r#"
class C(var v: Int) { fun bump() { v += 1 } }
fun box(): String {
    val n = "ab".run { length }                       // String receiver, bare property read
    if (n != 2) return "f0: $n"
    val sz = listOf(1, 2, 3).run { size }             // library receiver, bare accessor
    if (sz != 3) return "f1: $sz"
    val sb = StringBuilder().apply { append("O"); append("K") }   // classpath receiver, bare method call
    if (sb.toString() != "OK") return "f2: $sb"
    val c = C(1).apply { bump(); bump() }             // user receiver, bare method call
    if (c.v != 3) return "f3: ${c.v}"
    val r = 5.run { this + 1 }                         // primitive receiver via explicit `this`
    if (r != 6) return "f4: $r"
    val up = "ab".run { uppercase() }                  // bare stdlib EXTENSION call on `this`
    if (up != "AB") return "f5: $up"
    val tr = "  hi  ".run { trim() }                   // another stdlib extension on `this`
    if (tr != "hi") return "f6: $tr"
    return "OK"
}
"#,
    ),
    // `StringBuilder` is a Kotlin source-level alias in `kotlin.text`; the JVM provider resolves that
    // alias to the platform class. The resolver must not need a direct `java/lang/StringBuilder` branch.
    (
        "KotlinTextStringBuilderAlias",
        r#"// WITH_STDLIB
import kotlin.text.StringBuilder

fun box(): String {
    val sb: StringBuilder = StringBuilder()
    sb.append("O")
    sb.apply { append("K") }
    return if (sb.toString() == "OK") "OK" else sb.toString()
}
"#,
    ),
    // `takeIf`/`takeUnless` return `T?` — a nullable result the JVM signature erases but `@Metadata`
    // keeps. Over a PRIMITIVE receiver the result is the boxed wrapper, so `?: default` must keep its
    // null-check (a primitive-typed result would fold the elvis away and unbox a possibly-null value →
    // NPE — the bug this fixes). The lambda's `it` is the receiver. MULTIPLE such branchy splices (each a
    // `takeIf`/`takeUnless` with an elvis) live in ONE method — the emitter resets the operand-stack
    // baseline per statement, so a later branchy splice isn't blocked by an earlier one's tracker drift.
    (
        "TakeIfNullableResult",
        r#"
fun box(): String {
    val a = 5.takeIf { it > 3 } ?: 0            // predicate true → 5
    if (a != 5) return "f0: $a"
    val b = 5.takeIf { it < 3 } ?: 9            // predicate false → null → 9
    if (b != 9) return "f1: $b"
    val c = 5.takeUnless { it > 3 } ?: 0        // predicate true → null → 0
    if (c != 0) return "f2: $c"
    val d = 5.takeUnless { it < 3 } ?: 7        // predicate false → 5
    if (d != 5) return "f3: $d"
    val s = "ab".takeIf { it.length > 1 } ?: "x"   // reference receiver
    if (s != "ab") return "f4: $s"
    val n: Int? = 5.takeIf { it > 3 }           // nullable return type
    if (n != 5) return "f5: $n"
    return "OK"
}
"#,
    ),
    // `sumOf { selector }` — `@OverloadResolutionByLambdaReturnType`: the source name `sumOf` has no JVM
    // method; kotlinc picks `sumOfInt`/`sumOfLong`/`sumOfDouble`/… by the lambda's RETURN type (a
    // `@JvmName`-mangled, package-private `@InlineOnly` method whose fold-loop body is spliced). Resolved by
    // deriving `sumOf` + the return type's name and verifying it against the real classpath method.
    (
        "SumOfByLambdaReturn",
        r#"
class P(val price: Int)
fun box(): String {
    val i = listOf(1, 2, 3).sumOf { it * 2 }            // -> sumOfInt
    if (i != 12) return "f0: $i"
    val l = listOf(1, 2, 3).sumOf { it.toLong() }       // -> sumOfLong
    if (l != 6L) return "f1: $l"
    val d = listOf(1, 2, 3).sumOf { it * 1.5 }          // -> sumOfDouble
    if (d != 9.0) return "f2: $d"
    val f = listOf(P(2), P(3)).sumOf { it.price }       // selector over a user property
    if (f != 5) return "f3: $f"
    val s = setOf(1, 2, 3).sumOf { it }                 // Set receiver
    if (s != 6) return "f4: $s"
    val a = intArrayOf(1, 2, 3).sumOf { it * 2 }        // IntArray (not UIntArray — selector picks it)
    if (a != 12) return "f5: $a"
    return "OK"
}
"#,
    ),
    // No-lambda stdlib `@InlineOnly` extensions on a primitive receiver — `Char.isDigit()`/`isLetter()`/
    // `uppercaseChar()`/… inline their real body (`Character.isDigit(this)`/`toUpperCase(this)`). Accepted
    // only for a non-unsigned primitive receiver + primitive/`String` return (an unsigned return like
    // `toUShort(): UShort` is rejected — krusty can't model it, so it skips rather than miscompiling).
    (
        "PrimitiveInlineExtension",
        r#"
fun box(): String {
    if (!'7'.isDigit()) return "f0"
    if ('a'.isDigit()) return "f1"
    if (!'a'.isLetter()) return "f2"
    if (!' '.isWhitespace()) return "f3"
    if ('a'.uppercaseChar() != 'A') return "f4"
    if ('A'.lowercaseChar() != 'a') return "f5"
    val up = "aBc".map { it.uppercaseChar() }.joinToString("")
    if (up != "ABC") return "f6: $up"
    return "OK"
}
"#,
    ),
    // Safe-call scope functions `s?.let { it… }` / `?.run` / `?.also` / `?.apply` — the most idiomatic
    // null-handling form: when the receiver is non-null run the scope fn (binding `it`/`this`), else the
    // whole expression is `null`. The trailing lambda attaches to the safe call; `let`/`run` yield the
    // body (nullable), `also`/`apply` yield the receiver.
    (
        "SafeCallScopeFn",
        r#"
class B(var v: Int)
fun box(): String {
    val s: String? = "ab"
    val z: String? = null
    if ((s?.let { it.length } ?: 0) != 2) return "f0"
    if ((z?.let { it.length } ?: 9) != 9) return "f1"          // null receiver → 9
    if ((s?.run { length } ?: 0) != 2) return "f2"
    if ((s?.let { it + "!" } ?: "x") != "ab!") return "f3"
    if ((z?.let { it + "!" } ?: "x") != "x") return "f4"       // null receiver → "x"
    val b: B? = B(1)
    b?.also { it.v = 5 }
    if (b!!.v != 5) return "f5: ${b.v}"
    val r = B(1).apply { v = 7 }
    if (r.v != 7) return "f6: ${r.v}"
    if ((s?.uppercase() ?: "x") != "AB") return "f7"          // safe-call stdlib extension
    if ((s?.uppercase()?.length ?: 0) != 2) return "f8"       // chained safe calls
    if ((z?.uppercase()?.length ?: 0) != 0) return "f9"       // null short-circuits the chain
    if ((s?.let { it.length }?.let { it + 1 } ?: 0) != 3) return "f10"  // chained ?.let, primitive
    if ((z?.let { it.length }?.let { it + 1 } ?: 0) != 0) return "f11"  // null short-circuits
    if ((s?.run { length }?.run { this * 3 } ?: 0) != 6) return "f12"   // chained ?.run, unboxed `this`
    return "OK"
}
"#,
    ),
    // `with(receiver) { … }` — the stdlib 2-arg receiver-lambda scope function: the first argument is
    // the lambda body's implicit `this`. Driven by the checker's recorded receiver-lambda decision (the
    // same generic path as `x.run`/`x.apply`), over a builtin / classpath / user receiver, with member
    // reads, member calls, and stdlib extension calls — including nesting inside another scope lambda.
    (
        "WithReceiver",
        r#"
class B(val v: Int) { fun dbl() = v * 2 }
fun box(): String {
    val n = with("ab") { length }                        // builtin receiver, member read
    if (n != 2) return "f0: $n"
    val up = with("ab") { uppercase() }                  // stdlib extension on `this`
    if (up != "AB") return "f1: $up"
    val s = with(StringBuilder()) { append("O"); append("K"); toString() }  // classpath, member calls
    if (s != "OK") return "f2: $s"
    val d = with(B(3)) { dbl() }                         // user receiver, member method
    if (d != 6) return "f3: $d"
    val nested = "xy".run { with(this) { length } }      // `with` nested in a receiver lambda
    if (nested != 2) return "f4: $nested"
    return "OK"
}
"#,
    ),
    // A bare call to a stdlib EXTENSION (`uppercase`/`reversed`) through an extension function's own
    // implicit `this` receiver — `this.uppercase()` written unqualified, resolved through the same
    // extension-call path as a qualified receiver call (no per-function hardcode).
    (
        "ExtensionFnBodyBareExtCall",
        r#"
fun String.shout(): String = uppercase()
fun String.echo(): String = this.shout() + "!" + reversed()
fun box(): String {
    if ("ab".shout() != "AB") return "f0: ${"ab".shout()}"
    if ("ab".echo() != "AB!ba") return "f1: ${"ab".echo()}"
    return "OK"
}
"#,
    ),
    // `repeat(n) { i -> … }` — the stdlib top-level `inline fun`, spliced from its real loop body via the
    // generic route (no name-match desugar). The index `i` is `Int`; a mutable capture works (inline).
    (
        "Repeat",
        r#"
fun box(): String {
    var s = 0
    repeat(4) { s += it }
    if (s != 6) return "f0: $s"
    var c = 0
    repeat(3) { c++ }
    if (c != 3) return "f1: $c"
    return "OK"
}
"#,
    ),
    // A non-local `return` through a spliced loop host (`repeat`/`forEach`) — including the unconditional
    // (diverging-body) form. The splicer relocates the stack-map frame the host's now-unreachable
    // post-invoke continuation needs.
    (
        "InlineNonLocalReturnThroughLoop",
        r#"
fun first(): String {
    repeat(5) { return "OK" }
    return "Fail"
}
fun firstEven(xs: List<Int>): Int {
    xs.forEach { if (it % 2 == 0) return it }
    return -1
}
fun box(): String {
    if (first() != "OK") return "f0"
    if (firstEven(listOf(1, 3, 4, 5)) != 4) return "f1"
    return "OK"
}
"#,
    ),
    // A non-local `return` from a lambda passed to a USER `inline fun` (the IR-inliner path). The bare
    // `return` is non-local — it returns from `box`, not the inline fn — even when the inline fn has its
    // own `return`.
    (
        "UserInlineNonLocalReturn",
        r#"
inline fun forEachI(xs: List<Int>, f: (Int) -> Unit) { for (x in xs) f(x) }
inline fun firstOr(g: () -> Int): Int { val v = g(); return v }
fun pick(xs: List<Int>): String {
    forEachI(xs) { if (it == 3) return "found" }
    return "none"
}
fun box(): String {
    if (pick(listOf(1, 2, 3, 4)) != "found") return "f0"
    if (pick(listOf(1, 2)) != "none") return "f1"
    if (firstOr { 41 } != 41) return "f2"
    return "OK"
}
"#,
    ),
    // A default LAMBDA parameter (`g: (Int) -> Int = { it + 1 }`): the default lambda's `it` types from the
    // declared function type, and an omitted argument uses it.
    (
        "DefaultLambdaParam",
        r#"
inline fun applyOr(x: Int, g: (Int) -> Int = { it + 1 }): Int = g(x)
fun box(): String {
    if (applyOr(5) != 6) return "f0"
    if (applyOr(5) { it * 2 } != 10) return "f1"
    return "OK"
}
"#,
    ),
    // A generic-RECEIVER extension inline fn (`<T> T.applyIt(f: (T) -> R)`): the lambda's `it` specializes
    // to the actual receiver type, so `it.length` (a `String` member) resolves — not the erased `Object`.
    (
        "GenericReceiverExtInline",
        r#"
inline fun <T> T.applyIt(f: (T) -> Int): Int = f(this)
fun box(): String {
    val n = "abc".applyIt { it.length }
    if (n != 3) return "f0: $n"
    val m = listOf(1, 2, 3, 4).applyIt { it.size }
    if (m != 4) return "f1: $m"
    return "OK"
}
"#,
    ),
    // A user generic `inline fun` taking a lambda (`twice(1) { it+10 }`): the inliner SPECIALIZES the type
    // parameter `T` from the call's VALUE arguments — the lambda's `it`, the value-parameter slots, and the
    // call's return type are the concrete type (`Int`/`String`), not the erased `Any`. The body is inlined
    // (no call). (A type param bound only by a lambda's RETURN, e.g. `<T,R> (T)->R`, is a separate follow-up.)
    (
        "GenericInlineHof",
        r#"
inline fun <T> twice(x: T, f: (T) -> T): T = f(f(x))
inline fun <T, R> applyFn(x: T, f: (T) -> R): R = f(x)
fun box(): String {
    val a = twice(1) { it + 10 }
    if (a != 21) return "f0: $a"
    val s = twice("x") { it + "!" }
    if (s != "x!!") return "f1: $s"
    val b = applyFn("ab") { it.length }   // R bound by the lambda's return type (Int)
    if (b != 2) return "f2: $b"
    return "OK"
}
"#,
    ),
    // `Char.MAX_VALUE`/`MIN_VALUE` companion constants keep their `Char` type when boxed (a vararg/generic
    // position): `listOf(Char.MAX_VALUE, …)` is a `List<Char>` holding `Character`s, not `Integer`s.
    (
        "CharCompanionConst",
        "fun box(): String {\n    val l = listOf(Char.MAX_VALUE, Char.MIN_VALUE)\n    if (l[0] != '\\uFFFF') return \"f0\"\n    if (l[1] != '\\u0000') return \"f1\"\n    return \"OK\"\n}\n",
    ),
    // `a until b` resolves to the `Int` overload (which has the `MIN_VALUE` guard), not the guard-less
    // `Byte`/`Short` overload krusty's `Byte`/`Short`/`Int` → `Int` collapse made indistinguishable:
    // `2 until Int.MIN_VALUE` must be EMPTY, not wrap to `2..Int.MAX_VALUE`.
    (
        "UntilIntOverloadGuard",
        r#"
fun box(): String {
    val empty = 2 until Int.MIN_VALUE   // value-form `until` → Int overload (MIN_VALUE guard) → empty
    var n = 0
    for (i in empty) n++
    if (n != 0) return "f1:$n"
    val r = 0 until 5
    var s = 0
    for (i in r) s += i                 // 0+1+2+3+4 = 10
    if (s != 10) return "f2:$s"
    return "OK"
}
"#,
    ),
    // A `Char` range with a `step` keeps `Char` loop elements (the step is an `Int`), and a stepped
    // `Int`/`Long` range near MAX/MIN_VALUE terminates without wrapping past the bound (overflow-safe).
    (
        "SteppedRangeCharAndOverflow",
        r#"
fun box(): String {
    val sb = StringBuilder()
    for (c in 'a'..'e' step 2) sb.append(c)        // a,c,e (loop var is Char, step Int)
    if (sb.toString() != "ace") return "char: $sb"
    val a = ArrayList<Int>()
    for (i in Int.MAX_VALUE - 5..Int.MAX_VALUE step 3) a.add(i)   // MaxI-5, MaxI-2 (no wrap)
    if (a.toString() != "[2147483642, 2147483645]") return "int: $a"
    val b = ArrayList<Long>()
    for (i in Long.MAX_VALUE - 5..Long.MAX_VALUE step 3) b.add(i) // no wrap on Long either
    if (b.size != 2) return "long: $b"
    return "OK"
}
"#,
    ),
    // A user `plusAssign`/`minusAssign` operator: `target op= rhs` is an in-place CALL, legal even on a
    // `val` (member operator AND extension operator), not a reassignment.
    (
        "UserPlusAssign",
        r#"
class Box(var v: String) {
    operator fun plusAssign(s: String) { v += s }   // member opAssign
}
class Acc(var n: Int)
operator fun Acc.minusAssign(d: Int) { n -= d }     // extension opAssign
class Holder { val b = Box("O"); val a = Acc(10) }
fun box(): String {
    val h = Holder()
    h.b += "K"          // val property, member plusAssign
    h.a -= 3            // val property, extension minusAssign
    val local = Box("x")
    local += "y"        // local val, member plusAssign
    if (h.b.v != "OK") return "fail b=${h.b.v}"
    if (h.a.n != 7) return "fail a=${h.a.n}"
    if (local.v != "xy") return "fail local=${local.v}"
    return "OK"
}
"#,
    ),
    // Collection `+=`: resolved exactly as kotlinc (no mutability predicate). A `MutableList`/`MutableSet`/
    // `MutableMap` receiver (and a concrete `ArrayList`) resolves `MutableCollection.plusAssign`, spliced
    // to in-place `add`/`addAll`; a read-only `List` has NO applicable `plusAssign` (the candidate's Kotlin
    // receiver is `MutableCollection`, not a supertype of `List`), so `l += x` lowers as `l = l.plus(x)`
    // (reassignment of the `var`). The read-only/mutable distinction comes from `@Metadata`/builtins, never
    // from the JVM type (both erase to `java/util/List`).
    (
        "CollectionPlusAssign",
        r#"
fun box(): String {
    val ml = mutableListOf(1, 2)
    ml += 3                 // MutableList.plusAssign(element) -> add
    ml += listOf(4, 5)      // MutableList.plusAssign(elements) -> addAll
    if (ml != listOf(1, 2, 3, 4, 5)) return "f0=$ml"

    val ms = mutableSetOf("a")
    ms += "b"
    if (ms.size != 2 || !ms.contains("b")) return "f1=$ms"

    val mm = mutableMapOf(1 to "x")
    mm += (2 to "y")        // MutableMap.plusAssign(pair)
    if (mm.size != 2 || mm[2] != "y") return "f2=$mm"

    val al = ArrayList<Int>()   // concrete mutable class
    al += 7
    if (al[0] != 7) return "f3=$al"

    var ro = listOf(1, 2)   // read-only: += is reassignment (l = l.plus(x))
    val before = ro
    ro += 3
    if (ro != listOf(1, 2, 3)) return "f4=$ro"
    if (before != listOf(1, 2)) return "f5 read-only mutated: $before"
    return "OK"
}
"#,
    ),
    // `error(msg)` is a real `@InlineOnly` stdlib function (`throw IllegalStateException(msg.toString())`)
    // — discovered via `@Metadata` and SPLICED from its jar bytecode (no hardcoded body). Its body
    // diverges (`athrow`), so this also exercises diverging-inline-call divergence handling in linear,
    // `if`-branch, and `try` positions.
    (
        "ErrorInline",
        r#"
fun box(): String {
    val a = try { error("boom") } catch (e: IllegalStateException) { "caught" }
    if (a != "caught") return "f0: $a"
    val s = "y"
    if (s == "n") error("dead")          // diverging call in an if-branch (must not be taken)
    val b = try { error("x") } catch (e: RuntimeException) { "rte" }   // ISE caught as supertype
    if (b != "rte") return "f1: $b"
    return "OK"
}
"#,
    ),
    // `require`/`check`: branchy, NON-public (`@InlineOnly`) `inline fun` from the stdlib. There is no
    // callable method to invoke — kotlinc inlines the body (`if (!cond) throw IllegalXException(…)`), so
    // krusty splices it via `splice_branchy` (StackMapTable relocate). A passing condition falls through;
    // a failing one throws. Exercises branchy inline splicing of a non-public callee at statement level.
    (
        "RequireCheck",
        r#"
fun box(): String {
    require(1 + 1 == 2)
    check("a".length == 1)
    var caught = false
    try { require(false) } catch (e: IllegalArgumentException) { caught = true }
    if (!caught) return "f0"
    var c2 = false
    try { check(false) } catch (e: IllegalStateException) { c2 = true }
    if (!c2) return "f1"
    return "OK"
}
"#,
    ),
    // Two-arg `require(cond) { lazyMessage }` / `check(cond) { … }`: the BRANCHY host body invokes the
    // message lambda only on failure. The unified splicer splices the branchy precondition body AND the
    // lambda body at its `Function0.invoke` site together (kotlinc inlines both — no `Function0` is
    // materialized). The lambda runs (captures + mutates an outer local) only when the condition fails.
    (
        "RequireCheckMsg",
        r#"
fun box(): String {
    var pass = 0
    require(true) { pass += 1; "unused" }
    if (pass != 0) return "f0: $pass"     // lambda must NOT run when the condition holds
    var ran = false
    try { require(false) { ran = true; "boom" } } catch (e: IllegalArgumentException) { }
    if (!ran) return "f1"                 // lambda MUST run when the condition fails
    var ran2 = false
    try { check(false) { ran2 = true; "nope" } } catch (e: IllegalStateException) { }
    if (!ran2) return "f2"
    return "OK"
}
"#,
    ),
    // Function overloading: same name, different parameter signatures. A call selects the matching
    // overload by argument types (and arity); each overload emits as its own JVM method (same name,
    // different descriptor). Covers type-distinguished, arity-distinguished, and the ordering-sensitive
    // `(Int, Any)` vs `(Any, Int)` case.
    (
        "FunctionOverloading",
        r#"
fun f(x: Int): String = "int:$x"
fun f(x: String): String = "str:$x"
fun f(x: Int, y: Int): String = "two:${x + y}"
fun g(x: Int, y: Any): String = "IA"
fun g(x: Any, y: Int): String = "AI"
fun box(): String {
    if (f(1) != "int:1") return "f0"
    if (f("a") != "str:a") return "f1"
    if (f(2, 3) != "two:5") return "f2"
    if (g(1, "x") != "IA") return "f3:${g(1, "x")}"
    if (g("x", 1) != "AI") return "f4:${g("x", 1)}"
    return "OK"
}
"#,
    ),
    // `is`/`!is` with a NULLABLE reference target (`x is A?`): `null` IS an `A?`, so it lowers to
    // `x == null || x is A` (and `x !is A?` to `x != null && x !is A`), not a bare `instanceof` (which is
    // false for null). The operand is evaluated once.
    (
        "IsNullableType",
        r#"
interface I
class A : I
class B
fun box(): String {
    val a: Any? = A()
    if (a !is A?) return "f0"
    val n: Any? = null
    if (n !is A?) return "f1"
    if (n !is I?) return "f2"
    val b: Any? = B()
    if (b is A?) return "f3"
    if (a !is A) return "f4"
    return "OK"
}
"#,
    ),
    // The `Unit` singleton used as a VALUE (not a type) — `Unit`, `take(Unit)`, `val u = Unit` — is the
    // `kotlin/Unit` object, materialized as `getstatic kotlin/Unit.INSTANCE`. `toString()` is "kotlin.Unit"
    // and the singleton compares equal (and identical) to itself.
    (
        "UnitAsValue",
        r#"
fun take(x: Any): String = x.toString()
fun box(): String {
    val u = Unit
    if (u.toString() != "kotlin.Unit") return "f0: $u"
    if (take(Unit) != "kotlin.Unit") return "f1"
    if (u != Unit) return "f2"
    val any: Any = Unit
    if (any !== Unit) return "f3"
    return "OK"
}
"#,
    ),
    // `for (x in <iterable> step n)` where the iterable is not a `..` literal (a progression val, a
    // `.reversed()` result, a chained `step`): the for-range parser continues the trailing `step`
    // infix call so the whole `progression.step(n)` becomes the loop iterable.
    (
        "StepOnProgression",
        r#"
fun box(): String {
    val p = 1..10
    var s = 0
    for (i in p step 2) { s += i }                 // 1,3,5,7,9 = 25
    var r = 0
    for (i in (1..9).reversed() step 2) { r += i } // 9,7,5,3,1 = 25
    var t = 0
    for (i in p step 2 step 3) { t += i }          // fromClosedRange(1,9,3): 1,4,7 = 12
    return if (s == 25 && r == 25 && t == 12) "OK" else "F:$s,$r,$t"
}
"#,
    ),
    // An extension function body referencing the receiver's members implicitly (`n` = `this.n`,
    // including inside a string template) — read through the property getter since the body is outside
    // the class. Plus an ordinary-named extension on a *nullable* reference receiver (`A?.foo()`), whose
    // body branches on `this == null`. (Operator extensions on nullable receivers stay unsupported.)
    (
        "ExtensionImplicitReceiver",
        r#"
class A(val n: Int)
fun A.twice(): Int = n + n
fun A.label(): String = "n=$n"
fun A?.orZero(): Int = if (this == null) 0 else n
fun box(): String {
    val a = A(5)
    if (a.twice() != 10) return "f1:${a.twice()}"
    if (a.label() != "n=5") return "f2:${a.label()}"
    val nn: A? = null
    if (a.orZero() != 5) return "f3"
    if (nn.orZero() != 0) return "f4"
    return "OK"
}
"#,
    ),
    // Referential identity `===`/`!==`: object refs compare with `if_acmp*` (distinct instances are
    // not identical, the same instance is); a boxed Boolean is the cached singleton so `===` holds; on
    // primitive operands `===` is just value `==`.
    (
        "RefIdentity",
        r#"
class C(val n: Int)
fun boxB(b: Boolean): Any = b
fun box(): String {
    val a = C(1); val b = C(1); val aa = a
    if (a === b) return "f1"
    if (a !== aa) return "f2"
    if (!(a === a)) return "f3"
    if (boxB(true) !== boxB(true)) return "f4"
    if (boxB(true) === boxB(false)) return "f5"
    val i = 7; val j = 7L
    if (!(i === i) || i !== i) return "f6"
    if (!(j === j)) return "f7"
    return "OK"
}
"#,
    ),
    // Char arithmetic: `Char + Int`/`Char - Int` → Char (truncated mod 2^16 with i2c, so it wraps),
    // `Char - Char` → Int (the distance). No promotion between Char and Int — the op runs on ints.
    (
        "CharArithmetic",
        r#"
fun box(): String {
    if ('a' + 2 != 'c') return "f1"
    if ('z' - 1 != 'y') return "f2"
    if ('z' - 'a' != 25) return "f3"
    val lo: Char = Char.MIN_VALUE
    if ((lo - 1) <= lo) return "f4"          // 0 - 1 -> 0xFFFF, wraps high
    val hi: Char = Char.MAX_VALUE
    if ((hi + 1) >= hi) return "f5"          // 0xFFFF + 1 -> 0, wraps low
    return "OK"
}
"#,
    ),
    // Primitive-array init constructor `IntArray(n) { i -> elem }`: the index lambda is inlined into a
    // fill loop (`new T[n]; i=0; while (i<n) { a[i]=body(i); i++ }`). The single param is the index.
    (
        "PrimArrayInit",
        r#"
fun box(): String {
    val a = IntArray(4) { it * it }
    if (a[0] != 0 || a[3] != 9) return "f1:${a[3]}"
    val c = CharArray(3) { 'a' + it }
    if (c[2] != 'c') return "f2"
    val d = DoubleArray(2) { i -> i + 0.5 }
    if (d[1] != 1.5) return "f3"
    val b = BooleanArray(3) { it % 2 == 0 }
    if (!b[0] || b[1] || !b[2]) return "f4"
    var s = 0L
    LongArray(3) { (it + 1).toLong() }.forEach { s += it }
    if (s != 6L) return "f5:$s"
    return "OK"
}
"#,
    ),
    // Reference array init constructor `Array<T>(n) { i -> elem }` (anewarray + fill loop), plus the
    // `x == null` / `x != null` reference null-check compiling to `ifnull`/`ifnonnull` even when the
    // element value is read through a local (a frame-pinned `if_icmp*` on a ref would not verify).
    (
        "RefArrayInit",
        r#"
fun box(): String {
    val a = Array(3) { i -> "s$i" }
    if (a[0] != "s0" || a[2] != "s2" || a.size != 3) return "f1"
    val b = Array<String?>(2) { null }
    val x = b[0]
    if (x != null) return "f2"
    if (b[1] != null) return "f3"
    val n = Array(3) { if (it == 1) "mid" else "x" }   // branchy body
    if (n[1] != "mid" || n[0] != "x") return "f4"
    return "OK"
}
"#,
    ),
    // Safe cast `x as? T`: `{ val t = x; if (t is T) t as T else null }` — `instanceof` then
    // `checkcast` on a match, `null` on a mismatch (never throws). Reference targets only.
    (
        "SafeCast",
        r#"
open class Base
class Sub : Base()
fun box(): String {
    val a: Any = "hi"
    if ((a as? String) != "hi") return "f1"
    if ((a as? Sub) != null) return "f2"
    val s: Base = Sub()
    if ((s as? Sub) == null) return "f3"
    if ((Base() as? Sub) != null) return "f4"
    val n: Any = 5
    val r = (n as? String) ?: "OK"
    if (r != "OK") return "f5"
    return "OK"
}
"#,
    ),
    // `is`/`as`/`as?` against a classpath type (`CharSequence`, `Number`, `Comparable`), plus the
    // `ACC_BRIDGE` `compareTo(Object)` a class implementing a generic classpath interface needs so an
    // interface-typed call dispatches to the specialized override (here the bridge's checkcast throws CCE).
    (
        "ClasspathIsAs",
        r#"
class Foo(val s: String) : Comparable<Foo> {
    override fun compareTo(other: Foo): Int = s.compareTo(other.s)
}
fun box(): String {
    val a: Any = "hi"
    if (a !is CharSequence) return "f1"
    val num: Any = 5
    if (num !is Number || (a as? Number) != null) return "f2"
    if ("x" is Number) return "f3"
    val n = a as CharSequence
    if (n.length != 2) return "f4"
    if (Foo("a").compareTo(Foo("b")) >= 0) return "f5"
    try {
        (Foo("1") as Comparable<Any>).compareTo(2)
        return "f6"
    } catch (e: ClassCastException) {}
    return "OK"
}
"#,
    ),
    // Receiver scope functions `run`/`apply` (the receiver is `this`): the body's bare member reads,
    // writes, and method calls resolve against the receiver through its getter/setter/method (external
    // access, since the inlined body runs in the caller). `run` yields the body, `apply` the receiver.
    (
        "ApplyRun",
        r#"
class C(val n: Int) { var x = 0; var y = 0; fun a() = n; fun b() = n * 2 }
fun box(): String {
    val c = C(1).apply { x = 5; y = x + 2 }     // write + read-own-write, yields receiver
    if (c.x != 5 || c.y != 7) return "f1:${c.x},${c.y}"
    val r = C(3).run { x = 10; x + n }           // write then read, yields body
    if (r != 13) return "f2:$r"
    val s = C(4).run { a(); b() }                // method calls, yields last
    if (s != 8) return "f3:$s"
    return "OK"
}
"#,
    ),
    // A bare `x++` / `x--` on a `var` field of the enclosing class (implicit `this.x`) in statement
    // position — `this.x = this.x ± 1` via direct field read/write, with Byte/Short/Char width-wrap.
    (
        "MemberIncDec",
        r#"
class C {
    var i = 0
    var n = 5L
    var b: Byte = 127
    fun go() { i++; i++; n--; b++ }
}
fun box(): String {
    val c = C()
    c.go()
    if (c.i != 2) return "f1:${c.i}"
    if (c.n != 4L) return "f2"
    if (c.b.toInt() != -128) return "f3:${c.b}"
    return "OK"
}
"#,
    ),
    // Bare access to INHERITED `var`/`val` members from a subclass method: read, write, and `++`/`--`
    // resolve through the class's superclass chain (inherited writes/incdec go via the getter/setter).
    (
        "InheritedMembers",
        r#"
open class A(val base: Int) { var count = 0; var label = "x" }
class B(b: Int) : A(b) {
    fun read() = base + count                 // inherited read (inferred return)
    fun mutate() { count = 10; count++; label = "y" }
}
fun box(): String {
    val b = B(3)
    if (b.read() != 3) return "f1:${b.read()}"
    b.mutate()
    if (b.count != 11) return "f2:${b.count}"
    if (b.label != "y") return "f3"
    if (b.read() != 14) return "f4:${b.read()}"
    return "OK"
}
"#,
    ),
    // A method with an inferred expression body can resolve a call to another method of the same class
    // (`fun b() = a()`), via `this`, or to an inherited method — the return-type inference scope is seeded
    // with this class's and its superclasses' explicitly-typed method returns.
    (
        "MethodCallInference",
        r#"
open class A { fun base(): Int = 100 }
class C : A() {
    fun a(): Int = 5
    fun b() = a()
    fun c() = this.a() + a()
    fun d() = base() + a()
}
fun box(): String {
    val o = C()
    if (o.b() != 5) return "f1"
    if (o.c() != 10) return "f2:${o.c()}"
    if (o.d() != 105) return "f3:${o.d()}"
    return "OK"
}
"#,
    ),
    // Dead code after a diverging statement (`throw`/`return`) is dropped, like kotlinc — emitting it
    // would leave a (dead) branch target without the stackmap frame the verifier requires. Plus: a
    // side-effecting `for`-range `step` is evaluated exactly once (hoisted), not per iteration.
    (
        "DeadCodeAndStep",
        r#"
val log = StringBuilder()
fun stepN(): Int { log.append("S"); return 2 }
fun mid(): String {
    try {
        throw RuntimeException("x")
        @Suppress("UNREACHABLE_CODE") log.append("never")   // dropped
    } catch (e: Exception) {
        log.append("C")
    }
    return "m"
}
fun box(): String {
    if (mid() != "m") return "f0"
    var sum = 0
    for (i in 1 until 9 step stepN()) { sum += i }          // 1+3+5+7 = 16, stepN() once
    if (sum != 16) return "f1:$sum"
    if (log.toString() != "CS") return "f2:$log"            // C (catch), S (one step eval)
    return "OK"
}
"#,
    ),
    // `return throw e` — the returned value itself diverges (athrow), so the trailing return opcode is
    // unreachable dead code the verifier rejects unless dropped.
    (
        "ReturnThrow",
        r#"
fun t(b: Boolean): String {
    if (b) return throw RuntimeException("boom")
    return "OK"
}
fun box(): String = t(false)
"#,
    ),
    // An arithmetic operand that is branchy (`5 + if (c) 1 else 2`, `r += if (…) … else …`) spills the
    // other operand to a temp so it isn't stranded on the stack across the branch's merge frame.
    (
        "BranchyArithmetic",
        r#"
fun box(): String {
    val c = true
    val z = 5 + (if (c) 1 else 2)
    if (z != 6) return "f1:$z"
    var r = 20
    r += if (r > 0) 3 else 4
    if (r != 23) return "f2:$r"
    val w = 3L * (if (!c) 2L else 4L)
    if (w != 12L) return "f3"
    val m = (if (c) 10 else 0) and (if (c) 6 else 0)   // bitwise + branchy
    if (m != 2) return "f4:$m"
    return "OK"
}
"#,
    ),
    // A nullable-primitive FIELD smart-cast to its primitive (`if (value != null) value else 42` where
    // `value: Int?`) unboxes the wrapper on read; a `finally { return … }` overrides a throwing/returning
    // body. (Both surfaced enabling kotlin.test default-arg calls.)
    (
        "FieldSmartcastAndFinally",
        r#"
class Box(val value: Int?) { fun get(): Int = if (value != null) value else 42 }
fun viaFinally(): Int {
    try { throw RuntimeException("x") } finally { return 7 }
}
fun box(): String {
    if (Box(17).get() != 17) return "f1"
    if (Box(null).get() != 42) return "f2"
    if (viaFinally() != 7) return "f3"
    return "OK"
}
"#,
    ),
    // Mixed-primitive `a.compareTo(b)` → `{Integer,Long,Float,Double}.compare` after promoting both to
    // their common type. Plus: a negated float/double literal is the negative constant (`-0.0` keeps its
    // sign, so `Double.compare(0.0, -0.0) == 1`), not `0.0 - 0.0` (which would be `+0.0`).
    (
        "CompareToAndNegZero",
        r#"
fun box(): String {
    if (1.compareTo(1.1) >= 0) return "f1"
    if (2.compareTo(1.0) <= 0) return "f2"
    if (5L.compareTo(3) <= 0) return "f3"
    if (0.toByte().compareTo(5.0) >= 0) return "f4"
    if (0.0.compareTo(-0.0) != 1) return "f5"           // +0.0 > -0.0 in the total order
    if ((-0.0).compareTo(0.0) != -1) return "f6"
    if ((-2.5).toString() != "-2.5") return "f7"
    return "OK"
}
"#,
    ),
    // `when (subject)` with `in`/`!in` *range* condition branches (`in 4..6 -> …`), mixed with
    // comma-list and `is` branches. (`in <range>` is the bounds-check intrinsic — `InRange` — same as
    // kotlinc; `in <collection>` is unsupported and skips, not string-matched.)
    (
        "WhenInRange",
        r#"
fun cls(x: Int): String = when (x) {
    0 -> "zero"
    in 1..9 -> "low"
    !in -100..100 -> "far"
    else -> "other"
}
fun box(): String {
    if (cls(0) != "zero") return "f1"
    if (cls(5) != "low") return "f2"
    if (cls(500) != "far") return "f3:${cls(500)}"
    if (cls(50) != "other") return "f4"
    return "OK"
}
"#,
    ),
    // A `return` inside a `try { … } finally { … }` runs the finally before transferring control; the
    // return value is captured before the finally so a finally that mutates state can't change it. The
    // finally also runs on the normal-completion path (kotlinc semantics). (A finally that declares
    // locals is skipped, not modeled here.)
    (
        "ReturnInTryFinally",
        r#"
val log = StringBuilder()
fun early(c: Boolean): String {
    try {
        if (c) return "early"
        log.append("body;")
    } finally {
        log.append("fin;")
    }
    return "late"
}
fun valueCapturedBeforeFinally(): Int {
    var r = 1
    try { return r } finally { r = 99 }
}
fun box(): String {
    if (early(true) != "early") return "f1"
    if (early(false) != "late") return "f2"
    if (log.toString() != "fin;body;fin;") return "f3:$log"   // true→fin; false→body;fin;
    if (valueCapturedBeforeFinally() != 1) return "f4"
    return "OK"
}
"#,
    ),
    // Array stdlib extensions `isEmpty()`/`isNotEmpty()`/`count()` → the `arraylength` intrinsic
    // (`size == 0` / `size != 0` / `size`), for both primitive and reference arrays.
    (
        "ArrayIsEmpty",
        r#"
fun box(): String {
    val a = intArrayOf(1, 2, 3)
    val e = IntArray(0)
    if (a.isEmpty() || !a.isNotEmpty() || a.count() != 3) return "f1"
    if (!e.isEmpty() || e.isNotEmpty() || e.count() != 0) return "f2"
    val r = arrayOf("x", "y")
    if (!r.isNotEmpty() || r.count() != 2) return "f3"
    return "OK"
}
"#,
    ),
    // A class method's expression-body return type is inferred with its own PARAMETERS in scope
    // (`fun m(x: Int) = x + 1` → Int), which also unblocks a bound method reference `obj::m`.
    (
        "MethodParamInference",
        r#"
class C(val base: Int) {
    fun inc(x: Int) = x + 1
    fun add(a: Int, b: Int) = a + b + base
}
fun box(): String {
    val c = C(10)
    if (c.inc(5) != 6) return "f1"
    if (c.add(2, 3) != 15) return "f2"
    val r = listOf(1, 2, 3).map(c::inc)            // bound method ref
    if (r != listOf(2, 3, 4)) return "f3:$r"
    return "OK"
}
"#,
    ),
    (
        "Unsigned",
        r#"
fun box(): String {
    val u1 = 1u; val u2 = 2u
    val u3 = u1 + u2
    if (u3.toInt() != 3) return "f1"
    val a = 42.toUInt()
    if (a.toInt() != 42) return "f2"
    val d = 0u.dec()
    if (d.toLong() != 4294967295L) return "f3"
    val ul = 5uL
    if (ul.toLong() != 5L) return "f4"
    if ((3u - 1u).toInt() != 2) return "f5"
    if (0xFFu.toInt() != 255) return "f6"
    val x = 5u; val y = 3u
    if (x < y) return "f7"
    if (x / y != 1u) return "f8"
    if (x % y != 2u) return "f9"
    if (0u.dec() < x) return "f10"
    if (10uL / 4uL != 2uL) return "f11"
    if (10uL % 4uL != 2uL) return "f12"
    if (10uL < 4uL) return "f13"
    if (0u.dec().toString() != "4294967295") return "f14"
    if ("${0u.dec()}!" != "4294967295!") return "f15"
    if (0uL.dec().toString() != "18446744073709551615") return "f16"
    val any: Any = 5u
    if (any is Int) return "f18"
    if (any.toString() != "5") return "f19"
    var rs = 0u
    for (u in 1u..6u) rs += u
    if (rs != 21u) return "f22"
    var cnt = 0
    for (u in 0u..<4u) cnt++
    if (cnt != 4) return "f23"
    return "OK"
}
"#,
    ),
    (
        "CompanionConst",
        r#"
const val M = Int.MIN_VALUE
fun box(): String {
    if (Int.MAX_VALUE != 2147483647) return "f1"
    if (Int.MIN_VALUE != -2147483648) return "f2"
    if (Long.MAX_VALUE != 9223372036854775807L) return "f3"
    if (Byte.MAX_VALUE.toInt() != 127) return "f4"
    if (Short.MIN_VALUE.toInt() != -32768) return "f5"
    if (Int.MAX_VALUE * 2L + 1 != 4294967295L) return "f6"
    if (UInt.MIN_VALUE != 0u) return "f6a"
    if (UInt.MAX_VALUE.toLong() != 4294967295L) return "f6b"
    if (9223372036854775808uL.toString() != "9223372036854775808") return "f6c"
    // non-overflowing loops at the type boundary
    var c1 = 0
    for (i in M downTo M) c1++
    if (c1 != 1) return "f7: $c1"
    var c2 = 0
    for (i in (Int.MAX_VALUE - 2)..Int.MAX_VALUE) c2++
    if (c2 != 3) return "f8: $c2"
    return "OK"
}
"#,
    ),
    // A `let`/`also` body containing a branch (`if`/`when`) can't go through the branchless inline
    // splice — it falls back to the per-function desugar, which lowers the branchy body normally.
    (
        "ScopeFnsBranchy",
        r#"
fun box(): String {
    val a = 5.let { if (it > 3) "big" else "small" }
    if (a != "big") return "f1:$a"
    val b = 2.let { when { it > 3 -> "x"; else -> "y" } }
    if (b != "y") return "f2:$b"
    var c = ""
    "z".also { c = if (it == "z") "yes" else "no" }
    if (c != "yes") return "f3:$c"
    return "OK"
}
"#,
    ),
    // LOOP hosts `forEach`/`map`/`fold` with a (branchless) lambda body: the host iterates, so its body
    // has a back-edge and is INLINED (iterator/hasNext/next loop spliced in — no `invokestatic
    // CollectionsKt.map`). The universal splicer relocates the host's loop StackMapTable frames around the
    // spliced lambda body. (A branchy lambda body inside a loop still falls back to a real call.)
    (
        "LoopInline",
        r#"
fun box(): String {
    var s = 0
    listOf(1, 2, 3).forEach { s += it }
    if (s != 6) return "f0: $s"
    val m = listOf(1, 2, 3).map { it * 10 }
    if (m != listOf(10, 20, 30)) return "f1: $m"
    val t = listOf("a", "b", "c").fold("") { acc, x -> acc + x }
    if (t != "abc") return "f2: $t"
    return "OK"
}
"#,
    ),
    // REIFIED type parameters on inline functions: `is T`/`as T`/`T::class` are specialized to the call's
    // actual type argument when the inline body is expanded (kotlinc's reified inlining). A same-module
    // inline fn `<reified T>` is expanded by the IR inliner with T bound to the call type argument.
    (
        "ReifiedInline",
        r#"
inline fun <reified T> isT(x: Any): Boolean = x is T
inline fun <reified T> asT(x: Any): T = x as T
inline fun <reified T> countOf(xs: List<Any>): Int = xs.count { it is T }
inline fun <reified T> pair(a: T, b: T): Array<T> = Array<T>(2) { if (it == 0) a else b }
fun box(): String {
    if (!isT<String>("hi")) return "f0"
    if (isT<Int>("hi")) return "f1"
    if (!isT<Number>(42)) return "f2"
    val s: String = asT<String>("hello")
    if (s != "hello") return "f3: $s"
    val c = countOf<String>(listOf("a", 1, "b", 2, "c"))
    if (c != 3) return "f4: $c"
    // Reified array element: `Array<T>(n){…}` allocates `new T[]` (a real String[]), not Object[].
    val arr: Array<String> = pair<String>("p", "q")
    if (arr.size != 2 || arr[0] != "p" || arr[1] != "q") return "f5"
    return "OK"
}
"#,
    ),
    // User-defined `inline fun` EXTENSIONS (`inline fun T.foo()`): the receiver is bound as `this` and
    // the body expanded at the call site (kotlinc's inliner), so a mutable capture / reified / non-local
    // return through an extension works without a real call.
    (
        "InlineExtension",
        r#"
inline fun Int.doubled(): Int = this * 2
inline fun String.shout(): String = this + "!"
inline fun Int.clampPos(): Int { if (this < 0) return 0; return this }
inline fun <T> T.echo(): T = this
inline fun String.withLen(f: (String) -> Int): Int = f(this)
inline fun <T> T.alsoLen(f: (T) -> Int): Int = f(this)
fun box(): String {
    if (5.doubled() != 10) return "f0: ${5.doubled()}"
    if ("hi".shout() != "hi!") return "f1"
    if ((-3).clampPos() != 0) return "f2"
    if (7.clampPos() != 7) return "f3"
    // Generic-receiver extension: `<T> T.echo()` binds the receiver as `this`, specialized to the
    // actual type — `String` here, not the erased `Any`.
    val s: String = "ok".echo()
    if (s != "ok") return "f4: $s"
    if (42.echo() != 42) return "f5"
    // Extension with a lambda parameter (concrete + generic receiver): the lambda's `it` types as the
    // actual receiver type so `it.length` resolves; the lambda body is inlined at the `f(this)` site.
    if ("abcd".withLen { it.length } != 4) return "f6"
    if ("hello".alsoLen { it.length } != 5) return "f7"
    return "OK"
}
"#,
    ),
    // String indexing `s[i]` → `charAt(i): Char` (java.lang.String has no `get` member), including
    // `this[0]` inside an inline extension on `String`.
    (
        "StringIndex",
        r#"
inline fun String.firstChar(): Char = this[0]
fun box(): String {
    if ("hello"[1] != 'e') return "f0"
    if ("hi".firstChar() != 'h') return "f1"
    var s = ""
    for (i in 0 until "abc".length) s += "abc"[i]
    if (s != "abc") return "f2: $s"
    return "OK"
}
"#,
    ),
    // `arrayOfNulls<T>(n)` (incl. reified) + primitive-element collection boxing: `ArrayList<Byte>().add(0)`
    // must box the `Int` literal as the element wrapper (`Byte`/`Long`), not `Integer`, or iterating the
    // element (`checkcast Byte`) throws.
    (
        "ArrayOfNullsAndPrimColl",
        r#"
inline fun <reified T> nulls(n: Int): Array<T?> = arrayOfNulls<T>(n)
fun box(): String {
    val a = arrayOfNulls<String>(3)
    a[0] = "x"
    if (a.size != 3 || a[0] != "x" || a[1] != null) return "f0"
    val r = nulls<String>(2)
    r[0] = "z"
    if (r.size != 2 || r[0] != "z") return "f1"
    val bs = ArrayList<Byte>()
    bs.add(0); bs.add(0); bs.add(0)
    var cb = 0
    for (i in bs) cb++
    if (cb != 3) return "f2"
    val ls = ArrayList<Long>()
    ls.add(0); ls.add(0)
    var cl = 0L
    for (i in ls) cl += i
    if (cl != 0L) return "f3"
    return "OK"
}
"#,
    ),
    // Inline functions whose body has `return` (early/conditional/loop returns). The IR inliner wraps
    // the body in `do { … } while(false)` and rewrites each `return x` to `result = x; break@end`, so the
    // function return becomes a jump to the body's end — including a `return` out of a `for` loop.
    (
        "InlineNonLocalReturn",
        r#"
inline fun classify(x: Int): String {
    if (x < 0) return "neg"
    if (x == 0) return "zero"
    return "pos"
}
inline fun firstEven(xs: List<Int>): Int {
    for (x in xs) { if (x % 2 == 0) return x }
    return -1
}
inline fun shout(s: String, loud: Boolean) {
    if (!loud) return
    println(s)
}
fun box(): String {
    if (classify(-5) != "neg") return "f0"
    if (classify(0) != "zero") return "f1"
    if (classify(7) != "pos") return "f2"
    if (firstEven(listOf(1, 3, 4, 5)) != 4) return "f3"
    if (firstEven(listOf(1, 3, 5)) != -1) return "f4"
    shout("x", false)
    return "OK"
}
"#,
    ),
    // Inline functions with EXCEPTION HANDLERS (try/catch/finally): `synchronized` (monitorenter; try
    // { block } finally { monitorexit }), `run`/`let` are handler-free, but `synchronized`/`runCatching`/
    // `use` carry an exception table. The splicer relocates each entry's byte offsets + catch_type into
    // the caller (handler frames are already StackMapTable targets) instead of bailing to a real call.
    (
        "InlineWithHandlers",
        r#"
fun box(): String {
    val lock = Any()
    val r = synchronized(lock) { 21 + 21 }
    if (r != 42) return "f0: $r"
    var acc = 0
    synchronized(lock) {
        for (i in 1..3) acc += i
    }
    if (acc != 6) return "f1: $acc"
    return "OK"
}
"#,
    ),
    // A frame-recording inline HOF call used as a NON-FIRST operand — i.e. at a NON-EMPTY caller operand
    // baseline (a dispatch receiver / earlier argument already on the stack). `records_frame` must report
    // the inline splice so the parent operand sequence spills the earlier operands to temps, letting the
    // splice land at an empty baseline instead of bailing to a real `invokestatic CollectionsKt.*` call.
    (
        "InlineHofNonEmptyBaseline",
        r#"
fun makePair(k: String, v: List<Int>): String = "$k=$v"
fun box(): String {
    // map result is the 2nd arg → `k`-temp is live on the stack across the branchy-lambda splice.
    val a = makePair("k", listOf(-1, 2, -3).map { if (it > 0) it else -it })
    if (a != "k=[1, 2, 3]") return "f0: $a"
    // filter result reached through a receiver chain with a StringBuilder already on the stack.
    val sb = StringBuilder()
    sb.append("r=")
    sb.append(listOf(1, 2, 3, 4).filter { it % 2 == 0 }.toString())
    if (sb.toString() != "r=[2, 4]") return "f1: $sb"
    return "OK"
}
"#,
    ),
    // `takeIf`/`takeUnless`: BRANCHY host (returns receiver or null per the inlined predicate) with a
    // `Function1` predicate whose body is a COMPARISON — i.e. a branchy lambda body. Exercises the
    // universal splicer's full frame relocation: the host's StackMapTable frames AND the lambda body's own.
    // A LOOP host (`map`/`filter` builds a collection) whose lambda body is BRANCHY (a comparison/`if`):
    // `map` keeps the destination collection on the operand stack BELOW the lambda result, so the lambda
    // body's own StackMapTable frames must be rebased onto that `[Collection]` prefix (computed by the
    // splicer's forward operand-stack simulation). `forEach`/`fold` exercise the EMPTY-prefix branchy case.
    (
        "MapBranchy",
        r#"
fun box(): String {
    val m = listOf(-1, 2, -3).map { if (it > 0) it else -it }
    if (m != listOf(1, 2, 3)) return "f0: $m"
    val f = listOf(1, 2, 3, 4).filter { it % 2 == 0 }
    if (f != listOf(2, 4)) return "f1: $f"
    var s = 0
    listOf(1, 2, 3, 4).forEach { if (it % 2 == 0) s += it }
    if (s != 6) return "f2: $s"
    val t = listOf(1, 2, 3).fold(0) { acc, x -> if (x > 1) acc + x else acc }
    if (t != 5) return "f3: $t"
    return "OK"
}
"#,
    ),
    (
        "TakeIf",
        r#"
fun box(): String {
    val a = "hi".takeIf { it.length == 2 }
    if (a != "hi") return "f0: $a"
    val b = "hi".takeIf { it.length == 9 }
    if (b != null) return "f1"
    val c = "yo".takeUnless { it.length == 0 }
    if (c != "yo") return "f2"
    val d = "x".takeUnless { it.length == 1 }
    if (d != null) return "f3"
    return "OK"
}
"#,
    ),
    (
        "ScopeFns",
        r#"
fun box(): String {
    val r = "abc".let { it.length }
    if (r != 3) return "f1: $r"
    var s = 0
    3.let { s += it }
    if (s != 3) return "f2: $s"
    val a = "x".also { s += 5 }
    if (s != 8 || a != "x") return "f3: $s,$a"
    val chain = 5.let { it * 2 }.let { it + 1 }
    if (chain != 11) return "f4: $chain"
    return "OK"
}
"#,
    ),
    (
        "ArrayOfRef",
        r#"
fun box(): String {
    val a = arrayOf("O", "K")
    if (a[0] + a[1] != "OK") return "f1"
    val b = arrayOf("x", "y", "z")
    var s = ""
    for (e in b) s += e
    if (s != "xyz") return "f2: $s"
    if (b.size != 3) return "f3"
    return "OK"
}
"#,
    ),
    // An `inner class` captures the enclosing instance (a synthetic `this$0` field); it is constructed
    // as `outerInstance.Inner(args)` → `new Outer$Inner(outer, args)`.
    (
        "InnerClassCtor",
        r#"
class Outer(val tag: Int) {
    inner class Inner(val n: Int) {
        fun describe(): Int = n
    }
}
fun box(): String {
    val o = Outer(7)
    val i = o.Inner(42)
    if (i.describe() != 42) return "f1"
    if (i.n != 42) return "f2"
    return "OK"
}
"#,
    ),
    // An inner method reads the enclosing instance's members through `this$0` (via the getter); an
    // inner property initializer can combine outer + own members (`val z = x + y`).
    (
        "InnerOuterAccess",
        r#"
class Outer(val x: String) {
    fun shout(): String = x + "!"
    inner class Inner(val y: String) {
        val z = x + y
        fun outer(): String = x
        fun callOuter(): String = shout()
    }
}
fun box(): String {
    val i = Outer("O").Inner("K")
    if (i.z != "OK") return "f1:${i.z}"
    if (i.outer() != "O") return "f2"
    if (i.callOuter() != "O!") return "f3:${i.callOuter()}"
    return "OK"
}
"#,
    ),
    // A `var` captured (even read-only) by a closure but reassigned later in the enclosing scope is
    // boxed, so the closure observes the update (kotlinc's captured-var semantics; KT-4656 style).
    (
        "CaptureReassigned",
        r#"
fun eval(f: () -> Int): Int = f()
fun box(): String {
    var n = 1
    val r1 = eval { n }
    n = 5
    fun getN() = n
    val r2 = eval { n }
    if (r1 != 1) return "f1:$r1"
    if (r2 != 5) return "f2:$r2"
    if (getN() != 5) return "f3"
    return "OK"
}
"#,
    ),
    // A capturing local function is lifted with its captured locals prepended as parameters: a `val`
    // is passed by value, a `var` it writes is boxed into a shared `Ref` holder (so the mutation is
    // visible to the enclosing scope).
    (
        "LocalFunCapture",
        r#"
fun box(): String {
    val base = 100
    fun add(x: Int) = base + x
    if (add(5) != 105) return "f1"
    var acc = 0
    fun bump(x: Int) { acc = acc + x }
    bump(3); bump(4)
    if (acc != 7) return "f2:$acc"
    return "OK"
}
"#,
    ),
    // A non-capturing local function is lifted to a private static method on the facade; calls route
    // to it. Recursion and multiple local functions in one body are supported.
    (
        "LocalFun",
        r#"
fun box(): String {
    fun dbl(x: Int) = x * 2
    fun fib(n: Int): Int {
        if (n < 2) return n
        return fib(n - 1) + fib(n - 2)
    }
    if (dbl(21) != 42) return "f1"
    if (fib(7) != 13) return "f2"
    return "OK"
}
"#,
    ),
    // A mutable local captured and written by a non-inlined lambda (a closure) is boxed into a
    // `kotlin/jvm/internal/Ref$XxxRef` so the closure and the enclosing scope share the cell.
    (
        "MutableCapture",
        r#"
fun twice(f: () -> Unit) { f(); f() }
fun call(f: () -> Int): Int = f()
fun box(): String {
    var sum = 0
    twice { sum += 1 }
    twice { sum += 10 }
    if (sum != 22) return "f1:$sum"
    var s = "a"
    twice { s = s + "b" }
    if (s != "abb") return "f2:$s"
    var x = 10
    val r = call { x = x * 2; x }
    if (x != 20 || r != 20) return "f3"
    var c = 0
    twice { c++ }
    twice { c-- ; c-- }
    if (c != -2) return "f4:$c"
    return "OK"
}
"#,
    ),
    // Unbound member property reference `A::x` — a synthesized `PropertyReference1Impl` singleton;
    // `.get(receiver)` reads the property via its getter, `.name` is the property name.
    (
        "PropertyRef",
        r#"
class A(val x: Int)
fun box(): String {
    val p = A::x
    if (p.get(A(42)) != 42) return "f1"
    if (p.get(A(-1)) != -1) return "f2"
    if (p.name != "x") return "f3"
    return "OK"
}
"#,
    ),
    // Nullable primitives (`Int?`/`Char?`/…) are their boxed wrappers (`java/lang/Integer`); `!!`
    // unboxes to the primitive after a null check, and a primitive boxes into a nullable slot.
    (
        "NullablePrimitive",
        r#"
fun foo(): Int? = 42
fun box(): String {
    val a: Int? = 5
    if (a!! + 1 != 6) return "f1"
    if (foo()!! > 239) return "f2"
    val c: Char? = 'a'
    if (c!! >= 'b') return "f3"
    var n: Int? = null
    if (n != null) return "f4"
    val r = try { n!!.toString() } catch (e: NullPointerException) { "OK" }
    if (r != "OK") return "f5"
    return "OK"
}
"#,
    ),
    // Null-check smart-cast of a nullable primitive: after `if (t != null)`, `t` narrows to its
    // unboxed primitive, so it can be used in arithmetic directly.
    (
        "NullableSmartCast",
        r#"
fun box(): String {
    val t: Int? = 7
    if (t != null) {
        if (t + 1 != 8) return "f1"
    } else {
        return "f2"
    }
    val u: Int? = null
    if (u == null) return "OK"
    return "f3"
}
"#,
    ),
    // Elvis on a nullable primitive (`Int? ?: 0`): the result is the unboxed primitive — the non-null
    // lhs unboxes and the default coerces to it.
    (
        "NullableElvis",
        r#"
fun box(): String {
    val a: Int? = 5
    if ((a ?: 9) != 5) return "f1"
    val n: Int? = null
    if ((n ?: 9) != 9) return "f2"
    if ((a ?: 0) + 10 != 15) return "f3"
    return "OK"
}
"#,
    ),
    // A nullable primitive compares with a primitive (`a == 5`): the primitive is boxed for structural
    // equality. A generic constructor with a primitive type argument coerces a literal to that type.
    (
        "NullableEqAndGenericCtor",
        r#"
class Box<T>(val value: T)
fun box(): String {
    val a: Int? = 5
    if (a != 5) return "f1"
    if (a == 6) return "f2"
    val n: Int? = null
    if (n == 5) return "f3"
    val b = Box<Long>(-1)
    val v: Long = b.value
    if (v != -1L) return "f4:$v"
    return "OK"
}
"#,
    ),
    // A nullable-primitive `==`/`!=` a primitive short-circuits like kotlinc: when the wrapper is null
    // the primitive side is NOT evaluated (so `sideEffecting()` runs once, not twice). A `?.` on a null
    // receiver yields null → the comparison resolves without touching the RHS.
    (
        "NullableEqShortCircuit",
        r#"
var result = ""
fun se(): Int { result += "X"; return 123 }
class C(val x: Int)
val a: C? = C(123)
val b: C? = null
fun box(): String {
    if (a?.x != se()) return "f1"
    if (b?.x == se()) return "f2"
    return if (result == "X") "OK" else "f3:$result"
}
"#,
    ),
    // Bound property reference `obj::x` — a `PropertyReference0Impl` carrying the captured receiver;
    // `.get()` (no args) reads `this.receiver`'s property.
    (
        "BoundPropertyRef",
        r#"
class A(val x: Int)
fun box(): String {
    val a = A(7)
    val p = a::x
    if (p.get() != 7) return "f1"
    if (p.name != "x") return "f2"
    return "OK"
}
"#,
    ),
    // The literal `-2147483648` is `Int.MIN_VALUE` (an Int), not a Long — usable as an Int `when`
    // branch and in an Int context (the bare `2147483648` overflows Int and is a Long).
    (
        "IntMinLiteral",
        r#"
fun cls(x: Int): String = when (x) {
    2147483647 -> "MAX"
    -2147483648 -> "MIN"
    else -> "other"
}
fun box(): String {
    val i: Int = -2147483648
    if (i != -2147483648) return "f1"
    if (cls(-2147483648) != "MIN") return "f2"
    if (cls(2147483647) != "MAX") return "f3"
    return "OK"
}
"#,
    ),
    // Method references: bound `obj::m` (receiver captured) and unbound `Type::m` (receiver is the
    // first argument) — each a closure over a synthesized `(receiver, args) -> receiver.m(args)`.
    (
        "MethodRef",
        r#"
class C(val p: String) {
    fun get(): String = p
    fun plus(x: String): String = p + x
}
fun box(): String {
    val c = C("OK")
    val bound = c::get
    if (bound() != "OK") return "f1"
    val unbound = C::plus
    if (unbound(C("A"), "B") != "AB") return "f2"
    return "OK"
}
"#,
    ),
    // A `Unit`-returning function reference `::add` wraps the call and returns the Unit singleton
    // (a direct method handle would adapt `void` to `null`, breaking a `FunctionN` consumer).
    (
        "UnitFunRef",
        r#"
val sb = StringBuilder()
fun add(s: String) { sb.append(s) }
fun apply2(f: (String) -> Unit) { f("O"); f("K") }
fun box(): String {
    apply2(::add)
    return sb.toString()
}
"#,
    ),
    // Constructor reference `::A` — a closure wrapping `new A(args)`, usable as a `FunctionN` value.
    (
        "CtorRef",
        r#"
class A(val result: String)
class P(val x: Int, val y: Int)
fun box(): String {
    val f = ::A
    if (f("OK").result != "OK") return "f1"
    val g = ::P
    val p = g(3, 4)
    if (p.x != 3 || p.y != 4) return "f2"
    return "OK"
}
"#,
    ),
    // Enum entries with a body: each bodied entry is a synthesized subclass (`Op$ADD extends Op`)
    // overriding an abstract member; the override can read an enum constructor `val`.
    (
        "EnumEntryBody",
        r#"
enum class Op(val sym: String) {
    ADD("+") { override fun apply(a: Int, b: Int) = a + b },
    MUL("*") { override fun apply(a: Int, b: Int) = a * b };
    abstract fun apply(a: Int, b: Int): Int
}
fun box(): String {
    if (Op.ADD.apply(2, 3) != 5) return "f1"
    if (Op.MUL.apply(2, 3) != 6) return "f2"
    if (Op.ADD.sym != "+") return "f3"
    if (Op.MUL.sym != "*") return "f4"
    return "OK"
}
"#,
    ),
    // The overridable members compareTo/equals/hashCode have Kotlin-contract return types (Int/Boolean/
    // Int), used when the body can't be inferred locally (`compareTo(o) = v - o.v` references `o`).
    (
        "CompareToContract",
        r#"
class N(val v: Int) {
    operator fun compareTo(o: N) = v - o.v
}
fun box(): String {
    if (!(N(3) < N(5))) return "f1"
    if (!(N(7) > N(2))) return "f2"
    if (!(N(4) <= N(4))) return "f3"
    return "OK"
}
"#,
    ),
    // Class member operators: `a + b` → `a.plus(b)` (and minus/times/div/rem); `a < b` →
    // `a.compareTo(b) < 0`.
    (
        "ClassOperators",
        r#"
class V(val x: Int) {
    operator fun plus(o: V) = V(x + o.x)
    operator fun minus(o: V) = V(x - o.x)
    operator fun times(o: V) = V(x * o.x)
    operator fun compareTo(o: V): Int = x - o.x
}
fun box(): String {
    if ((V(1) + V(2)).x != 3) return "f1"
    if ((V(7) - V(3)).x != 4) return "f2"
    if ((V(4) * V(5)).x != 20) return "f3"
    if (!(V(2) < V(5))) return "f4"
    if (!(V(9) >= V(9))) return "f5"
    if ((V(1) + V(2) + V(3)).x != 6) return "f6"
    return "OK"
}
"#,
    ),
    // An expression-bodied extension function with no explicit return type infers it from the body,
    // with `this` bound to the receiver (`fun Int.double() = this * 2` → return Int).
    (
        "ExtThisInfer",
        r#"
fun Int.double() = this * 2
fun String.shout() = this + "!"
fun Int.isPos() = this > 0
fun box(): String {
    if (5.double() != 10) return "f1"
    if ("hi".shout() != "hi!") return "f2"
    if (!4.isPos()) return "f3"
    return "OK"
}
"#,
    ),
    // A deferred `val` (declared with a type, no initializer) is assigned exactly once in an `init`
    // block — a real backing field initialized in the constructor body.
    (
        "DeferredValInit",
        r#"
class C(x: Int) {
    val a: Int
    val b: Int
    init {
        a = x
        b = x + 1
    }
}
fun box(): String {
    val o = C(5)
    if (o.a != 5) return "f1"
    if (o.b != 6) return "f2"
    return "OK"
}
"#,
    ),
    // A non-`val`/`var` primary-constructor parameter is an argument only (no field), available in the
    // constructor body for property initializers and `init` blocks — including interleaved with `val`s.
    (
        "NonPropertyCtorParam",
        r#"
class A(x: Int) { val y = x * 2 }
class B(val a: Int, b: Int, val c: Int) { val sum = a + b + c }
class C(x: Int) { var z = 0; init { z = x + 10 } }
class D(name: String) { val greeting = "Hi " + name }
fun box(): String {
    if (A(3).y != 6) return "f1"
    val b = B(1, 2, 3)
    if (b.a != 1 || b.c != 3 || b.sum != 6) return "f2"
    if (C(5).z != 15) return "f3"
    if (D("Bob").greeting != "Hi Bob") return "f4"
    return "OK"
}
"#,
    ),
    // A body property's type is inferred from its initializer with the preceding properties (and
    // val/var ctor params) in scope: `val b = a + 1` sees the earlier `a`.
    (
        "SequentialPropInfer",
        r#"
class C(val x: Int) {
    val a = 10
    val b = a + 1
    val c = b * x
}
fun box(): String {
    val o = C(2)
    if (o.a != 10) return "f1"
    if (o.b != 11) return "f2"
    if (o.c != 22) return "f3:${o.c}"
    return "OK"
}
"#,
    ),
    // A private @InlineOnly String extension (`uppercase`/`lowercase` → `toUpperCase(Locale.ROOT)`) is
    // inlined from its real stdlib bytecode (it has no callable body and no JDK member equivalent).
    (
        "StringInlineExt",
        r#"
fun box(): String {
    if ("ab".uppercase() != "AB") return "f1"
    if ("AB".lowercase() != "ab") return "f2"
    if (" Ab ".trim().uppercase() != "AB") return "f3"
    return "OK"
}
"#,
    ),
    // String members resolve to their java.lang.String JVM methods (a member wins over a same-named
    // private @InlineOnly extension like StringsKt.isEmpty).
    (
        "StringMembers",
        r#"
fun box(): String {
    if ("abc".isEmpty()) return "f1"
    if (!"".isEmpty()) return "f2"
    if (!"abc".startsWith("ab")) return "f3"
    if ("abc".indexOf("b") != 1) return "f4"
    return "OK"
}
"#,
    ),
    // A data class ALWAYS generates equals/hashCode/toString over an OPEN base member (KT-6206), but
    // INHERITS a `final` base member (can't override it).
    (
        "DataClassOverBase",
        r#"
abstract class Open { override fun toString() = "base" }
data class D1(val f: String) : Open()
abstract class Final { final override fun toString() = "kept" }
data class D2(val f: String) : Final()
fun box(): String {
    if (D1("x").toString() != "D1(f=x)") return "f1:${D1("x")}"
    if (D2("x").toString() != "kept") return "f2:${D2("x")}"
    return "OK"
}
"#,
    ),
    // Kotlin's built-in collection mapped members: `Map.keys`/`entries` resolve to the JVM
    // `keySet()`/`entrySet()` (Map.values/size keep their JVM name and already worked).
    (
        "MapMappedMembers",
        r#"
fun box(): String {
    val m = mapOf(1 to "a", 2 to "b", 3 to "c")
    if (m.keys.size != 3) return "f1"
    if (m.entries.size != 3) return "f2"
    if (m.values.size != 3) return "f3"
    if (m.size != 3) return "f4"
    if (!m.keys.contains(2)) return "f5"
    return "OK"
}
"#,
    ),
    // Legal nested-scope variable shadowing: an inner block's `val x` shadows an outer `x` (each gets
    // its own slot; the outer is restored at block exit). Same-scope redeclaration is still an error.
    (
        "Shadowing",
        r#"
fun box(): String {
    val x = 1
    var sum = 0
    for (i in 0..2) {
        val x = i * 10
        sum += x
    }
    if (sum != 30) return "f1:$sum"
    if (x != 1) return "f2:$x"
    if (true) {
        val x = "abc"
        if (x.length != 3) return "f3"
    }
    return if (x == 1) "OK" else "f4:$x"
}
"#,
    ),
    // Nested try/catch (without a finally in the nest) compiles and runs; only a nested-try combined
    // with a finally is rejected (skip), never miscompiled.
    (
        "NestedTry",
        r#"
fun box(): String {
    var r = ""
    try {
        try {
            r += "a"
            throw RuntimeException("x")
        } catch (e: RuntimeException) {
            r += "b"
        }
        r += "c"
    } catch (e: Exception) {
        r += "z"
    }
    return if (r == "abc") "OK" else "F:$r"
}
"#,
    ),
    // A bare-value lambda types its parameters from its own annotations (`{ x: Int -> … }`), even with
    // no expected function type — so the body and a direct call both check correctly.
    (
        "LambdaParamType",
        r#"
fun box(): String {
    val dbl = { x: Int -> x * 2 }
    if (dbl(3) != 6) return "f1"
    val add = { a: Int, b: Int -> a + b }
    if (add(2, 5) != 7) return "f2"
    val len = { s: String -> s.length }
    if (len("abcd") != 4) return "f3"
    return "OK"
}
"#,
    ),
    // Labeled loops: `break@label`/`continue@label` target the named enclosing loop.
    (
        "LabeledLoops",
        r#"
fun box(): String {
    var s = 0
    outer@ for (i in 0 until 3) {
        for (j in 0 until 3) {
            if (j == 2) continue@outer
            if (i == 2) break@outer
            s += 1
        }
    }
    if (s != 4) return "f1:$s"
    var t = 0
    loop@ for (x in listOf(1, 2, 3, 4)) {
        if (x == 3) break@loop
        t += x
    }
    if (t != 3) return "f2:$t"
    return "OK"
}
"#,
    ),
    // The array creators are intrinsics keyed on the resolved stdlib symbol: a user function of the
    // same name must shadow the intrinsic (as in kotlinc), not be silently lowered to an array.
    (
        "ArrayOfUserShadow",
        r#"
fun arrayOf(a: String): String = "user:$a"
fun box(): String {
    val r = arrayOf("x")
    return if (r == "user:x") "OK" else "F:$r"
}
"#,
    ),
    (
        "RepeatInline",
        r#"
fun box(): String {
    var s = 0
    repeat(4) { s += it }
    if (s != 6) return "f1: $s"
    var c = 0
    repeat(5) { c++ }
    if (c != 5) return "f2: $c"
    val sb = StringBuilder()
    repeat(3) { sb.append("x") }
    if (sb.toString() != "xxx") return "f3"
    return "OK"
}
"#,
    ),
    (
        "ForeachInline",
        r#"
fun box(): String {
    var s = 0
    listOf(1, 2, 3, 4).forEach { s += it }
    if (s != 10) return "f1: $s"
    var p = 1
    setOf(2, 3, 5).forEach { p *= it }
    if (p != 30) return "f2: $p"
    val sb = StringBuilder()
    listOf("a", "b", "c").forEach { sb.append(it) }
    if (sb.toString() != "abc") return "f3: $sb"
    var w = 0
    listOf(10, 20, 30).forEachIndexed { i, x -> w += (i + 1) * x }
    if (w != 140) return "f4: $w"
    // array + String forEach (inlined index loop) with mutable capture
    var asum = 0
    intArrayOf(1, 2, 3, 4).forEach { asum += it }
    if (asum != 10) return "f5: $asum"
    var csum = 0
    "abc".forEach { csum += it.code }
    if (csum != 'a'.code + 'b'.code + 'c'.code) return "f6: $csum"
    return "OK"
}
"#,
    ),
    (
        "MapIndexed",
        r#"
fun box(): String {
    val r = listOf(10, 20, 30).mapIndexed { i, x -> i * x + 1 }
    if (r != listOf(1, 21, 61)) return "f1: $r"
    val r2 = listOf("a", "bb", "ccc").mapIndexed { i, s -> i + s.length }
    if (r2 != listOf(1, 3, 5)) return "f2: $r2"
    return "OK"
}
"#,
    ),
    (
        "IncDec",
        r#"
fun ident(n: Int): Int = n
fun box(): String {
    var i = 5
    val a = i++
    if (a != 5 || i != 6) return "f1"
    val b = ++i
    if (b != 7 || i != 7) return "f2"
    var j = 3
    if (j-- != 3 || j != 2) return "f3"
    if (--j != 1 || j != 1) return "f4"
    var k = 0
    if ((k++) + (k++) != 1 || k != 2) return "f5"
    var m = 3
    if (ident(m--) != 3 || m != 2) return "f6"
    var t = 0
    if ("${t++}x" != "0x" || t != 1) return "f7"
    var w = 0
    when (w++) { 0 -> {} else -> {} }
    if (w != 1) return "f8"
    var n = 0; n++; ++n
    if (n != 2) return "f9"
    var by1: Byte = 127; by1++
    if (by1.toInt() != -128) return "f10"
    var by2: Byte = 127
    val ob = by2++
    if (ob.toInt() != 127 || by2.toInt() != -128) return "f11"
    var sh: Short = 32767; sh++
    if (sh.toInt() != -32768) return "f12"
    var ch = 'a'
    val oc = ch++
    if (oc != 'a' || ch != 'b') return "f13"
    return "OK"
}
"#,
    ),
    (
        "UserInline",
        r#"
inline fun twice(block: () -> Unit) { block(); block() }
inline fun applyN(n: Int, block: (Int) -> Unit) { var i = 0; while (i < n) { block(i); i++ } }
inline fun pick(c: Boolean, a: () -> Int, b: () -> Int): Int = if (c) a() else b()
fun box(): String {
    var s = 0
    twice { s += 3 }
    if (s != 6) return "f1: $s"
    var acc = 0
    applyN(4) { acc += it }
    if (acc != 6) return "f2: $acc"
    val r = pick(true, { 10 }, { 20 })
    if (r != 10) return "f3: $r"
    // nested inline calls + mutable capture across both
    var t = 0
    twice { applyN(3) { t += it } }
    if (t != 6) return "f4: $t"
    return "OK"
}
"#,
    ),
    (
        "RangeValue",
        r#"
fun box(): String {
    val r = 0..3
    if (r.first != 0) return "f1"
    if (r.last != 3) return "f2"
    var s = 0
    for (x in r) s += x
    if (s != 6) return "f3"
    if ((1..<4).last != 3) return "f4"
    val lr = 10L..12L
    if (lr.last != 12L) return "f5a"
    var lo = 0L
    for (y in lr) lo += y
    if (lo != 33L) return "f5"
    var t = 0
    for (z in 5..7) t += z
    if (t != 18) return "f6"
    var cs = 0
    for (c in 'a'..'e') cs += c.code
    if (cs != 'a'.code + 'b'.code + 'c'.code + 'd'.code + 'e'.code) return "f7"
    var lt = 0L
    for (y in 1L..4L) lt += y
    if (lt != 10L) return "f8"
    return "OK"
}
"#,
    ),
    // A property reference is a function value: `C::n` (a `KProperty1`) is a `(C)->Int`, usable where a
    // `Function1` is expected — stored in a function-typed local and invoked, passed to a user
    // higher-order function, and passed to a stdlib `Iterable.map` (a `Function1` parameter).
    (
        "PropertyRefFn",
        r#"
class C(val n: Int)
fun apply1(f: (C) -> Int, c: C): Int = f(c)
fun box(): String {
    val g: (C) -> Int = C::n
    if (g(C(7)) != 7) return "f1"
    if (apply1(C::n, C(42)) != 42) return "f2"
    val xs = listOf(C(5), C(9)).map(C::n)
    if (xs[0] != 5 || xs[1] != 9) return "f3"
    return "OK"
}
"#,
    ),
    // Integer-family `rangeTo` widening: `Byte..Byte`/`Short..Short` build an `IntRange`; a `Long` operand
    // makes a `LongRange`. Plus `listOf<Long>` literal adaptation (the int literals box as `Long`) and the
    // overflow-safe counted loop for a range ending at `Int.MAX_VALUE` (must not wrap and spin).
    (
        "RangeWidenAndVararg",
        r#"
fun box(): String {
    val b1: Byte = 1; val b5: Byte = 5
    val rb = b1..b5
    var sb = 0
    for (x in rb) sb += x
    if (sb != 15) return "fb"

    val s1: Short = 2; val s4: Short = 4
    val rs = s1..s4
    var ss = 0
    for (x in rs) ss += x
    if (ss != 9) return "fs"

    val rl = 3L..6L
    var sl = 0L
    for (y in rl) sl += y
    if (sl != 18L) return "fl"
    if (listOf<Long>(3, 4, 5, 6) != listOf<Long>(3L, 4L, 5L, 6L)) return "fv"

    val rmax = (Int.MAX_VALUE - 2)..Int.MAX_VALUE
    var cnt = 0
    for (i in rmax) { cnt++; if (cnt > 10) return "foverflow" }
    if (cnt != 3) return "fc"
    return "OK"
}
"#,
    ),
    // A library operator function on a reference receiver: `collection + x` desugars to
    // `Collection.plus(x)` (a stdlib extension). Overload selection is most-specific: `+ element` uses the
    // `plus(T)` overload, `+ collection` the `plus(Iterable)` concat overload (not a nested element).
    (
        "CollectionPlus",
        r#"
fun box(): String {
    val a = listOf(1, 2) + 3
    if (a != listOf(1, 2, 3)) return "fa: $a"
    val l = listOf(10L) + 20L
    if (l != listOf(10L, 20L)) return "fl: $l"
    val s = setOf(1, 2) + 2
    if (s != setOf(1, 2)) return "fs: $s"
    val c = listOf(1, 2) + listOf(3, 4)
    if (c != listOf(1, 2, 3, 4)) return "fc: $c"
    return "OK"
}
"#,
    ),
    // Unsigned `in`-range membership (`x in a..b` for UInt/ULong) is the bounds-check intrinsic with
    // UNSIGNED comparison (`Integer/Long.compareUnsigned`), so values past the sign bit order correctly.
    (
        "UnsignedInRange",
        r#"
fun box(): String {
    if (2u !in 1u..3u) return "f1"
    if (5u in 1u..3u) return "f2"
    val big = 4000000000u
    if (big !in 1u..4000000001u) return "f3"
    if (big in 1u..3u) return "f4"
    if (10uL !in 0uL..100uL) return "f5"
    if (20000000000uL !in 0uL..30000000000uL) return "f6"
    return "OK"
}
"#,
    ),
    // Iterating an unsigned range *value* (`val r = 0u..5u; for (i in r)`): builds a `UIntRange`/
    // `ULongRange` (the ctor takes a synthetic marker) and iterates via the MANGLED inline-class getters
    // (`getFirst-pVg5ArA`, resolved from the classpath by prefix) with unsigned comparison.
    (
        "UnsignedRangeIterate",
        r#"
fun box(): String {
    val r = 0u..5u
    var s = 0
    for (i in r) s += i.toInt()
    if (s != 15) return "f1: $s"
    var t = 0uL
    val lr = 10uL..13uL
    for (j in lr) t += j
    if (t != 46uL) return "f2: $t"
    // a UInt range reaching past the signed-int sign bit must iterate in unsigned order
    var c = 0
    for (k in 4294967293u..4294967295u) c++
    if (c != 3) return "f3: $c"
    return "OK"
}
"#,
    ),
    // An `if`/`when` whose branches are a primitive and `null` joins to the boxed nullable wrapper
    // (`if (c) true else null` is `Boolean?`); the primitive branch is boxed at the merge.
    (
        "PrimitiveNullJoin",
        r#"
fun pick(c: Boolean): Boolean? = if (c) true else null
fun box(): String {
    if (pick(true) != true) return "f1"
    if (pick(false) != null) return "f2"
    val x: Int? = if (pick(true) == true) 5 else null
    if (x != 5) return "f3"
    val y: Char? = when (3) { 3 -> 'z'; else -> null }
    if (y != 'z') return "f4"
    return "OK"
}
"#,
    ),
    // `super.method(args)` → a non-virtual `invokespecial` to the base method, for a user base class
    // and for a classpath base (`super.toString()` reaching `Object`/an open stdlib method).
    (
        "SuperMethodCall",
        r#"
open class Base { open fun tag(s: String): String = "base:$s" }
class Derived : Base() { override fun tag(s: String): String = super.tag(s) + "+derived" }
open class Animal { override fun toString(): String = "animal" }
class Dog : Animal() { override fun toString(): String = super.toString() + "+dog" }
fun box(): String {
    if (Derived().tag("x") != "base:x+derived") return "f1: ${Derived().tag("x")}"
    if (Dog().toString() != "animal+dog") return "f2: ${Dog()}"
    return "OK"
}
"#,
    ),
    // Two `if`/`when` branches of the SAME class join to that class (erased type args): the runtime class
    // is identical so the merge frame is well-typed.
    (
        "SameClassJoin",
        r#"
fun box(): String {
    val a = listOf(1, 2)
    val b = listOf("x", "y", "z")
    val c = if (a.size > 5) a else b
    if (c.size != 3) return "f1: ${c.size}"
    val d = when (1) { 1 -> a; else -> b }
    if (d.size != 2) return "f2: ${d.size}"
    return "OK"
}
"#,
    ),
    // Two `if`/`when` branches of UNRELATED classes join to their common supertype, which krusty
    // approximates as `Any` (`Object`): the merge stack frame is `Object`, which every branch satisfies.
    (
        "UnrelatedRefJoin",
        r#"
open class P
class Foo : P() { fun who() = "foo" }
class Bar : P() { fun who() = "bar" }
fun box(): String {
    val x: Any = if (true) Foo() else Bar()
    if (x !is Foo) return "f1"
    val y: Any = if (false) "str" else Foo()
    if (y !is Foo) return "f2"
    val z: P = if (true) Foo() else Bar()
    if ((z as Foo).who() != "foo") return "f3"
    return "OK"
}
"#,
    ),
    // A property overriding a supertype property with a more specific (covariant) or generic-erased type
    // gets a synthetic `ACC_BRIDGE` getter returning the supertype's type, delegating to the concrete
    // getter — so a read through the supertype reference reaches the override.
    (
        "PropertyGetterBridge",
        r#"
interface Node
class NodeImpl(val tag: String) : Node
interface Edge { val from: Node }
class EdgeImpl(override val from: NodeImpl) : Edge
interface Box<T> { val item: T }
class StrBox(override val item: String) : Box<String>
fun box(): String {
    val e: Edge = EdgeImpl(NodeImpl("n"))
    if ((e.from as NodeImpl).tag != "n") return "f1"
    val b: Box<String> = StrBox("hi")
    if (b.item != "hi") return "f2"
    return "OK"
}
"#,
    ),
    // Bridges whose concrete member returns a PRIMITIVE: the synthetic `ACC_BRIDGE` boxes it to the
    // erased reference type — a generic property `val value: T` (erased `Object`) overridden `: Int`,
    // and a generic method `fun pick(): T` overridden returning `Int`.
    (
        "PrimitiveBridges",
        r#"
interface Holder<T> { val value: T; fun pick(): T }
class IntHolder(override val value: Int) : Holder<Int> { override fun pick(): Int = value + 1 }
fun box(): String {
    val h: Holder<Int> = IntHolder(41)
    if (h.value != 41) return "f1: ${h.value}"
    if (h.pick() != 42) return "f2: ${h.pick()}"
    return "OK"
}
"#,
    ),
    // `x as Int` on a reference operand is an unbox cast: `checkcast Integer; intValue()`. Works for each
    // primitive (a wrong dynamic type throws ClassCastException, like kotlinc).
    (
        "AsToPrimitive",
        r#"
fun box(): String {
    val a: Any = 42
    if ((a as Int) != 42) return "f1"
    val d: Any = 2.5
    if ((d as Double) != 2.5) return "f2"
    val c: Any = 'q'
    if ((c as Char) != 'q') return "f3"
    val l: Any = 99L
    if ((l as Long) != 99L) return "f4"
    val bo: Any = true
    if (!(bo as Boolean)) return "f5"
    val s: Any = "s"
    try { (s as Int); return "f6" } catch (e: ClassCastException) {}
    return "OK"
}
"#,
    ),
    // `ByteArray`/`ShortArray`/`FloatArray` size + init constructors (the checker had only Int/Long/
    // Double/Boolean/Char).
    (
        "MorePrimitiveArrays",
        r#"
fun box(): String {
    val b = ByteArray(3); b[0] = 5; b[1] = 7
    if (b[0] + b[1] != 12) return "fb"
    val s = ShortArray(2) { (it * 10).toShort() }
    if (s[1] != 10.toShort()) return "fs"
    val f = FloatArray(3) { it * 1.5f }
    if (f[2] != 3.0f) return "ff"
    return "OK"
}
"#,
    ),
    // A `data class` with an array property: `toString` renders content (`Arrays.toString`), but `equals`
    // and `hashCode` use array REFERENCE identity (kotlinc's actual behaviour — different instances with
    // the same content are NOT equal). The array field keeps its real type (`[I`), not erased to `Object`.
    (
        "DataClassArray",
        r#"
data class P(val v: IntArray, val s: String)
fun box(): String {
    val arr = intArrayOf(1, 2, 3)
    val a = P(arr, "x")
    if (a != P(arr, "x")) return "f1"             // same array instance -> equal
    if (a == P(intArrayOf(1, 2, 3), "x")) return "f2"  // different instance, same content -> NOT equal
    if (a.hashCode() != P(arr, "x").hashCode()) return "f3"
    if (a.toString() != "P(v=[1, 2, 3], s=x)") return "f4: $a"  // toString IS content
    return "OK"
}
"#,
    ),
    // ---- @JvmInline value classes ----
    // Unboxed construction + sole-property access + a member function (dispatched on the boxed value).
    (
        "ValueClassMemberFn",
        r#"
@JvmInline
value class Meters(val v: Int) {
    fun doubled(): Int = v * 2
    fun label(): String = "m=$v"
}
fun box(): String {
    val m = Meters(21)
    if (m.v != 21) return "f1"
    if (m.doubled() != 42) return "f2:${m.doubled()}"
    if (m.label() != "m=21") return "f3:${m.label()}"
    return "OK"
}
"#,
    ),
    // Synthesized equals/hashCode/toString over the single underlying field.
    (
        "ValueClassEqHashStr",
        r#"
@JvmInline
value class Id(val raw: String)
fun box(): String {
    val a = Id("x")
    if (a != Id("x")) return "f1"
    if (a == Id("y")) return "f2"
    if (a.hashCode() != Id("x").hashCode()) return "f3"
    if (a.toString() != "Id(raw=x)") return "f4:$a"
    return "OK"
}
"#,
    ),
    // Mixed `==`: a value class compared to `Any`/another type is FALSE (boxed identity, type-checked),
    // not a raw compare of the underlying — `Id("x") == "x"` must be false.
    (
        "ValueClassMixedEquality",
        r#"
@JvmInline
value class Id(val raw: String)
@JvmInline
value class Count(val n: Int)
fun eqAny(a: Any?, b: Any?) = a == b
fun box(): String {
    if (eqAny(Id("x"), "x")) return "f1"          // value class vs raw String -> false
    if (!eqAny(Id("x"), Id("x"))) return "f2"      // same value class -> true
    if (eqAny(Count(1), 1)) return "f3"            // value class vs raw Int -> false
    if (!eqAny(Count(7), Count(7))) return "f4"
    if (eqAny(Count(1), Count(2))) return "f5"
    return "OK"
}
"#,
    ),
    // A nullable value class (`Id?` over a reference underlying) boxes/erases correctly, including in
    // `==` against a non-null and against null.
    (
        "ValueClassNullable",
        r#"
@JvmInline
value class Id(val raw: String)
fun wrap(s: String): Id? = if (s.isEmpty()) null else Id(s)
fun box(): String {
    if (wrap("") != null) return "f1"
    if (wrap("a") == null) return "f2"
    if (wrap("a")!!.raw != "a") return "f3"
    if (wrap("a") != Id("a")) return "f4"
    return "OK"
}
"#,
    ),
    // A value-class value in a string template renders via its `toString` (boxed), not the raw field.
    (
        "ValueClassStringTemplate",
        r#"
@JvmInline
value class Tag(val v: Int)
fun box(): String {
    val t = Tag(42)
    val s = "tag=$t"
    return if (s == "tag=Tag(v=42)") "OK" else "f:$s"
}
"#,
    ),
    // A value class whose underlying is a nullable primitive (`Int?`): the unboxed value may be null,
    // so no spurious non-null parameter check, and `.v` reads back null.
    (
        "ValueClassNullablePrimitiveUnderlying",
        r#"
@JvmInline
value class Opt(val v: Int?)
fun useOpt(x: Opt): String = if (x.v == null) "none" else "some:${x.v}"
fun box(): String {
    if (useOpt(Opt(null)) != "none") return "f1"
    if (useOpt(Opt(5)) != "some:5") return "f2"
    return "OK"
}
"#,
    ),
    // Equality on a PRIMITIVE-underlying value class: same-class compares the underlying directly,
    // mixed-with-Any compares boxed (type-checked).
    (
        "ValueClassPrimitiveEquality",
        r#"
@JvmInline
value class Px(val v: Int)
fun nullableVsUnboxed(s: Px?, t: Px) = s == t
fun box(): String {
    if (Px(1) != Px(1)) return "f1"
    if (Px(1) == Px(2)) return "f2"
    val a: Any = Px(1)
    if (a != Px(1)) return "f3"
    if (a == Px(2)) return "f4"
    if (a == 1) return "f5"
    if (!nullableVsUnboxed(Px(3), Px(3))) return "f6"   // A? == A, same value -> true
    if (nullableVsUnboxed(Px(3), Px(4))) return "f7"
    if (nullableVsUnboxed(null, Px(3))) return "f8"     // null != Px(3)
    return "OK"
}
"#,
    ),
    // A value class IMPLEMENTING an interface: a member call on the unboxed value dispatches directly,
    // but assigning it to the interface type boxes (the box implements the interface; the raw underlying
    // does not), so virtual dispatch through the interface works.
    (
        "ValueClassImplementsInterface",
        r#"
interface IFoo { fun tag(): String }
@JvmInline
value class Str(val value: String) : IFoo { override fun tag(): String = "t:$value" }
fun box(): String {
    val s = Str("OK")
    if (s.tag() != "t:OK") return "f1:${s.tag()}"
    val f: IFoo = s
    return if (f.tag() == "t:OK") "OK" else "f2:${f.tag()}"
}
"#,
    ),
    // A concrete class overriding an interface method that returns a value class — the override returns
    // the unboxed value, dispatched virtually through the interface.
    (
        "ValueClassOverride",
        r#"
@JvmInline
value class Wrap(val s: String)
interface Base { fun get(): Wrap }
class Impl(val w: Wrap) : Base { override fun get(): Wrap = w }
fun box(): String {
    val b: Base = Impl(Wrap("OK"))
    return b.get().s
}
"#,
    ),
    // A value-class-returning override through a GENERIC interface: the override is emitted under a
    // mangled `-<hash>` name (returning the unboxed underlying) plus an `Object` boxing bridge, so the
    // erased virtual call hands back a boxed value (`is X` holds, `(x as X).v` unboxes).
    (
        "ValueClassGenericOverrideBridge",
        r#"
@JvmInline
value class Gx(val v: Any)
interface IFooG<T> { fun foo(): T; fun bar(): Gx }
class TestGx : IFooG<Gx> {
    override fun foo(): Gx = Gx("O")
    override fun bar(): Gx = Gx("K")
}
fun box(): String {
    val t: IFooG<Gx> = TestGx()
    val tFoo: Any = t.foo()
    if (tFoo !is Gx) return "f1: $tFoo"
    return (t.foo() as Gx).v.toString() + t.bar().v.toString()
}
"#,
    ),
    // A value class returned from a lambda (`() -> X`) is boxed at the lambda body's tail, so a generic
    // `() -> T` consumer hands back a boxed value the caller can unbox.
    (
        "ValueClassReturnedFromLambda",
        r#"
@JvmInline
value class Lx(val x: Any)
fun useLx(x: Lx): String = x.x as String
fun <T> callL(fn: () -> T): T = fn()
fun box(): String = useLx(callL { Lx("OK") })
"#,
    ),
    // A NESTED value class (`Nb(val a: Na)` where `Na` is itself a value class): both unbox to the
    // innermost underlying; property chains read through, and equality compares the final underlying.
    (
        "ValueClassNested",
        r#"
@JvmInline
value class Na(val x: Int)
@JvmInline
value class Nb(val a: Na)
fun box(): String {
    val b = Nb(Na(42))
    if (b.a.x != 42) return "f1:${b.a.x}"
    if (b != Nb(Na(42))) return "f2"
    if (b == Nb(Na(7))) return "f3"
    return "OK"
}
"#,
    ),
    // A value class with a SECONDARY constructor delegating to the primary.
    (
        "ValueClassOverrideGenericWithVcParam",
        r#"
@JvmInline value class Mk(val i: Int)
interface Iface<T> { fun foo(i: Mk): T }
@JvmInline value class Wrp(val a: Any)
class CG : Iface<Wrp> { override fun foo(i: Mk): Wrp = Wrp("OK") }
fun box(): String {
    val g: Iface<Wrp> = CG()
    val r: Wrp = g.foo(Mk(0))
    if (r.a != "OK") return "f1"
    val r2: Wrp = CG().foo(Mk(0))
    if (r2.a != "OK") return "f2"
    return "OK"
}
"#,
    ),
    (
        "ValueClassOverriddenReturnThroughMangledInterface",
        r#"
@JvmInline value class Xb(val x: String)
interface IBar1<T> { fun foo(x: T): Xb }
interface IBar2 { fun foo(x: String): Xb }
class TestBar : IBar1<String>, IBar2 { override fun foo(x: String): Xb = Xb(x) }
fun box(): String {
    val t1: IBar1<String> = TestBar()
    val t2: IBar2 = TestBar()
    return t1.foo("O").x + t2.foo("K").x
}
"#,
    ),
    (
        "ValueClassUnboundedGenericNullCapable",
        r#"
@JvmInline value class Ag<T>(val x: T)
fun <T> isNullVac(s: Ag<T>) = s == null
fun mk(): Ag<String?> = Ag(null)
fun box(): String {
    if (isNullVac(mk())) return "f1"
    return "OK"
}
"#,
    ),
    (
        "ValueClassPropertyThroughInterface",
        r#"
interface Bse { val id: Int }
@JvmInline value class Chld(val id2: Int) : Bse { override val id: Int get() = id2 }
interface Bse2 { val prop: Bse }
class Chld2(override val prop: Chld) : Bse2
fun box(): String {
    val y: Bse2 = Chld2(Chld(5))
    if (y.prop.id != 5) return "f1:${y.prop.id}"
    return "OK"
}
"#,
    ),
    (
        "ValueClassInferredNullableLocal",
        r#"
@JvmInline value class Zq1(val x: String)
@JvmInline value class Zqn(val z: Zq1?)
@JvmInline value class Zqn2(val z: Zqn)
fun zq(b: Boolean): Zqn2? = if (b) null else Zqn2(Zqn(null))
fun zeq(a: Any?, b: Any?) = a == b
fun box(): String {
    val x = zq(true)
    val y = zq(false)
    if (zeq(x, y)) return "f1"
    if (x != null) return "f2"
    if (y == null) return "f3"
    return "OK"
}
"#,
    ),
    (
        "ValueClassSafeCallReturningVc",
        r#"
@JvmInline value class Zs(val x: Int)
class Ah { fun foo() = Zs(42) }
fun tst(a: Ah?): Zs = a?.foo()!!
fun box(): String {
    val t = tst(Ah())
    if (t.x != 42) return "f:${t.x}"
    return "OK"
}
"#,
    ),
    (
        "ValueClassSafeCastAndAccessInMember",
        r#"
interface MyIf
var k5sink: Any? = null
fun k5save(a: Any?) { k5sink = a }
@JvmInline value class MyCv(val value: Int): MyIf {
    fun foo(other: MyIf) { k5save((other as? MyCv)?.value) }
}
fun box(): String {
    val x = MyCv(5)
    x.foo(x)
    if (k5sink != 5) return "f:$k5sink"
    return "OK"
}
"#,
    ),
    (
        "ValueClassMemberWithVcParamCall",
        r#"
interface IFp<T> { fun foo(x: T): String }
@JvmInline value class Zp(val x: Int) : IFp<Zp> { override fun foo(x: Zp) = "OK" }
fun box(): String = Zp(1).foo(Zp(2))
"#,
    ),
    (
        "MemberFieldShadowsTopLevel",
        r#"
var shadowResult = "Fail"
@JvmInline value class Av(val value: String) { fun f() = value + "K" }
class Bh(val a: Av) { val shadowResult: String; init { shadowResult = a.f() } }
fun box(): String = Bh(Av("O")).shadowResult
"#,
    ),
    (
        "ValueClassGenericMemberCallInInitBlock",
        r#"
@JvmInline value class Ag2<T: String>(val value: T) { fun f() = value + "K" }
class Bg2<T: String>(val a: Ag2<T>) { val gicResult: String; init { gicResult = a.f() } }
fun box(): String = Bg2(Ag2("O")).gicResult
"#,
    ),
    (
        "ValueClassGenericInterfaceBridgeUnbox",
        r#"
@JvmInline value class Aw(val s: String)
interface Bw<T, U> { fun g(x: T, y: U): String }
open class Cw { open fun g(x: Aw, y: Aw): String = y.s }
class Dw : Cw(), Bw<Aw, Aw> { override fun g(x: Aw, y: Aw): String = x.s }
fun box(): String = (Dw() as Bw<Aw, Aw>).g(Aw("OK"), Aw("Fail"))
"#,
    ),
    (
        // A class method returning a nullable value class over a PRIMITIVE underlying must keep it BOXED
        // (`X?` can't be the unboxed `int`), and the inferred local `val r = h.get(..)` must carry the `?`.
        "ValueClassNullableMemberReturnBoxes",
        r#"
@JvmInline value class Wrapn(val i: Int)
class Holdern { fun get(b: Boolean): Wrapn? = if (b) Wrapn(5) else null }
fun box(): String {
    val h = Holdern()
    if (h.get(false) != null) return "f1"
    val r = h.get(true)
    if (r == null) return "f2"
    return "OK"
}
"#,
    ),
    (
        // An override returning a value class through an interface declaring `X?`: the override mangles by
        // the interface's nullable signature + a bridge. Boxed `X?` (over `Any?`) and unboxed `X?` (over a
        // non-null `Any`) take different bridge return-box decisions.
        "ValueClassOverrideNullableInterfaceReturnBoxed",
        r#"
@JvmInline value class Xob(val x: Any?)
interface IFob { fun foo(): Xob? }
class Tob : IFob { override fun foo(): Xob = Xob(null) }
fun box(): String {
    val t1: IFob = Tob()
    val x1 = t1.foo()
    if (x1 != Xob(null)) return "f1: $x1"
    return "OK"
}
"#,
    ),
    (
        "ValueClassOverrideNullableInterfaceReturnUnboxed",
        r#"
@JvmInline value class Xou(val x: Any)
interface IFou { fun foo(): Xou? }
class Tou : IFou { override fun foo(): Xou = Xou("OK") }
fun box(): String {
    val t1: IFou = Tou()
    val x1 = t1.foo()
    if (x1 != Xou("OK")) return "f1: $x1"
    return "OK"
}
"#,
    ),
    (
        // A generic value class wrapping a `List<T>` with a secondary constructor `(value: T)`. Two things:
        // (1) the `List<T>` underlying resolves to `java/util/List` (not erased `Object`), so the primary
        // `constructor-impl(List)` and the secondary `constructor-impl(T=Object)` are DISTINCT (no Duplicate
        // method); (2) `ICs("abc")` (a String arg) selects the SECONDARY ctor, not the primary `List` one.
        "ValueClassSecondaryConstructorGeneric",
        r#"
@JvmInline value class ICs<T>(val value: List<T>) {
    constructor(value: T) : this(listOf(value))
}
fun box(): String {
    if (ICs("abc").value.singleOrNull() != "abc") return "f1"
    if (ICs(listOf("x", "y")).value.size != 2) return "f2"
    return "OK"
}
"#,
    ),
    (
        "ValueClassComparableBridgeKeepsBoxedParam",
        r#"
@JvmInline value class Fooc(val x: Int) : Comparable<Fooc> { override fun compareTo(other: Fooc): Int = 10 }
fun box(): String {
    val f1 = Fooc(42)
    val ff1: Comparable<Fooc> = f1
    if (ff1.compareTo(f1) != 10) return "Fail"
    return "OK"
}
"#,
    ),
    (
        "ValueClassInitBlockNotInBoxImpl",
        r#"
@JvmInline value class Icb(val i: Int) { init { icbCounter += i } }
var icbCounter = 0
fun <T> ident(t: T) = t
fun box(): String {
    val ic = Icb(42)
    if (icbCounter != 42) return "f1:$icbCounter"
    icbCounter = 0
    ident(ic)
    if (icbCounter != 0) return "f2:$icbCounter"
    return "OK"
}
"#,
    ),
    (
        "ValueClassWrappingValueClassBoxes",
        r#"
@JvmInline value class Res<T>(val a: Any?)
fun box(): String {
    val a = Res<Int>(1)
    val c = Res<Res<Int>>(a)
    if (a.a !is Int) return "f1"
    if (c.a !is Res<*>) return "f2"
    if ((c.a as Res<*>).a !is Int) return "f3"
    return "OK"
}
"#,
    ),
    (
        "ValueClassGenericReceiverNoSpuriousUnbox",
        r#"
@JvmInline value class Gs<T: String>(val s: T)
class GsHolder(val g: Gs<String>) {
    constructor(x: String, g: Gs<String>) : this(Gs(x + g.s))
}
fun box(): String {
    if (GsHolder("O", Gs("K")).g.s != "OK") return "f1"
    return "OK"
}
"#,
    ),
    (
        "ValueClassSecondaryCtor",
        r#"
@JvmInline
value class Sc(val v: Int) {
    constructor(s: String) : this(s.length)
}
fun box(): String {
    if (Sc(3).v != 3) return "f1"
    if (Sc("abcde").v != 5) return "f2:${Sc("abcde").v}"
    return "OK"
}
"#,
    ),
    // An enum class with a value-class constructor param (`enum Te(val s: Sv) { OK(Sv("OK")) }`): the
    // entry arg `Sv("OK")` in `<clinit>` rewrites `new Sv` → `constructor-impl`.
    (
        "ValueClassEnumConstructor",
        r#"
@JvmInline value class Sev(val string: String)
enum class Te(val s: Sev) { OK(Sev("OK")) }
fun box(): String = Te.OK.s.string
"#,
    ),
    // A REGULAR class's secondary constructor with value-class params (`Test(x: String, s: Sv)`): its
    // params erase, and its `this(Sv(…))` delegation body rewrites `new Sv` → `constructor-impl` and
    // unboxes `s.string`.
    (
        "ValueClassRegularClassSecondaryCtor",
        r#"
@JvmInline value class Sv(val string: String)
class TestSc(val s: Sv) { constructor(x: String, s: Sv) : this(Sv(x + s.string)) }
fun box(): String {
    if (TestSc("O", Sv("K")).s.string != "OK") return "f1"
    return "OK"
}
"#,
    ),
    // A value-class member call INSIDE a regular class's `init { … }` block — the init block runs in
    // `<init>` over the unboxed ctor params, so the `a.f()` receiver (an unboxed value class) must box.
    (
        "ValueClassMemberCallInInitBlock",
        r#"
@JvmInline value class Iv(val value: String) { fun f() = value + "K" }
class Holder(val a: Iv) {
    val result: String
    init { result = a.f() }
}
fun box(): String {
    if (Holder(Iv("O")).result != "OK") return "f1"
    return "OK"
}
"#,
    ),
    // A base-class constructor argument that is a value class (`class Sub(x: Vb) : Base(x)`): the super
    // call runs in the subclass `<init>` over its unboxed ctor params, so the arg is rewritten/boxed.
    (
        "ValueClassSuperCtorArg",
        r#"
@JvmInline value class Vb(val s: String)
abstract class BaseVb(val x: Vb)
class SubVb(x: Vb) : BaseVb(x)
sealed class SealedVb(val x: Vb)
class SubSealed(x: Vb) : SealedVb(x)
fun box(): String {
    if (SubVb(Vb("OK")).x.s != "OK") return "f1"
    if (SubSealed(Vb("OK")).x.s != "OK") return "f2"
    return "OK"
}
"#,
    ),
    // A value class whose underlying is a BOXED value class (`Hzn(val z: Hz1?)`, `Hz1(val x: Int)`): its
    // synthesized hashCode/equals must operate on the immediate erased underlying (`LHz1;`, a reference →
    // `Hz1`'s own hashCode/equals), NOT the final `Int` of the nested chain.
    (
        "ValueClassNestedBoxedHashEq",
        r#"
@JvmInline value class Hz1(val x: Int)
@JvmInline value class Hzn(val z: Hz1?)
fun hznWrap(n: Int): Hzn? = if (n < 0) null else Hzn(Hz1(n))
fun box(): String {
    if (hznWrap(-1) != null) return "f1"
    if (hznWrap(42)!!.z!!.x != 42) return "f2"
    if (hznWrap(42) != hznWrap(42)) return "f3"
    if (hznWrap(42).hashCode() != hznWrap(42).hashCode()) return "f4"
    return "OK"
}
"#,
    ),
    // A value class whose nested underlying chain is null-capable (`Nc2(val z: Nc1)`, `Nc1(val z: Ncs?)`):
    // `<init>` emits NO `checkNotNullParameter` on the null-capable param, and `Nc2?` BOXES so `Nc2(Nc1(null))`
    // ≠ a `null` `Nc2?`.
    (
        "ValueClassNestedNullCapable",
        r#"
@JvmInline value class Ncs(val x: String)
@JvmInline value class Nc1(val z: Ncs?)
@JvmInline value class Nc2(val z: Nc1)
fun ncMk(b: Boolean): Nc2? = if (b) null else Nc2(Nc1(null))
fun ncEq(a: Any?, b: Any?) = a == b
fun box(): String {
    val x: Nc2? = ncMk(true)
    val y: Nc2? = ncMk(false)
    if (ncEq(x, y)) return "f1"
    if (x != null) return "f2"
    if (y == null) return "f3"
    return "OK"
}
"#,
    ),
    // A value class over a NULLABLE reference (`NrefA(val x: String?)`), nested (`NrefN(val z: NrefA?)`):
    // `NrefA?`/`NrefN?` box (underlying holds null). The `!!.z!!.x` chain must `unbox-impl` at each step —
    // order-independently of when the inner `.z` access is rewritten (step-4 `targets` iterate unordered).
    (
        "ValueClassNestedNullableRef",
        r#"
@JvmInline value class NrefA(val x: String?)
@JvmInline value class NrefN(val z: NrefA?)
fun nrefWrap1(x: String): NrefA? = if (x.length == 0) null else NrefA(x)
fun nrefWrapN(x: String): NrefN? = if (x.length == 0) null else NrefN(NrefA(x))
fun box(): String {
    if (nrefWrap1("") != null) return "f1"
    if (nrefWrap1("a")!!.x != "a") return "f2"
    if (nrefWrapN("") != null) return "f3"
    if (nrefWrapN("a")!!.z!!.x != "a") return "f4"
    return "OK"
}
"#,
    ),
    // A value class whose underlying is ITSELF a value class (`Z2(val z: Z1)`), returned nullable.
    // The nested chain erases to the final underlying (`Int`); a nullable return is the boxed wrapper.
    (
        "ValueClassNestedNullable",
        r#"
@JvmInline
value class NnstA(val x: Int)
@JvmInline
value class NnstB(val z: NnstA)
fun nnstWrap2(n: Int): NnstB? = if (n < 0) null else NnstB(NnstA(n))
fun box(): String {
    if (nnstWrap2(-1) != null) return "f1"
    if (nnstWrap2(42) == null) return "f2"
    if (nnstWrap2(42)!!.z.x != 42) return "f3:${nnstWrap2(42)!!.z.x}"
    return "OK"
}
"#,
    ),
    // A `super.f(vc)` call to a method mangled because it takes a value-class parameter — the super
    // (invokespecial) call must use the mangled name + erased descriptor.
    (
        "ValueClassMangledSuperCall",
        r#"
@JvmInline
value class Iv(val i: Int)
abstract class Ab { abstract fun f(i: Iv): String }
open class Bs : Ab() { override fun f(i: Iv): String = "OK" }
class Cs : Bs() { override fun f(i: Iv): String = super.f(i) }
fun box(): String = Cs().f(Iv(0))
"#,
    ),
    // A value class boxed to `Any` then tested with `is` against an interface it does NOT implement —
    // the box is not a `Comparable`/`Number`, so all branches are false.
    (
        "ValueClassBoxedInstanceOf",
        r#"
@JvmInline
value class Xs(val x: String)
@JvmInline
value class Yi(val x: Int)
fun box(): String = when {
    (Xs("") as Any) is Comparable<*> -> "1"
    (Yi(2) as Any) is Comparable<*> -> "2"
    (Xs("") as Any) is Number -> "3"
    (Yi(2) as Any) is Number -> "4"
    else -> "OK"
}
"#,
    ),
    // A nullable value class flowing into a stdlib call (a map key) boxes null-safely — a `null` value
    // class stays `null`, not `box-impl(null)` (which would hit the ctor's non-null check).
    (
        "ValueClassNullableIntoStdlib",
        r#"
class Uuid
@JvmInline
value class ValueId(val value: Uuid)
fun box(): String {
    val m = mutableMapOf<ValueId?, String>()
    val v: ValueId? = null
    m[v] = "OK"
    return m[v]!!
}
"#,
    ),
    // A data class with VALUE-CLASS fields: its synthesized `toString`/`equals`/`hashCode` box each
    // value-class field so the value class's own methods run (`a=1`, structural equality).
    (
        "DataClassWithValueClassFields",
        r#"
@JvmInline
value class Aug(val x: Int) { override fun toString(): String = (x + 1).toString() }
data class Pair2(val a: Aug, val b: Aug)
fun box(): String {
    val p = Pair2(Aug(0), Aug(4))
    if (p.toString() != "Pair2(a=1, b=5)") return "f1:$p"
    if (p != Pair2(Aug(0), Aug(4))) return "f2"
    if (p == Pair2(Aug(9), Aug(4))) return "f3"
    if (p.hashCode() != Pair2(Aug(0), Aug(4)).hashCode()) return "f4"
    return "OK"
}
"#,
    ),
    // Value classes in string templates: multiple appends, `+` concat, and a nullable value rendering
    // as "null".
    (
        "ValueClassStringTemplateConcat",
        r#"
@JvmInline
value class Z(val value: Int)
fun t1(z: Z) = "$z$z"
fun t2(z: Z) = "-" + z
fun t3(z: Z?) = "$z"
fun box(): String {
    if (t1(Z(42)) != "Z(value=42)Z(value=42)") return "f1:${t1(Z(42))}"
    if (t2(Z(42)) != "-Z(value=42)") return "f2:${t2(Z(42))}"
    if (t3(null) != "null") return "f3:${t3(null)}"
    if (t3(Z(7)) != "Z(value=7)") return "f4:${t3(Z(7))}"
    return "OK"
}
"#,
    ),
    // A value class with a CUSTOM `toString` override — the user's wins (not the synthesized default),
    // including in a string template.
    (
        "ValueClassCustomToString",
        r#"
@JvmInline
value class Augmented(val x: Int) {
    override fun toString(): String = (x + 1).toString()
}
fun box(): String {
    val a = Augmented(0)
    if (a.toString() != "1") return "f1:${a.toString()}"
    if ("$a" != "1") return "f2:$a"
    return "OK"
}
"#,
    ),
    // A value class implementing an interface, returned through `if/else` branches as the interface
    // type — each branch boxes so the interface method dispatches.
    (
        "ValueClassBranchInterfaceReturn",
        r#"
interface Base { fun result(): Int }
@JvmInline
value class Inlined(val x: Int) : Base { override fun result(): Int = x }
fun foo(b: Boolean): Base = if (b) Inlined(0) else Inlined(1)
fun box(): String {
    if (foo(true).result() != 0) return "f1"
    if (foo(false).result() != 1) return "f2"
    return "OK"
}
"#,
    ),
    // A function whose declared return type is `Any?` (a supertype) returning a value-class value must
    // box it at the return.
    (
        "ValueClassReturnedAsAny",
        r#"
@JvmInline
value class W(val v: Int)
fun make(): W = W(7)
fun makeAny(): Any? = make()
fun box(): String {
    val a = makeAny()
    if (a !is W) return "f1"
    if ((a as W).v != 7) return "f2"
    return "OK"
}
"#,
    ),
    // Value-class member functions calling each other, including one taking a value-class parameter and
    // passing `this` — the member bodies run on the boxed object.
    (
        "ValueClassMembersCallMembers",
        r#"
@JvmInline
value class Foo(val x: Int) {
    fun empty() = ""
    fun withParam(a: String) = a
    fun withInlineClassParam(f: Foo) = f.toString()
    fun test(): String = empty() + withParam("hello") + withInlineClassParam(this)
    override fun toString(): String = x.toString()
}
fun box(): String = if (Foo(12).test() != "hello12") "fail" else "OK"
"#,
    ),
    // A GENERIC value class (`Gc<T>(val v: T)`): the type parameter erases to its bound (`Any`/the
    // upper bound), construction + property access + nullable wrapping behave like the erased form.
    (
        "ValueClassGeneric",
        r#"
@JvmInline
value class Gc<T>(val v: T)
fun wrap(s: String): Gc<String>? = if (s.isEmpty()) null else Gc(s)
fun box(): String {
    val g = Gc("OK")
    if (g.v != "OK") return "f1"
    if (wrap("") != null) return "f2"
    if (wrap("a")!!.v != "a") return "f3"
    return "OK"
}
"#,
    ),
    // A property reference to a value class's member (`Z::x`), invoked via `.get(boxedReceiver)`.
    (
        "ValueClassPropertyRef",
        r#"
@JvmInline
value class Z(val x: Int)
fun box(): String {
    if ((Z::x).get(Z(42)) != 42) return "f1"
    return "OK"
}
"#,
    ),
    // A value class over a `Double` uses IEEE TOTAL-ORDER equality (kotlinc): `NaN == NaN` is true and
    // `0.0 != -0.0`, via `equals-impl0` delegating to the boxed `Double` compare, not bare `==`.
    (
        "ValueClassDoubleTotalOrder",
        r#"
@JvmInline
value class D(val v: Double)
fun box(): String {
    if (D(Double.NaN) != D(Double.NaN)) return "f1"
    if (D(0.0) == D(-0.0)) return "f2"
    if (D(1.5) != D(1.5)) return "f3"
    return "OK"
}
"#,
    ),
    // Equality for a value class over a NULLABLE underlying (`Any?`): `== null` on a non-null value is
    // vacuously false; a nullable value participates in null checks and same-class structural compares.
    (
        "ValueClassNullableUnderlyingEquality",
        r#"
@JvmInline
value class Av(val x: Any?)
fun vacuousLeft(s: Av) = s == null
fun nullLeft(s: Av?) = s == null
fun sameNullable(s: Av?, t: Av?) = s == t
fun box(): String {
    if (vacuousLeft(Av(0))) return "f1"
    if (vacuousLeft(Av(null))) return "f1b"
    if (nullLeft(Av(0))) return "f2"
    if (nullLeft(Av(null))) return "f2b"   // Av(null) is a non-null Av wrapping null -> NOT null
    if (!nullLeft(null)) return "f3"
    if (!sameNullable(null, null)) return "f4"
    if (!sameNullable(Av(1), Av(1))) return "f5"
    if (sameNullable(Av(1), Av(2))) return "f6"
    if (sameNullable(null, Av(1))) return "f7"
    if (!sameNullable(Av(null), Av(null))) return "f8"   // both Av(null) -> equal
    if (sameNullable(null, Av(null))) return "f9"        // null != Av(null)
    return "OK"
}
"#,
    ),
    // An array of value-class values: each element is boxed (a reference array of the boxed type), read
    // back and unboxed on access.
    (
        "ValueClassArray",
        r#"
@JvmInline
value class Vc(val v: Int)
fun box(): String {
    val arr = arrayOf(Vc(1), Vc(2), Vc(3))
    var sum = 0
    for (x in arr) sum += x.v
    if (sum != 6) return "f1:$sum"
    if (arr[1].v != 2) return "f2"
    return "OK"
}
"#,
    ),
    // The sized reference-array constructor must treat value-class elements as logical `Array<Vc>` via the
    // provider-owned value-underlying fact, not by hardcoding unsigned/builtin carriers in the checker.
    (
        "ValueClassSizedArray",
        r#"
@JvmInline
value class Vc(val v: Int)
fun box(): String {
    val arr = Array(3) { Vc(it + 1) }
    var sum = 0
    for (x in arr) sum += x.v
    if (sum != 6) return "f1:$sum"
    if (arr[2].v != 3) return "f2"
    return "OK"
}
"#,
    ),
    // A nested value class whose `init` block reads the value-class field's property.
    (
        "ValueClassNestedInitBlock",
        r#"
var sink = "Fail"
@JvmInline
value class A2(val value: String)
@JvmInline
value class B2(val a: A2) { init { sink = a.value } }
fun box(): String {
    B2(A2("OK"))
    return sink
}
"#,
    ),
    // A value class with an `init` block: the validation runs in `constructor-impl` on construction.
    (
        "ValueClassInitBlock",
        r#"
@JvmInline
value class Pos(val v: Int) {
    init { if (v < 0) throw IllegalArgumentException("neg") }
}
fun box(): String {
    val p = Pos(5)
    if (p.v != 5) return "f1"
    try {
        Pos(-1)
        return "f2"
    } catch (e: IllegalArgumentException) {
        return "OK"
    }
}
"#,
    ),
    // `for (i in (a..b).reversed())` over a literal range → descending counted loop (phase 402).
    (
        "ForInReversedLiteralRange",
        r#"
fun box(): String {
    var s = 0
    for (i in (1..4).reversed()) s = s * 10 + i
    if (s != 4321) return "f1: $s"
    var t = 0
    for (i in (0..3).reversed()) t = t * 10 + i
    if (t != 3210) return "f2: $t"
    var u = 0
    for (i in (4 downTo 1).reversed()) u = u * 10 + i
    if (u != 1234) return "f3: $u"
    var w = 0
    for (i in (1 until 5).reversed()) w = w * 10 + i
    if (w != 4321) return "f4: $w"
    return "OK"
}
"#,
    ),
];

#[test]
fn feature_snippets_run() {
    // Run on a thread with a generous stack. Under `cargo test` (what `just test-all` uses) each test
    // runs on a libtest worker thread whose default stack is 2 MiB — too small for the recursive-descent
    // compiler on a deeply-nested snippet (e.g. nested inline calls) in the unoptimized debug profile,
    // where `opt-level = 0` reuses no stack slots so a big-`match` frame (`lower_arg` ~485 KiB measured)
    // holds every arm's locals at once. It is NOT a runaway recursion (depth stays ~12) and never happens
    // in release/`gate` (slots reused) — the CLI (8 MiB main thread) and the nextest runner (per-test
    // PROCESS, main-thread stack) both compile it fine. This matches that headroom for the plain runner.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(feature_snippets_run_impl)
        .unwrap()
        .join()
        .unwrap();
}

fn feature_snippets_run_impl() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping feature_box_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping feature_box_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let jdk_modules = format!("{java_home}/lib/modules");
    let work = std::env::temp_dir().join(format!("krusty_feat_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    // Reflective runner compiled once.
    let runner = work.join("runner");
    fs::create_dir_all(&runner).unwrap();
    let runner_src = r#"import java.io.File; import java.net.URL; import java.net.URLClassLoader;
public class BoxRun {
  public static void main(String[] args) throws Exception {
    for (int i = 0; i + 1 < args.length; i += 2) {
      String result;
      try {
        URLClassLoader cl = new URLClassLoader(new URL[]{ new File(args[i]).toURI().toURL() }, BoxRun.class.getClassLoader());
        Object r = Class.forName(args[i+1], true, cl).getMethod("box").invoke(null);
        result = String.valueOf(r);
      } catch (Throwable t) { result = "EXC:" + t; }
      System.out.println(args[i+1] + "\t" + result);
    }
  }
}
"#;
    fs::write(runner.join("BoxRun.java"), runner_src).unwrap();
    let jc = Command::new(&javac)
        .args(["-d", runner.to_str().unwrap()])
        .arg(runner.join("BoxRun.java"))
        .output()
        .unwrap();
    assert!(
        jc.status.success(),
        "javac(BoxRun): {}",
        String::from_utf8_lossy(&jc.stderr)
    );

    // Compile every snippet in-process (sharing the process-global classpath caches) and write its
    // class bytes into its own dir — a per-snippet dir keeps the JVM runner's class loaders isolated, so
    // two snippets that both declare e.g. `class C` don't collide. In-process compilation avoids spawning
    // the krusty binary (and rebuilding the stdlib/jimage indexes) once per snippet.
    let cp_jars = common::classpath_jars_for("// WITH_STDLIB");
    let jdk_modules = std::path::Path::new(&jdk_modules);
    let modules_opt = jdk_modules.exists().then_some(jdk_modules);
    let mut cases: Vec<(String, String)> = Vec::new(); // (dir, boxClass)
    for (i, (stem, src)) in SNIPPETS.iter().enumerate() {
        let dir = work.join(format!("s{i}"));
        fs::create_dir_all(&dir).unwrap();
        let classes = common::compile_in_process(src, stem, &cp_jars, modules_opt)
            .unwrap_or_else(|| panic!("krusty {stem}: in-process compile failed"));
        for (internal, bytes) in &classes {
            let path = dir.join(format!("{internal}.class"));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, bytes).unwrap();
        }
        cases.push((dir.to_str().unwrap().to_string(), format!("{stem}Kt")));
    }

    // Run all snippets in one JVM.
    let mut cp = runner.to_str().unwrap().to_string();
    cp.push(':');
    cp.push_str(&stdlib);
    let mut args: Vec<String> = vec!["-Xverify:all".into(), "-cp".into(), cp, "BoxRun".into()];
    for (dir, class) in &cases {
        args.push(dir.clone());
        args.push(class.clone());
    }
    let run = Command::new(&java).args(&args).output().unwrap();
    assert!(
        run.status.success(),
        "BoxRun: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    let results: std::collections::HashMap<&str, &str> =
        stdout.lines().filter_map(|l| l.split_once('\t')).collect();
    for (_, class) in &cases {
        let got = results.get(class.as_str()).copied().unwrap_or("<missing>");
        assert!(
            got == "OK",
            "{class}.box() returned {got:?} (all: {stdout})"
        );
    }
    let _ = fs::remove_dir_all(&work);
}

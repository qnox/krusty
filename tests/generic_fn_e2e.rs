//! Generic functions (`fun <T> id(x: T): T`): the JVM signature erases `T` to `Object`, and the call
//! site inserts a `checkcast` to the inferred concrete type — matching kotlinc. Includes a generic
//! higher-order function (`fun <T> eval(fn: () -> T) = fn()`). Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn generic_fns_run() {
    let src = "fun <T> id(x: T): T = x\n\
fun <T> firstOf(a: T, b: T): T = a\n\
fun <T> eval(fn: () -> T): T = fn()\n\
fun box(): String {\n\
val s: String = id(\"OK\")\n\
if (s != \"OK\") return \"f1\"\n\
if (firstOf(\"X\", \"Y\") != \"X\") return \"f2\"\n\
if (eval { \"Z\" } != \"Z\") return \"f3\"\n\
return id(\"OK\")\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "G");
}

/// A property declared as a class type parameter (`class Box<T>(val x: T)`) erases to `Object`, but a
/// read on a concrete instantiation (`Box<Int>().x`) recovers the argument: the front end substitutes
/// the type argument, and codegen inserts the `checkcast`/unbox kotlinc emits on the erased read.
/// Covers a primitive argument (unbox), a reference argument (checkcast), and positional indexing.
#[test]
fn generic_property_substitution_runs() {
    let src = "class Box<T>(val x: T)\n\
class Pair2<A, B>(val a: A, val b: B)\n\
fun box(): String {\n\
val bi: Box<Int> = Box(40)\n\
if (bi.x + 2 != 42) return \"f1\"\n\
val bs: Box<String> = Box(\"OK\")\n\
if (bs.x != \"OK\") return \"f2\"\n\
val p: Pair2<Int, String> = Pair2(7, \"hi\")\n\
if (p.a != 7) return \"f3\"\n\
if (p.b != \"hi\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "G");
}

/// A generic instance method with its OWN type parameter and a function parameter
/// (`class Box<T> { fun <R> map(f: (T) -> R): R }`) substitutes both: the lambda parameter `it` types
/// as the receiver's element type `T` (`Box<String>` → `it: String`), and the method type parameter
/// `R` is inferred from the lambda body's type and becomes the call's result type. Covers a reference
/// element type (`Box<String>`, `it.length`) and a primitive element type (`Box<Int>`, `it * 2`), with
/// `R` inferred to both a primitive (`Int`) and a reference (`String`).
#[test]
fn generic_hof_method_substitution_runs() {
    let src = "class Box<T>(val v: T) { fun <R> map(f: (T) -> R): R = f(v) }\n\
fun box(): String {\n\
val bs: Box<String> = Box(\"hi\")\n\
val n: Int = bs.map { it.length }\n\
if (n + 1 != 3) return \"f1\"\n\
val bi: Box<Int> = Box(21)\n\
val d: Int = bi.map { it * 2 }\n\
if (d != 42) return \"f2\"\n\
val s: String = bi.map { it.toString() }\n\
if (s != \"21\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "G");
}

/// A NON-inline top-level generic higher-order function (`fun <T, R> transform(x: T, f: (T) -> R): R`)
/// binds its type parameter `T` from the FIRST value argument, so the lambda parameter `it` types as
/// that concrete type (`transform(Item(...)) { it.name }` → `it: Item`) and `R` is inferred from the
/// lambda body (the call result). The lambda materializes as an erased `Function1` whose `invoke`
/// `checkcast`s the parameter — sound for a reference/class binding.
#[test]
fn non_inline_generic_hof_binds_lambda_param() {
    let src = "class Item(val name: String)\n\
fun <T, R> transform(x: T, f: (T) -> R): R = f(x)\n\
fun box(): String {\n\
val r: String = transform(Item(\"k\")) { it.name }\n\
val n: Int = transform(listOf(1, 2, 3)) { it.size }\n\
return if (r == \"k\" && n == 3) \"OK\" else \"FAIL: $r/$n\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "G");
}

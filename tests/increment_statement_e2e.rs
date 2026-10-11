//! `i++`, `++i` and `i--` on an `Int` local in statement position are `iinc`, exactly like
//! `i += 1`: the increment carries its checked result type as an identity coercion around the same
//! arithmetic, and that coercion is no reason to fall back to load, add and store.
use super::common;

#[test]
fn local_increments_are_byte_identical_to_kotlinc() {
    let src = "\
fun steps(k: Int): Int {
    var n = 0
    n += 1
    n++
    ++n
    n--
    --n
    while (n < k) n++
    if (k > 3) n++
    return n
}
";
    match common::byte_diff_against_kotlinc_cp(
        "Increments",
        src,
        "IncrementsKt",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (Increments: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(why)) => panic!("{why}"),
    }
}

/// A postfix `x++` used as a value is `val tmp = x; x = tmp.inc(); tmp`: whatever storage `x` lives in (a captured
/// `Ref` cell, a local shared into a closure, a local class's captured cell, a plain local), the
/// update reads the saved old value rather than the storage again. Only a plain `Int` local is
/// updated in place (`iload; iinc`), as kotlinc's `JvmOptimizationLowering` rewrites it.
#[test]
fn postfix_updates_use_the_saved_old_value_like_kotlinc() {
    let src = "\
package postfix
fun <T> go(f: () -> T): T = f()
class Counter(val n: Int) {
    operator fun inc() = Counter(n + 1)
    operator fun dec() = Counter(n - 1)
}
fun captured(): Int { var c = 0; go { c++ }; go { c-- }; go { ++c }; return c }
fun capturedLong(): Long { var c = 0L; go { c++ }; return c }
fun capturedDouble(): Double { var c = 0.0; go { c-- }; return c }
fun capturedByte(): Byte { var c: Byte = 0; go { c++ }; return c }
fun capturedChar(): Char { var c = 'a'; go { c++ }; return c }
fun capturedShort(): Short { var c: Short = 0; go { c++ }; return c }
fun capturedFloat(): Float { var c = 0f; go { c++ }; return c }
fun capturedCounter(): Int { var c = Counter(0); go { c++ }; return c.n }
fun shared(): Int { var c = 0; val g = { c }; val r = c--; return r + c + g() }
fun sharedPrefix(): Int { var c = 0; val g = { c }; val r = ++c; return r + g() }
fun sharedLong(): Long { var c = 0L; val g = { c }; val r = c++; return r + g() }
fun sharedChar(): Char { var c = 'a'; val g = { c }; val r = c++; return r }
fun sharedCounter(): Int { var c = Counter(0); val g = { c }; val r = c++; return r.n + g().n }
fun plain(): Int { var c = 0; val r = c++; val s = c--; c++; return r + s + c }
fun plainPrefix(): Int { var c = 0; val r = ++c; return r + c }
fun plainLong(): Long { var c = 0L; val r = c++; return r + c }
fun plainByte(): Byte { var c: Byte = 0; val r = c++; return r }
fun plainChar(): Char { var c = 'a'; val r = c++; return r }
fun plainCounter(): Int { var c = Counter(0); val r = c++; return r.n + c.n }
fun indexed(xs: IntArray): Int { var i = 0; var s = 0; while (i < xs.size) s += xs[i++]; return s }
fun localClass(): Int {
    var c = 0
    class L {
        fun bump(): Int = c++
    }
    val l = L()
    return l.bump() + c
}
";
    for class in ["postfix/PostfixKt", "postfix/PostfixKt$localClass$L"] {
        match common::byte_diff_against_kotlinc_cp("Postfix", src, class, &[common::stdlib_jar()]) {
            None => eprintln!("skip (Postfix: reference toolchain unavailable)"),
            Some(Ok(())) => {}
            Some(Err(why)) => panic!("{class}: {why}"),
        }
    }
}

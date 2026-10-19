//! JVM parity for local prefix/postfix updates: `Int` updates contract to `iinc`, while captured and
//! wide/narrow values retain kotlinc's snapshot, writeback, and debug-table shape.
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

/// Complete class-byte parity covers instructions, line numbers, and local-variable debug ranges
/// for value- and statement-position updates across local and captured storage representations.
#[test]
fn postfix_and_prefix_update_shapes_are_byte_identical_to_kotlinc() {
    let source = "\
fun invoke(f: () -> Unit) { f() }

fun captured(): Int {
    var x = 0
    invoke { x++ }
    invoke { ++x }
    invoke { x += 2 }
    var c = 0L
    invoke { ++c; c++ }
    invoke { x--; Unit }
    val y = x++
    return x + y
}

fun ints(): Int {
    var x = 0
    x++
    ++x
    val y = x++
    val z = ++x
    return x + y + z
}

fun longs(): Long {
    var l = 0L
    l++
    ++l
    --l
    val m = l++
    return l + m
}

fun bytes(): Int {
    var b: Byte = 0
    b++
    ++b
    val d = b++
    return b + d
}
";
    common::byte_diff_against_kotlinc_cp(
        "PostfixIncrements",
        source,
        "PostfixIncrementsKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}

/// A postfix statement on property storage keeps its old-value temporary whether or not a receiver
/// separates the store from the read: kotlinc drops the discarded result only after the update's
/// scope closes, so a static field, an object field, a hoisted companion field and a top-level
/// property reached through its accessors all store and reload the old value.
#[test]
fn a_postfix_property_statement_keeps_its_temporary_without_a_receiver() {
    let source = "\
private var top = 1

class G {
    private var z = 1
    var w = 1
    companion object {
        private var y = 2
        var v = 3
        fun inC() { y++; v++ }
    }
    fun own() { z++; w++; y++; v++; top++ }
}

object O {
    private var q = 1
    fun f() { q++ }
}
";
    for class in ["G", "G$Companion", "O"] {
        common::byte_diff_against_kotlinc_cp(
            "PostfixPropertyStatements",
            source,
            class,
            &[common::stdlib_jar()],
        )
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|why| panic!("{class}: {why}"));
    }
}

/// A bare `if`/`when` branch body is an expression position, so an update there is the branch's
/// value even when the whole conditional is discarded: kotlinc materializes the old (postfix) or new
/// (prefix) value in each branch and pops it after the join. A loop body stays a statement.
#[test]
fn a_bare_branch_update_is_the_branch_value() {
    let source = "\
fun lg(): Long = 1L

class H { var p = 0L }

fun branches(k: Int, o: H): Long {
    var c = 0L
    if (k > 1) c++ else c--
    if (k > 1) @Suppress(\"x\") c++ else c--
    if (k > 2) ++c else lg()
    when (k) { 1 -> c++; else -> --c }
    if (k > 3) c++
    if (k > 4) o.p++ else o.p--
    while (c < k) c++
    val v = if (k > 5) c++ else 7L
    return c + v
}

fun captured(k: Int): Int {
    var c = 0
    val f = { c-- }
    f()
    if (k > 1) c++ else c--
    when (k) { 1 -> ++c; else -> c++ }
    return c
}
";
    common::byte_diff_against_kotlinc_cp(
        "BranchIncrements",
        source,
        "BranchIncrementsKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}

#[test]
fn a_lambda_updates_its_captured_cell_on_jvm_and_native() {
    // This capture shape is target E2E coverage rather than shared conformance: Wasm does not yet
    // lower RefNew, while plain local updates remain in the shared matrix.
    let source = "\
fun box(): String {
    var captured = 2
    val lambda = { val old = captured++; ++captured; old + captured }
    return if (lambda() == 6 && captured == 4) \"OK\" else \"fail\"
}
";
    common::expect_box_ok_with_stdlib(source, "CapturedLambdaIncrementJvm");
    common::expect_native_box(source, "CapturedLambdaIncrementNative", "OK");
}

#[test]
fn a_local_class_updates_its_captured_cell() {
    let source = "\
fun box(): String {
    var captured = 3
    class Worker {
        fun run(): Int {
            val old = captured++
            ++captured
            return old + captured
        }
    }
    return if (Worker().run() == 8 && captured == 5) \"OK\" else \"fail\"
}
";
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "LocalClassCapturedIncrement"),
        "OK"
    );
}

/// A postfix `x++` used as a value is `val tmp = x; x = tmp.inc(); tmp` whatever storage `x` lives
/// in — a lambda's captured `Ref` cell, a local shared into a closure, a local class's captured
/// cell, or a plain local of any primitive or user `inc` type: the update reads the saved old value
/// rather than the storage again. Only a plain `Int` local is updated in place (`iload; iinc`).
#[test]
fn value_position_postfix_updates_from_the_saved_old_value_like_kotlinc() {
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
        common::byte_diff_against_kotlinc_cp("Postfix", src, class, &[common::stdlib_jar()])
            .expect("the reference kotlinc is provisioned")
            .unwrap_or_else(|why| panic!("{class}: {why}"));
    }
}

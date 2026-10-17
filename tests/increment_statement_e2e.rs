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

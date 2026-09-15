//! An argument/descriptor arity mismatch is a refusal, never a panic and never a broken method.
//!
//! Three operand emitters asserted that a selected call's argument count matches its JVM descriptor:
//!
//! ```text
//! assertion `left == right` failed: selected call argument count must match its JVM descriptor
//!   left: 7   right: 8
//! ```
//!
//! A panic loses the diagnostic and takes down the whole compilation rather than one file. But
//! simply returning early is also wrong: the operand helper stops pushing while the caller still
//! emits its `invoke*`, so the method claims operands that were never pushed. The contract in
//! `src/jvm/ir_emit/call_operands.rs` therefore refuses with a typed value BEFORE touching the
//! operand stack, and each call site abandons the whole call.
//!
//! The refusal itself needs malformed IR to reach, so it is covered by unit tests beside the
//! contract — one per affected callee form. What lives HERE is the other half: the shapes whose
//! emitted arity legitimately differs from their source arity, which are exactly what the assertion
//! existed to police, and which must keep compiling.

use super::common;

/// Ordinary calls whose arity and descriptor DO agree must be unaffected — including the shapes
/// whose descriptors are synthesized rather than declared: a default-argument call, a vararg call,
/// and a suspend call that takes an appended continuation.
#[test]
fn ordinary_calls_at_every_arity_still_emit() {
    const MAIN: &str = "class Box(val label: String) {\n\
\x20   fun plain(a: Int, b: Int): Int = a + b\n\
\x20   fun defaulted(a: Int, b: Int = 2, c: Int = 3): Int = a + b + c\n\
\x20   fun varargs(vararg parts: String): Int = parts.size\n\
}\n\
\n\
fun box(): String {\n\
\x20   val b = Box(\"x\")\n\
\x20   if (b.plain(1, 2) != 3) return \"FAIL: plain\"\n\
\x20   if (b.defaulted(1) != 6) return \"FAIL: defaulted all\"\n\
\x20   if (b.defaulted(1, 5) != 9) return \"FAIL: defaulted one\"\n\
\x20   if (b.varargs(\"a\", \"b\", \"c\") != 3) return \"FAIL: varargs\"\n\
\x20   if (b.varargs() != 0) return \"FAIL: varargs empty\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "descriptor_arity_ordinary");
}

/// A suspend call carries a continuation the source never writes, so its emitted arity is one more
/// than its source arity — the shape the asserting sites exist to police.
#[test]
fn a_suspend_call_with_its_appended_continuation_still_emits() {
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
\n\
suspend fun inner(a: Int, b: Int): Int = a + b\n\
\n\
suspend fun outer(a: Int): Int = inner(a, 2)\n\
\n\
fun box(): String {\n\
\x20   val total = runBlocking { outer(1) }\n\
\x20   return if (total == 3) \"OK\" else \"FAIL: \" + total\n\
}\n";
    let classpath = vec![
        common::stdlib_jar(),
        common::coroutines_jar(),
        common::jdk_modules(),
    ];
    let jdk = common::jdk_modules();
    let result = common::expect_box_run(MAIN, "Main", &classpath, Some(jdk.as_path()));
    assert_eq!(result, "OK");
}

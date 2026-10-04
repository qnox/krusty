//! A classpath inline call whose `finally` cleanup is expanded in place (`Closeable.use`) keeps its
//! result in a temporary and reads it back after the `try`. When the call is a statement whose
//! result is `Unit`, that read is still a `kotlin/Unit` reference on the operand stack and must be
//! popped. Leaving it there made the `if` branch reach the join one operand higher than its sibling:
//! "cannot compute JVM frames ...: StackHeight".
//!
//! `use` is the declaration whose body the cause-and-`finally` plan decodes; a repository-owned
//! `try`/`finally` inline function reaches the bytecode inliner instead. The emitter-level boundary
//! itself is covered with repository-owned IR in `src/jvm/ir_emit.rs`.

use super::common;

const PROBE: &str = "class Probe : java.io.Closeable {\n\
    \x20   var closed = 0\n\
    \x20   var written = 0\n\
    \x20   override fun close() { closed++ }\n\
    }\n";

#[test]
fn a_unit_use_statement_on_one_branch_joins_its_sibling_like_kotlinc() {
    let source = format!(
        "{PROBE}\
        fun send(probe: Probe, text: String?): Int {{\n\
        \x20   if (text != null) {{ probe.use {{ it.written += text.length }} }} else {{ probe.close() }}\n\
        \x20   return probe.closed * 100 + probe.written\n\
        }}\n\
        fun sendWithoutElse(probe: Probe, text: String?): Int {{\n\
        \x20   if (text != null) {{ probe.use {{ it.written += text.length }} }}\n\
        \x20   return probe.closed * 100 + probe.written\n\
        }}\n\
        fun box(): String {{\n\
        \x20   val a = send(Probe(), \"abc\")\n\
        \x20   val b = send(Probe(), null)\n\
        \x20   val c = sendWithoutElse(Probe(), \"ab\")\n\
        \x20   val d = sendWithoutElse(Probe(), null)\n\
        \x20   return if (a == 103 && b == 100 && c == 102 && d == 0) \"OK\" else \"F:$a/$b/$c/$d\"\n\
        }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "discarded_unit_use_branch");
}

#[test]
fn a_unit_use_statement_on_one_branch_of_a_suspend_function_matches_kotlinc() {
    const SOURCE: &str =
        "import kotlin.coroutines.Continuation\n\
        import kotlin.coroutines.EmptyCoroutineContext\n\
        import kotlin.coroutines.startCoroutine\n\
        suspend fun send(text: String?): Int {\n\
        \x20   val out = java.io.ByteArrayOutputStream()\n\
        \x20   if (text != null) { out.use { it.write(text.toByteArray()) } } else { out.close() }\n\
        \x20   return out.size()\n\
        }\n\
        fun drive(text: String?): Int {\n\
        \x20   var result = -1\n\
        \x20   suspend { send(text) }.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
        \x20   return result\n\
        }\n\
        fun box(): String {\n\
        \x20   val a = drive(\"abcd\")\n\
        \x20   val b = drive(null)\n\
        \x20   return if (a == 4 && b == 0) \"OK\" else \"F:$a/$b\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SOURCE, "discarded_unit_use_suspend_branch");
}

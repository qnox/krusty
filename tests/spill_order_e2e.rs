//! A suspension spills references before primitives and restores them in reverse, as kotlinc does.
//!
//! kotlinc's `CoroutineTransformerMethodVisitor` spills every reference in slot order, then every
//! primitive (`referencesToSpill` before `primitivesToSpill`), each store inserted before the
//! suspending call. It inserts each restore right after the resume label, so the restores run in the
//! reverse of that order. krusty spilled and restored in plain slot order, interleaving the kinds.
use std::path::PathBuf;

use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "import kotlin.coroutines.*\n\
    import kotlin.coroutines.intrinsics.*\n\
    class SpillPayload(val name: String)\n\
    var parked: Continuation<Int>? = null\n\
    suspend fun step(): Int = suspendCoroutineUninterceptedOrReturn { continuation ->\n\
    \x20   parked = continuation\n\
    \x20   COROUTINE_SUSPENDED\n\
    }\n\
    fun inspect(payload: SpillPayload): Int = payload.name.length\n\
    suspend fun mixed(): Int {\n\
    \x20   val count = step()\n\
    \x20   val second = SpillPayload(\"bc\")\n\
    \x20   val more = step()\n\
    \x20   return count + more + inspect(second)\n\
    }\n";

/// The spill-field reads and writes of `member`, in order.
fn spill_field_accesses(disassembly: &str, member: &str) -> Vec<String> {
    method_instructions(disassembly, member)
        .iter()
        .filter_map(|insn| {
            let access = if insn.contains("putfield") {
                "put"
            } else if insn.contains("getfield") {
                "get"
            } else {
                return None;
            };
            let field = insn.split("Field ").nth(1)?.split(':').next()?;
            let field = field.rsplit('.').next()?;
            (field.starts_with("L$") || field.starts_with("I$"))
                .then(|| format!("{access} {field}"))
        })
        .collect()
}

#[test]
fn a_suspension_spills_references_first_and_restores_in_reverse_like_kotlinc() {
    let Some(built) = compare_with_kotlinc_plugin(
        "SpillOrder",
        SOURCE,
        "SpillOrderKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let member = "java.lang.Object mixed(";
    let reference = spill_field_accesses(&built.reference, member);
    assert_eq!(
        reference,
        ["put L$0", "put I$0", "get I$0", "get L$0"],
        "kotlinc's own order, spelled out so a change in it is visible here"
    );
    assert_eq!(spill_field_accesses(&built.krusty, member), reference);
}

#[test]
fn a_coroutine_with_reordered_spills_still_resumes() {
    let java_home = common::java_home();
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let source = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   var result = -1\n\
         \x20   val body: suspend () -> Unit = {{ result = mixed() }}\n\
         \x20   body.startCoroutine(Continuation(EmptyCoroutineContext) {{ it.getOrThrow() }})\n\
         \x20   parked!!.resume(3)\n\
         \x20   parked!!.resume(4)\n\
         \x20   return if (result == 9) \"OK\" else \"result $result\"\n\
         }}\n"
    );
    assert_eq!(
        common::compile_and_run_box(&source, "Main", &[common::stdlib_jar()], Some(&jdk))
            .as_deref(),
        Some("OK")
    );
}

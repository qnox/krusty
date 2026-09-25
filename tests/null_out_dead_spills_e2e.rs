//! A spilled reference nothing reads after a suspension goes through the stdlib probe, as in kotlinc.
//!
//! kotlinc spills every local still in scope at a suspension point, so the debugger can show it. A
//! reference that is dead there is spilled through `SpillingKt.nullOutSpilledVariable`, which
//! returns `null`: the field no longer keeps the object alive (`shouldSpillNull` in
//! `CoroutineTransformerMethodVisitor`). A primitive, or a reference still read later, is stored as it
//! is. krusty stored every spilled value as it was.
use std::path::PathBuf;

use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "import kotlin.coroutines.*\n\
    import kotlin.coroutines.intrinsics.*\n\
    class Box(val name: String)\n\
    var parked: Continuation<Int>? = null\n\
    suspend fun step(): Int = suspendCoroutineUninterceptedOrReturn { continuation ->\n\
    \x20   parked = continuation\n\
    \x20   COROUTINE_SUSPENDED\n\
    }\n\
    fun inspect(box: Box): Int = box.name.length\n\
    suspend fun demo(): Int {\n\
    \x20   val first = Box(\"a\")\n\
    \x20   inspect(first)\n\
    \x20   val count = step()\n\
    \x20   val second = Box(\"bc\")\n\
    \x20   val more = step()\n\
    \x20   return count + more + inspect(second)\n\
    }\n";

/// Each spill store of `member`, in order: the continuation field it writes and whether the value
/// went through `nullOutSpilledVariable` first.
fn spill_stores(disassembly: &str, member: &str) -> Vec<(String, bool)> {
    let body = method_instructions(disassembly, member);
    body.iter()
        .enumerate()
        .filter_map(|(at, insn)| {
            let field = insn.split("Field ").nth(1)?.split(':').next()?;
            let field = field.rsplit('.').next()?;
            (insn.contains("putfield") && field.starts_with("L$")).then(|| {
                let probed = at
                    .checked_sub(1)
                    .and_then(|previous| body.get(previous))
                    .is_some_and(|previous| previous.contains("nullOutSpilledVariable"));
                (field.to_string(), probed)
            })
        })
        .collect()
}

#[test]
fn a_dead_spilled_reference_goes_through_the_probe_like_kotlinc() {
    let Some(built) = compare_with_kotlinc_plugin(
        "NullOutDeadSpills",
        SOURCE,
        "NullOutDeadSpillsKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let member = "java.lang.Object demo(";
    let reference = spill_stores(&built.reference, member);
    assert!(
        reference.iter().any(|(_, probed)| *probed),
        "kotlinc's own spill of the dead `first`: {reference:?}"
    );
    assert_eq!(spill_stores(&built.krusty, member), reference, "{member}");
}

#[test]
fn a_coroutine_with_probed_spills_still_resumes() {
    let java_home = common::java_home();
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let source = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   var result = -1\n\
         \x20   val body: suspend () -> Unit = {{ result = demo() }}\n\
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

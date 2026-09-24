//! A state machine passes its continuation to a suspending call as it is, as kotlinc does.
//!
//! The machine's continuation local holds its own continuation class, already a `Continuation`.
//! kotlinc appends it to each suspending call after codegen (`aload $continuation; invoke…`), with
//! no cast; krusty cast it to `kotlin/coroutines/Continuation` first.
use std::path::PathBuf;

use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "import kotlin.coroutines.*\n\
    import kotlin.coroutines.intrinsics.*\n\
    var parked: Continuation<Int>? = null\n\
    suspend fun step(): Int = suspendCoroutineUninterceptedOrReturn { continuation ->\n\
    \x20   parked = continuation\n\
    \x20   COROUTINE_SUSPENDED\n\
    }\n\
    suspend fun twice(): Int {\n\
    \x20   val first = step()\n\
    \x20   return first + step()\n\
    }\n";

/// The casts to `kotlin/coroutines/Continuation` in `member`.
fn continuation_casts(disassembly: &str, member: &str) -> usize {
    method_instructions(disassembly, member)
        .iter()
        .filter(|insn| {
            insn.contains("checkcast") && insn.ends_with("class kotlin/coroutines/Continuation")
        })
        .count()
}

#[test]
fn a_machine_passes_its_continuation_uncast_like_kotlinc() {
    let Some(built) = compare_with_kotlinc_plugin(
        "UncastContinuationOperand",
        SOURCE,
        "UncastContinuationOperandKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let member = "java.lang.Object twice(";
    assert!(
        !method_instructions(&built.reference, member).is_empty(),
        "kotlinc: {member} not found"
    );
    assert!(
        !method_instructions(&built.krusty, member).is_empty(),
        "krusty: {member} not found"
    );
    assert_eq!(continuation_casts(&built.reference, member), 0, "kotlinc");
    assert_eq!(continuation_casts(&built.krusty, member), 0, "krusty");
}

#[test]
fn a_machine_with_an_uncast_continuation_still_resumes() {
    let java_home = common::java_home();
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let source = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   var result = -1\n\
         \x20   val body: suspend () -> Unit = {{ result = twice() }}\n\
         \x20   body.startCoroutine(Continuation(EmptyCoroutineContext) {{ it.getOrThrow() }})\n\
         \x20   parked!!.resume(3)\n\
         \x20   parked!!.resume(4)\n\
         \x20   return if (result == 7) \"OK\" else \"result $result\"\n\
         }}\n"
    );
    assert_eq!(
        common::compile_and_run_box(&source, "Main", &[common::stdlib_jar()], Some(&jdk))
            .as_deref(),
        Some("OK")
    );
}

/// A classpath `@JvmStatic` callable is selected as a member with a source receiver, then emitted as
/// a static method with that receiver dropped. Its continuation is still argument-owned provenance;
/// routing this branch through descriptor-only adaptation would reintroduce `checkcast Continuation`.
#[test]
fn a_jvm_static_suspend_call_keeps_its_recorded_continuation_uncast() {
    const LIBRARY: &str = "package boundary\n\
        object StaticSuspendBoundary {\n\
        \x20   @JvmStatic suspend fun step(value: Int): Int = value\n\
        }\n\
        suspend fun String.extensionStep(value: Int): Int = value + length\n";
    const CALLER: &str = "import boundary.StaticSuspendBoundary\n\
        import boundary.extensionStep\n\
        suspend fun twiceStatic(): Int {\n\
        \x20   val first = StaticSuspendBoundary.step(3)\n\
        \x20   return first + StaticSuspendBoundary.step(4)\n\
        }\n\
        suspend fun String.twiceExtension(): Int {\n\
        \x20   val first = extensionStep(3)\n\
        \x20   return first + extensionStep(4)\n\
        }\n";
    let Some(library) = common::compile_lib("uncast-jvm-static-boundary", LIBRARY) else {
        return;
    };
    let Some(built) = compare_with_kotlinc_plugin(
        "JvmStaticContinuationCaller",
        CALLER,
        "JvmStaticContinuationCallerKt",
        &[common::stdlib_jar(), library],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let member = "java.lang.Object twiceStatic(";
    assert!(
        !method_instructions(&built.reference, member).is_empty(),
        "kotlinc: {member} not found"
    );
    assert!(
        !method_instructions(&built.krusty, member).is_empty(),
        "krusty: {member} not found"
    );
    assert_eq!(continuation_casts(&built.reference, member), 0, "kotlinc");
    assert_eq!(continuation_casts(&built.krusty, member), 0, "krusty");

    // A classpath extension is selected with its receiver separate from checked arguments, but its
    // static descriptor physically prepends that receiver. The recorded continuation index must be
    // interpreted after that one non-argument operand.
    let extension_member = "java.lang.Object twiceExtension(";
    assert!(
        !method_instructions(&built.reference, extension_member).is_empty(),
        "kotlinc: {extension_member} not found"
    );
    assert!(
        !method_instructions(&built.krusty, extension_member).is_empty(),
        "krusty: {extension_member} not found"
    );
    assert_eq!(
        continuation_casts(&built.reference, extension_member),
        0,
        "kotlinc"
    );
    assert_eq!(
        continuation_casts(&built.krusty, extension_member),
        0,
        "krusty"
    );
}

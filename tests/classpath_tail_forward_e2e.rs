//! A suspend function whose body is a tail call to a dependency forwards its continuation, as
//! kotlinc does.
//!
//! `override suspend fun diff(config: String): Answer = delegate.diff(config)` needs no state
//! machine: kotlinc passes `$completion` to the callee and returns its `Object` result
//! (`invokeinterface …; areturn`). krusty did this for a callee compiled in the same module, but a
//! dependency call's erased result carries a coercion to its declared type, which hid the tail
//! call, as did the bare `return` a `Unit` member written `= unitCall(…)` ends with. Each such member
//! got a continuation class and a one-state machine.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const LIBRARY: &str = "package dependency\n\
    @JvmInline value class ParcelId(val raw: Int)\n\
    class Answer(val count: Int)\n\
    interface Engine {\n\
    \x20   suspend fun diff(config: String): Answer\n\
    \x20   suspend fun count(): Int\n\
    \x20   suspend fun parcel(): ParcelId\n\
    \x20   suspend fun forget(id: String)\n\
    }\n";

const SOURCE: &str = "import dependency.*\n\
    class Forwarding(private val delegate: Engine) : Engine {\n\
    \x20   override suspend fun diff(config: String): Answer = delegate.diff(config)\n\
    \x20   override suspend fun count(): Int = delegate.count()\n\
    \x20   override suspend fun parcel(): ParcelId = delegate.parcel()\n\
    \x20   override suspend fun forget(id: String) = delegate.forget(id)\n\
    }\n";

#[test]
fn a_tail_call_to_a_dependency_forwards_its_continuation_like_kotlinc() {
    let Some(library) = common::compile_libs("classpath-tail-forward", &[("Library", LIBRARY)])
    else {
        eprintln!("skipping: dependency did not build");
        return;
    };
    let Some(built) = compare_with_kotlinc_plugin(
        "ClasspathTailForward",
        SOURCE,
        "Forwarding",
        &[common::stdlib_jar(), library],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "java.lang.Object diff(",
        "java.lang.Object count(",
        "java.lang.Object parcel-",
        "java.lang.Object forget(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

#[test]
fn a_forwarded_suspension_still_resumes() {
    let main = format!(
        "{SOURCE}\
         import kotlin.coroutines.*\n\
         import kotlin.coroutines.intrinsics.*\n\
         var parked: Continuation<Answer>? = null\n\
         class Parking : Engine {{\n\
         \x20   override suspend fun diff(config: String): Answer =\n\
         \x20       suspendCoroutineUninterceptedOrReturn {{ parked = it; COROUTINE_SUSPENDED }}\n\
         \x20   override suspend fun count(): Int = 7\n\
         \x20   override suspend fun parcel(): ParcelId = ParcelId(9)\n\
         \x20   override suspend fun forget(id: String) {{}}\n\
         }}\n\
         fun box(): String {{\n\
         \x20   var result = \"none\"\n\
         \x20   val forwarding = Forwarding(Parking())\n\
         \x20   val body: suspend () -> Unit = {{\n\
         \x20       val answer = forwarding.diff(\"x\")\n\
         \x20       result = \"${{answer.count}}:${{forwarding.count()}}\"\n\
         \x20   }}\n\
         \x20   body.startCoroutine(Continuation(EmptyCoroutineContext) {{ it.getOrThrow() }})\n\
         \x20   parked!!.resume(Answer(5))\n\
         \x20   return if (result == \"5:7\") \"OK\" else \"result $result\"\n\
         }}\n"
    );
    assert_eq!(
        common::expect_box_run_against_ref("classpath-tail-forward-run", LIBRARY, &main).as_deref(),
        Some("OK")
    );
}

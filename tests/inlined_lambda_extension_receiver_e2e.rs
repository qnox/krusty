//! An inline-only callee's stored receiver survives in a regenerated object's method.
//!
//! kotlinc stores an inline call's extension receiver to a local of its own even when it already
//! lives in one, and its optimizer keeps the store and the reload: `r.getOrThrow()` is
//! `aload r; astore t; aload t; invokestatic throwOnFailure`. kotlinc's class writer optimizes each
//! method once, a regenerated anonymous object's (`Continuation { it.getOrThrow() }`) as much as a
//! source function's. The passes are not idempotent: a second run folds the pair.

use super::common;

const SOURCE: &str = "\
import kotlin.coroutines.*\n\
\n\
fun plain(r: Result<Unit>) { r.getOrThrow() }\n\
\n\
fun viaRun(r: Result<Unit>) { run { r.getOrThrow() } }\n\
\n\
fun viaLet(r: Result<Unit>) { r.let { it.getOrThrow() } }\n\
\n\
fun viaContinuation(): Continuation<Unit> =\n\
    Continuation(EmptyCoroutineContext) { it.getOrThrow() }\n\
\n\
fun box(): String {\n\
    plain(Result.success(Unit))\n\
    viaRun(Result.success(Unit))\n\
    viaLet(Result.success(Unit))\n\
    viaContinuation().resumeWith(Result.success(Unit))\n\
    return \"OK\"\n\
}\n";

#[test]
fn an_inlined_lambdas_receiver_argument_is_stored_like_kotlinc() {
    common::expect_box_ok_with_stdlib(SOURCE, "InlinedLambdaExtensionReceiver");
    let built =
        common::classes_against_kotlinc_module(&[("InlinedLambdaExtensionReceiver.kt", SOURCE)]);
    for class in [
        "InlinedLambdaExtensionReceiverKt",
        "InlinedLambdaExtensionReceiverKt$viaContinuation$$inlined$Continuation$1",
    ] {
        assert!(
            built.reference.contains_key(class),
            "kotlinc writes {class}"
        );
    }
    assert_eq!(built.differences(), Vec::<String>::new());
}

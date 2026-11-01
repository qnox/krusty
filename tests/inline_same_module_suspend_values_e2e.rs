//! A same-module inline function with a `noinline` suspend function-typed parameter is inlined from
//! its compiled body, as kotlinc inlines it: the parameter's argument is a value the body only
//! passes on, so the call's suspend lambda is the call site's own object and nothing in the callee
//! suspends inside the caller.

use super::common;

const DECLARATION: &str = r#"inline fun <T> deferredBy(noinline block: suspend () -> T): suspend () -> T = block

inline fun <T> pairedWith(value: T, noinline block: suspend (T) -> T): suspend () -> T = { block(value) }
"#;

const CALLER: &str = r#"import kotlin.coroutines.*

suspend fun pause(value: Int): Int = suspendCoroutine { it.resume(value) }

fun later(): suspend () -> Int = deferredBy { pause(20) + 1 }

suspend fun now(): Int = deferredBy { pause(1) }() + pairedWith(2) { pause(it) * 10 }()

fun builder(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

fun box(): String {
    var result = 0
    builder { result = later()() + now() }
    return if (result == 42) "OK" else "result $result"
}
"#;

#[test]
fn a_noinline_suspend_parameters_callee_is_inlined_from_the_compiled_body_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Declaration.kt", DECLARATION),
        ("Caller.kt", CALLER),
    ]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    // `DeclarationKt$pairedWith$1` lacks only the `SourceDebugExtension` kotlinc gives a lambda an
    // inline function declares, and `CallerKt$builder$$inlined$Continuation$1` is the library
    // `Continuation { }` copy: neither is this route's.
    for name in [
        "CallerKt",
        "CallerKt$box$1",
        "CallerKt$later$1",
        "CallerKt$now$1",
        "CallerKt$now$2",
        "CallerKt$now$3",
        "DeclarationKt",
    ] {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn a_noinline_suspend_parameters_callee_inlined_from_the_compiled_body_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[("Declaration.kt", DECLARATION), ("Caller.kt", CALLER)],
        "CallerKt",
    );
}

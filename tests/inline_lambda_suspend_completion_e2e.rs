//! The continuation an inline suspend dependency call receives inside an inline lambda's body.
//!
//! kotlinc compiles an inline lambda's body as a method of its own, which reads the enclosing
//! function's continuation as a value the inliner binds rather than as a local of the caller. The
//! inlined callee's continuation parameter therefore gets a local of its own, `$completion$iv`,
//! which is spilled at the callee's suspension like any other live local. Called directly from the
//! function's body, the callee reads the caller's continuation local and gets no such local.
//! Both shapes are written below, for a plain inline lambda and for a suspend one.
//!
//! PLATFORM-SPECIFIC: Native and Wasm do not lower
//! `kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED.<get-COROUTINE_SUSPENDED>` yet. Its KLIB
//! body reads an enum entry, so this program stays on the JVM leaf.

use super::common;

fn with_program(test: impl FnOnce(&str, &str)) {
    let modules = krusty::conformance::split_modules(include_str!(
        "fixtures/inline_lambda_suspend_completion/inlineLambdaSuspendCompletion.kt"
    ))
    .expect("the complete inline-lambda continuation program");
    let [producer, consumer] = modules.as_slice() else {
        panic!("producer and consumer modules")
    };
    assert_eq!(consumer.deps, std::slice::from_ref(&producer.name));
    let [(library_stem, library)] = producer.files.as_slice() else {
        panic!("one complete producer source")
    };
    let [(main_stem, main)] = consumer.files.as_slice() else {
        panic!("one complete consumer source")
    };
    assert_eq!(library_stem, "Lib");
    assert_eq!(main_stem, "Main");
    test(library, main);
}

/// Each call resumes through its suspension with the callee's continuation in its own local. The
/// reference compiler's consumer, built against the same krusty-built dependency, establishes the
/// result first.
#[test]
fn an_inline_suspend_call_in_an_inline_lambda_resumes() {
    with_program(|lib, main| {
        let library = common::compile_lib("inline_lambda_suspend_completion", lib)
            .expect("krusty builds the library");
        assert_eq!(
            common::kotlinc_box_result_with_classpath(main, &[library]),
            "OK",
            "kotlinc's consumer must succeed"
        );
        assert_eq!(
            common::expect_box_run_against("inline_lambda_suspend_completion", lib, main)
                .as_deref(),
            Some("OK")
        );
    });
}

/// PLATFORM-SPECIFIC: JVM class bytes, including spill fields and debug-local scopes, must match
/// the reference compiler for every listed class.
#[test]
fn an_inline_suspend_call_in_an_inline_lambda_matches_kotlinc() {
    with_program(|lib, main| {
        let library = common::compile_lib("inline_lambda_suspend_completion", lib)
            .expect("krusty builds the library");
        let classpath = [library, common::stdlib_jar()];
        // `Placed` itself is compared through its continuation only, and is not byte-identical: when
        // the inline call is the placed suspend lambda's whole value, kotlinc ends the expansion with
        // one more `nop` than krusty writes, a separate gap. `Continued` continues the lambda after
        // the same call, which avoids it, so its host method and the `$completion$iv` local and
        // scope the placed lambda gives the call are compared whole.
        for class in [
            "Api",
            "Api$direct$1",
            "Api$inLambda$1",
            "Placed$inSuspendLambda$1",
            "Continued",
            "Continued$inSuspendLambda$1",
        ] {
            let compared =
                common::compare_with_kotlinc_plugin("Main", main, class, &classpath, "25", &[])
                    .expect("reference kotlinc and javap must be available");
            assert!(
                compared.krusty_bytes == compared.reference_bytes,
                "{class} differs\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
                compared.reference,
                compared.krusty
            );
        }
    });
}

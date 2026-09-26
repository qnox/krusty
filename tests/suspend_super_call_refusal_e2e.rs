//! A `super` call to a `suspend` member on an ENCLOSING instance (`super<Base>@Outer.f()` from an
//! inner class) is REFUSED by the front end, not emitted.
//!
//! Such a call reaches the outer instance through a generated nonvirtual bridge on the outer
//! class, and that bridge is not a suspend function: emitting it anyway kept the unlinkable
//! `invokespecial Base.f:()Ljava/lang/String;` with no diagnostic, which is the one outcome the
//! project's rule forbids. A super call on the current instance is an ordinary suspension point
//! (`suspend_super_calls_e2e.rs`).
//!
//! The refusal belongs to the CHECKER, which selected the target and its receiver and has its
//! suspend shape in hand. The fixture asserts the COMPLETE ordered diagnostic ledger, with
//! positions. If it stops being refused, the bridge has learned the shape and the test should
//! become a round-trip test.

use super::common;

/// Front-end diagnostics of a source SET as `file:line:col: severity: message`, in emission order.
///
/// A refusal's position is half its contract — a message alone cannot tell a diagnostic anchored on
/// the `super` call from one anchored on the enclosing declaration, or on the wrong file.
fn located_diagnostics(sources: &[&str], cp_jars: &[std::path::PathBuf]) -> Vec<String> {
    let jdk = common::jdk_modules();
    let cp = common::cached_classpath(cp_jars, Some(jdk.as_path()));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp).expect("JVM provider initialization"),
    );
    let inputs = sources
        .iter()
        .map(|source| krusty::frontend::SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    let mut diags = krusty::diag::DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &krusty::features::LangFeatures::new(),
        |_, _| {},
        &mut diags,
    );
    let _ = krusty::compiler::check_frontend_only(analysis, &mut diags);
    diags
        .diags
        .iter()
        .map(|diagnostic| {
            let source = sources
                .get(diagnostic.file as usize)
                .copied()
                .unwrap_or_default();
            let (line, column) = krusty::diag::line_col(source, diagnostic.span.lo);
            let severity = match diagnostic.severity {
                krusty::diag::Severity::Error => "error",
                _ => "warning",
            };
            format!(
                "{}:{line}:{column}: {severity}: {}",
                diagnostic.file, diagnostic.msg
            )
        })
        .collect()
}

fn refusal(file: u32, line: u32, column: u32, name: &str) -> String {
    format!(
        "{file}:{line}:{column}: error: krusty: a super call to the suspend member '{name}' is \
         not supported yet."
    )
}

/// The LABELED enclosing dispatch. This is the shape a backend guard could not see: the call is
/// wrapped in a generated enclosing-dispatch bridge, which is not itself recorded as suspend, so
/// the suspension point never reached the guard and the bridge kept the unlinkable
/// `invokespecial Base.f:()Ljava/lang/String;`.
#[test]
fn a_labeled_enclosing_super_call_is_refused() {
    let source = "open class Base {\n\
                  \x20   open suspend fun f(): String = \"O\"\n\
                  }\n\
                  \n\
                  open class Outer : Base() {\n\
                  \x20   override suspend fun f(): String = \"override\"\n\
                  \n\
                  \x20   inner class Inner {\n\
                  \x20       suspend fun run(): String = super<Base>@Outer.f() + \"K\"\n\
                  \x20   }\n\
                  }\n";
    assert_eq!(
        located_diagnostics(&[source], &[common::stdlib_jar()]),
        vec![refusal(0, 9, 37, "f")],
    );
}

/// A `super` call to an ORDINARY member is untouched: the refusal keys on the TARGET being suspend,
/// not on the dispatch being non-virtual.
#[test]
fn a_super_call_to_an_ordinary_member_still_runs() {
    assert_eq!(
        common::compile_and_run_box(
            "abstract class A {\n\
             \x20   open fun f(): String = \"O\"\n\
             }\n\
             \n\
             class B : A() {\n\
             \x20   override fun f(): String = super.f() + \"K\"\n\
             }\n\
             \n\
             fun box(): String = B().f()\n",
            "SuperOrdinaryMember",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path()),
        )
        .as_deref(),
        Some("OK"),
    );
}

/// …and an ordinary VIRTUAL suspend call is untouched too.
#[test]
fn a_virtual_suspend_call_still_runs() {
    assert_eq!(
        common::compile_and_run_box(
            "import kotlin.coroutines.*\n\
             import kotlin.coroutines.intrinsics.*\n\
             \n\
             abstract class A {\n\
             \x20   open suspend fun f(): String = \"O\"\n\
             }\n\
             \n\
             class B : A() {\n\
             \x20   override suspend fun f(): String = \"OK\"\n\
             }\n\
             \n\
             fun builder(c: suspend () -> String): String {\n\
             \x20   var result = \"\"\n\
             \x20   c.startCoroutine(object : Continuation<String> {\n\
             \x20       override val context = EmptyCoroutineContext\n\
             \x20       override fun resumeWith(value: Result<String>) {\n\
             \x20           result = value.getOrThrow()\n\
             \x20       }\n\
             \x20   })\n\
             \x20   return result\n\
             }\n\
             \n\
             fun box(): String = builder { (B() as A).f() }\n",
            "VirtualSuspendCall",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path()),
        )
        .as_deref(),
        Some("OK"),
    );
}

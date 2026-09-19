//! A `super` call to a `suspend` member is REFUSED by the front end, not emitted.
//!
//! Threading a continuation through a NON-VIRTUAL dispatch and resuming back into it is not
//! modeled. Emitting it anyway produced an `invokespecial` naming the SOURCE descriptor
//! (`A.f:()Ljava/lang/String;`) against a declaration that is
//! `A.f:(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;`:
//!
//! ```text
//! NoSuchMethodError: 'java.lang.String A.suspendHere()'
//! ```
//!
//! An unlinkable artifact emitted without a diagnostic, which is the one outcome the project's rule
//! forbids. The refusal belongs to the CHECKER, which selected the target and has its suspend shape
//! in hand: a backend traversal has to rediscover the fact from a realization that no longer names
//! it, and only recognizes the call shapes that reach one particular node — the same source with
//! its superclass in a SIBLING FILE, in a DEPENDENCY, or behind an `@Outer`-labeled enclosing
//! dispatch reaches a different one and slipped through, still emitting the unlinkable call.
//!
//! Every fixture below therefore asserts the COMPLETE ordered diagnostic ledger, with positions:
//! one message, at the `super` call, whatever the spelling and wherever the declaration lives. If
//! one of them stops being refused, the state machine has learned the shape and the test should
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

/// The corpus shape, reduced: an override whose body calls `super.f()` on a suspend member.
#[test]
fn a_direct_super_call_to_a_suspend_member_is_refused() {
    let source = "abstract class A {\n\
                  \x20   open suspend fun f(): String = \"O\"\n\
                  }\n\
                  \n\
                  class B : A() {\n\
                  \x20   override suspend fun f(): String = super.f() + \"K\"\n\
                  }\n";
    assert_eq!(
        located_diagnostics(&[source], &[common::stdlib_jar()]),
        vec![refusal(0, 6, 40, "f")],
    );
}

/// With a parameter, which is where the descriptor divergence is visible: the realized call said
/// `(Ljava/lang/String;)Ljava/lang/String;` where the declaration says
/// `(Ljava/lang/String;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;`.
#[test]
fn a_super_call_with_a_parameter_is_refused() {
    let source = "abstract class A {\n\
                  \x20   open suspend fun f(v: String): String = v\n\
                  }\n\
                  \n\
                  class B : A() {\n\
                  \x20   override suspend fun f(v: String): String = super.f(v) + \"K\"\n\
                  }\n";
    assert_eq!(
        located_diagnostics(&[source], &[common::stdlib_jar()]),
        vec![refusal(0, 6, 49, "f")],
    );
}

/// The TYPED spelling `super<A>.f()`, which reaches selection by a different route.
#[test]
fn a_typed_super_call_is_refused() {
    let source = "interface I {\n\
                  \x20   suspend fun f(): String = \"O\"\n\
                  }\n\
                  \n\
                  class B : I {\n\
                  \x20   override suspend fun f(): String = super<I>.f() + \"K\"\n\
                  }\n";
    assert_eq!(
        located_diagnostics(&[source], &[common::stdlib_jar()]),
        vec![refusal(0, 6, 40, "f")],
    );
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

/// The superclass in a SIBLING SOURCE FILE. The backend guard recognized only same-file targets,
/// because it recovered the suspend fact through a predeclaration table that holds this file's own
/// functions; moving the superclass one file over made the same source compile and emit.
#[test]
fn a_super_call_across_source_files_is_refused() {
    let library = "abstract class A {\n\
                   \x20   open suspend fun f(): String = \"O\"\n\
                   }\n";
    let main = "class B : A() {\n\
                \x20   override suspend fun f(): String = super.f() + \"K\"\n\
                }\n";
    assert_eq!(
        located_diagnostics(&[library, main], &[common::stdlib_jar()]),
        vec![refusal(1, 2, 40, "f")],
    );
}

/// The superclass in a DEPENDENCY, compiled by the reference compiler. Its declaration has no
/// source identity in this compilation at all, which is the other origin a backend recovery misses.
#[test]
fn a_super_call_into_a_dependency_is_refused() {
    let Some(library) = common::compile_lib_ref(
        "suspend_super_dependency",
        "abstract class A {\n\
         \x20   open suspend fun f(): String = \"O\"\n\
         }\n",
    ) else {
        panic!("reference kotlinc unavailable under the test harness");
    };
    let main = "class B : A() {\n\
                \x20   override suspend fun f(): String = super.f() + \"K\"\n\
                }\n";
    assert_eq!(
        located_diagnostics(&[main], &[library, common::stdlib_jar()]),
        vec![refusal(0, 2, 40, "f")],
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

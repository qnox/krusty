//! PLATFORM-SPECIFIC: the handoff selects the stdlib declarations a suspend function's lowering
//! calls from the exact Wasm and Native stdlib KLIBs.

use krusty::compilation_target::CompilationTarget;
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const SUSPENDING: &str = "suspend fun leaf(): Int = 1\nfun plain(): Int = 2\n";

/// What one file's handoff selected: whether it holds the runtime, and whether the frozen
/// facts answer for both of its identities.
#[derive(Debug, Eq, PartialEq)]
struct Selected {
    runtime: bool,
    facts: bool,
}

#[test]
fn a_suspending_file_selects_and_freezes_the_coroutine_runtime_on_every_klib_target() {
    let mut checked = 0;
    for (target, semantics) in [
        (CompilationTarget::WasmJs, KlibStdlibSemantics::Wasm),
        (CompilationTarget::WasmWasi, KlibStdlibSemantics::Wasm),
        (CompilationTarget::Native, KlibStdlibSemantics::Native),
    ] {
        let Some(selected) = handoff(target, semantics, SUSPENDING) else {
            continue;
        };
        checked += 1;
        assert_eq!(
            selected,
            Selected {
                runtime: true,
                facts: true
            },
            "{target:?}"
        );
    }
    assert!(checked > 0, "no target stdlib is provisioned");
}

#[test]
fn a_file_without_a_suspend_function_selects_no_runtime() {
    for (target, semantics) in [
        (CompilationTarget::WasmJs, KlibStdlibSemantics::Wasm),
        (CompilationTarget::Native, KlibStdlibSemantics::Native),
    ] {
        let Some(selected) = handoff(target, semantics, "fun plain(): Int = 2\n") else {
            continue;
        };
        assert_eq!(
            selected,
            Selected {
                runtime: false,
                facts: false
            },
            "{target:?}"
        );
    }
}

fn handoff(
    target: CompilationTarget,
    semantics: KlibStdlibSemantics,
    source: &str,
) -> Option<Selected> {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(target)?;
    let libraries =
        KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(stdlib, semantics)]).ok()?;
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &[krusty::source::SourceInput::kotlin(source).with_file_stem("Main")],
        krusty::frontend::PlatformProvider::new(target, Box::new(libraries.for_compilation())),
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );

    struct Probe(
        CompilationTarget,
        std::rc::Rc<std::cell::RefCell<Vec<Selected>>>,
    );

    impl krusty::compiler::Backend for Probe {
        type State = ();

        fn compilation_target(&self) -> CompilationTarget {
            self.0
        }

        fn lower_ir_file(
            &self,
            file: krusty::compiler::CheckedIrFile<'_>,
            _state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<krusty::compiler::Artifact> {
            let runtime = file.ir.coroutine_runtime;
            self.1.borrow_mut().push(Selected {
                runtime: runtime.is_some(),
                facts: runtime.is_some_and(|runtime| {
                    file.callables.property(runtime.suspended).is_some()
                        && file.callables.callable(runtime.get_or_throw).is_some()
                }),
            });
            Vec::new()
        }

        fn finalize(
            &self,
            _state: Self::State,
            _module_name: &str,
        ) -> Vec<krusty::compiler::Artifact> {
            Vec::new()
        }
    }

    let seen = std::rc::Rc::default();
    let stems = ["Main".to_owned()];
    krusty::compiler::emit_analyzed(
        analysis,
        &stems,
        &Probe(target, std::rc::Rc::clone(&seen)),
        "main",
        &mut diags,
    );
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|d| d.msg.clone())
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
    let mut seen = seen.take();
    assert_eq!(seen.len(), 1);
    seen.pop()
}

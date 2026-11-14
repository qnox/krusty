//! PLATFORM-SPECIFIC: the handoff selects the stdlib declarations a suspend function's lowering
//! calls from the exact Wasm and Native stdlib KLIBs, for a backend that lowers coroutines on
//! common IR.

use krusty::compilation_target::CompilationTarget;
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const SUSPENDING: &str = "suspend fun leaf(): Int = 1\nfun plain(): Int = 2\n";
const PLAIN: &str = "fun plain(): Int = 2\n";

const TARGETS: [(CompilationTarget, KlibStdlibSemantics); 3] = [
    (CompilationTarget::WasmJs, KlibStdlibSemantics::Wasm),
    (CompilationTarget::WasmWasi, KlibStdlibSemantics::Wasm),
    (CompilationTarget::Native, KlibStdlibSemantics::Native),
];

/// Whether one file's handoff selected the runtime.
#[derive(Debug, Eq, PartialEq)]
struct Selected {
    runtime: bool,
}

const SELECTED: Selected = Selected { runtime: true };

const NONE: Selected = Selected { runtime: false };

#[test]
fn a_suspending_file_selects_the_coroutine_runtime_on_every_klib_target() {
    assert_on_provisioned_targets(SUSPENDING, true, SELECTED);
}

#[test]
fn a_file_without_a_suspend_function_selects_no_runtime() {
    assert_on_provisioned_targets(PLAIN, true, NONE);
}

#[test]
fn a_backend_with_its_own_coroutine_pass_selects_no_runtime() {
    assert_on_provisioned_targets(SUSPENDING, false, NONE);
}

/// Assert what the handoff of `source` selects on every target whose stdlib is provisioned. At
/// least one must be; `KRUSTY_REQUIRE_KLIB` requires every one.
fn assert_on_provisioned_targets(source: &str, common_ir_coroutines: bool, expected: Selected) {
    let mut checked = 0;
    for (target, semantics) in TARGETS {
        let Some(stdlib) = krusty::toolchain::kotlin_stdlib_klib(target) else {
            assert!(
                std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
                "KRUSTY_REQUIRE_KLIB is set but no {target:?} stdlib KLIB is provisioned"
            );
            continue;
        };
        checked += 1;
        let libraries =
            KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(stdlib.clone(), semantics)])
                .unwrap_or_else(|error| panic!("open {}: {error:?}", stdlib.display()));
        assert_eq!(
            handoff(target, &libraries, source, common_ir_coroutines),
            expected,
            "{target:?}"
        );
    }
    assert!(checked > 0, "no target stdlib KLIB is provisioned");
}

fn handoff(
    target: CompilationTarget,
    libraries: &KlibLibraries,
    source: &str,
    common_ir_coroutines: bool,
) -> Selected {
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &[krusty::source::SourceInput::kotlin(source).with_file_stem("Main")],
        krusty::frontend::PlatformProvider::new(target, Box::new(libraries.for_compilation())),
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );

    struct Probe {
        target: CompilationTarget,
        common_ir_coroutines: bool,
        seen: std::rc::Rc<std::cell::RefCell<Vec<Selected>>>,
    }

    impl krusty::compiler::Backend for Probe {
        type State = ();

        fn compilation_target(&self) -> CompilationTarget {
            self.target
        }

        fn common_ir_preparation(&self) -> Option<krusty::backend::CommonIrPreparation> {
            self.common_ir_coroutines
                .then(|| krusty::backend::CommonIrPreparation {
                    counted_loops: krusty::backend::counted_loops::CountedLoopPolicy {
                        style: krusty::backend::counted_loops::CounterLoopStyle::PreTested,
                        inlining: krusty::backend::counted_loops::HeaderInlining::None,
                        until_steps: if self.target == CompilationTarget::WasmJs
                            || self.target == CompilationTarget::WasmWasi
                        {
                            krusty::backend::counted_loops::UntilSteps::Built
                        } else {
                            krusty::backend::counted_loops::UntilSteps::Counted
                        },
                    },
                    enums: match self.target {
                        CompilationTarget::Native => krusty::backend::enums::Contract::KotlinNative,
                        CompilationTarget::WasmJs | CompilationTarget::WasmWasi => {
                            krusty::backend::enums::Contract::KotlinWasm
                        }
                        _ => unreachable!("this probe only visits Native and Wasm targets"),
                    },
                })
        }

        fn lower_ir_file(
            &self,
            file: krusty::compiler::CheckedIrFile<'_>,
            _state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<krusty::compiler::Artifact> {
            self.seen.borrow_mut().push(Selected {
                runtime: file.ir.coroutine_runtime.is_some(),
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
    let probe = Probe {
        target,
        common_ir_coroutines,
        seen: std::rc::Rc::clone(&seen),
    };
    krusty::compiler::emit_analyzed(analysis, &stems, &probe, "main", &mut diags);
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.clone())
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
    let mut seen = seen.take();
    assert_eq!(seen.len(), 1);
    seen.pop().expect("one checked file")
}

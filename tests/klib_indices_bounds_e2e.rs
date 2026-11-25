//! PLATFORM-SPECIFIC: the Native stdlib KLIB's bodies that count the `indices` of a text.
//!
//! kotlinc bounds a counted `for` loop over `indices` with the receiver's `size` or `length`, a
//! member read the serialized loop never makes: it holds only the `indices` getter call.
//! `CharSequence.trimEnd` loops over `this.indices.reversed()`, so lowering its body reads the
//! receiver's `length` through the dependency property's own getter. The portable behavior of the
//! function is pinned against kotlinc by the box corpus; this test pins that the shared KLIB
//! lowering emits the body, with that one read, instead of declining the loop.

use krusty::backend::{Artifact, Backend, CheckedIrFile, CheckedIrUnitKind};
use krusty::diag::DiagSink;
use krusty::ir::{IrCheckedOperation, IrExpr};
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const CONSUMER: &str = "\
fun box(): String {
    if (\"abc  \".trimEnd() != \"abc\") return \"fail\"
    return \"OK\"
}
";

/// The external property reads in each function the dependency support units lower, as
/// `(function, receiver type, result type)`.
struct IndicesProbe {
    native: krusty_native::CraneliftBackend,
    reads: std::cell::RefCell<Vec<(String, String, String)>>,
}

impl Backend for IndicesProbe {
    type State = ();

    fn compilation_target(&self) -> krusty::compilation_target::CompilationTarget {
        self.native.compilation_target()
    }

    fn emits_dependency_support(&self) -> bool {
        self.native.emits_dependency_support()
    }

    fn common_ir_preparation(&self) -> Option<krusty::backend::CommonIrPreparation> {
        self.native.common_ir_preparation()
    }

    fn represents_dependency_classifier(
        &self,
        classifier: krusty::backend::DependencyClassifier,
    ) -> bool {
        self.native.represents_dependency_classifier(classifier)
    }

    fn numbers_slots(&self) -> bool {
        self.native.numbers_slots()
    }

    fn realizes_bodyless_dependency_support(&self) -> bool {
        self.native.realizes_bodyless_dependency_support()
    }

    fn realizes_dependency_intrinsic(
        &self,
        operation: krusty::backend::BackendCompilerIntrinsic,
    ) -> bool {
        self.native.realizes_dependency_intrinsic(operation)
    }

    fn realizes_dependency_getter(
        &self,
        signature: krusty::backend::DependencyGetterSignature<'_>,
    ) -> bool {
        self.native.realizes_dependency_getter(signature)
    }

    fn same_module_inlining(&self) -> krusty::ir::SameModuleInlining {
        self.native.same_module_inlining()
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        _state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        if file.kind == CheckedIrUnitKind::DependencySupport {
            for function in &file.ir.functions {
                let Some(body) = function.body else {
                    continue;
                };
                let mut pending = vec![body];
                while let Some(expression) = pending.pop() {
                    krusty::ir::for_each_child(&file.ir.exprs, expression, &mut |child| {
                        pending.push(child)
                    });
                    if let IrExpr::Checked(checked) = &file.ir.exprs[expression as usize] {
                        if let IrCheckedOperation::ExternalPropertyRead {
                            result,
                            source_receiver,
                            ..
                        } = &**checked
                        {
                            self.reads.borrow_mut().push((
                                function.name.clone(),
                                format!("{source_receiver:?}"),
                                format!("{result:?}"),
                            ));
                        }
                    }
                }
            }
        }
        Vec::new()
    }

    fn finalize(&self, _state: Self::State, _module_name: &str) -> Vec<Artifact> {
        Vec::new()
    }
}

/// The probe after the Native stdlib lowered `consumer`'s dependency support, or `None` without
/// a Native distribution for the host.
fn lowered(consumer: &str) -> Option<IndicesProbe> {
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE").map(std::path::PathBuf::from) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return None;
    };
    let target = krusty_native::NativeTarget::host()?;
    let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        root.join("klib/common/stdlib"),
        KlibStdlibSemantics::Native,
    )])
    .expect("open the Native stdlib KLIB");
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &[krusty::source::SourceInput::kotlin(consumer).with_file_stem("Consumer")],
        krusty::frontend::PlatformProvider::new(
            krusty::compilation_target::CompilationTarget::Native,
            Box::new(libraries.for_compilation()),
        ),
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    let probe = IndicesProbe {
        native: krusty_native::CraneliftBackend::new(target).with_entry(krusty_native::Entry::Box),
        reads: std::cell::RefCell::default(),
    };
    krusty::compiler::emit_analyzed(
        analysis,
        &["Consumer".to_owned()],
        &probe,
        "consumer",
        &mut diags,
    );
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|diag| diag.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    Some(probe)
}

#[test]
fn trim_end_lowers_its_indices_loop_with_the_receivers_length_read() {
    let Some(probe) = lowered(CONSUMER) else {
        return;
    };
    // The loop's one bound: `CharSequence.length`, read inside `trimEnd`'s body.
    assert_eq!(
        probe.reads.into_inner(),
        [(
            "trimEnd".to_owned(),
            "Some(Obj(TypeName(\"kotlin/CharSequence\"), []))".to_owned(),
            "Obj(TypeName(\"kotlin/Int\"), [])".to_owned(),
        )]
    );
}

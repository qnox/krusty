//! Lowering one source, or one file of a source set, through the frontend and the common FIR sink
//! into the `IrFile` a lowering test inspects.

use crate::fir::{DeclarationId, FirBody};
use crate::ir::IrFile;

pub(crate) fn lower_single_source(source: &str, stem: &str) -> IrFile {
    lower_single_source_with_platform(source, stem, Box::new(crate::libraries::EmptySymbolSource))
}

pub(crate) fn lower_single_source_with_jvm_stdlib(source: &str, stem: &str) -> IrFile {
    lower_with_jvm_stdlib_into(source, stem, IrFile::default())
}

/// [`lower_single_source_with_jvm_stdlib`] into `ir`, which carries the backend's lowering choices.
pub(crate) fn lower_with_jvm_stdlib_into(source: &str, stem: &str, ir: IrFile) -> IrFile {
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let platform: Box<dyn crate::libraries::SemanticPlatform> = Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(classpath),
        ))
        .expect("JVM provider initialization"),
    );
    lower_single_source_into(source, stem, platform, ir)
}

pub(crate) fn lower_single_source_with_platform(
    source: &str,
    stem: &str,
    platform: Box<dyn crate::libraries::SemanticPlatform>,
) -> IrFile {
    lower_single_source_into(source, stem, platform, IrFile::default())
}

fn lower_single_source_into(
    source: &str,
    stem: &str,
    platform: Box<dyn crate::libraries::SemanticPlatform>,
    mut ir: IrFile,
) -> IrFile {
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[crate::source::SourceInput::kotlin(source).with_file_stem(stem)],
        crate::frontend::PlatformProvider::jvm(platform),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary =
        streamed.ordinary_body_work(&analysis.files[0], crate::fir::SourceFileId::from_raw(0));
    let (index, mut inline_bodies, mut default_arguments, mut sources) =
        streamed.module.into_parts();
    let retained_inline_count = inline_bodies.len();
    let mut sink =
        super::super::CommonIrBodySink::new(&index, crate::fir::SourceFileId::from_raw(0), &mut ir)
            .unwrap();
    sink.accept_default_arguments(&index, &mut default_arguments)
        .unwrap();
    sink.accept_inline_bodies(&index, &mut inline_bodies)
        .unwrap();
    for work in ordinary {
        let mut indexed_sink = sink.indexed(&index);
        crate::fir::check_and_dispatch_body(
            crate::fir::BodyCheckUnit {
                file: &analysis.files[0],
                info: analysis.types[0].as_ref().expect("checked type info"),
                source: crate::fir::SourceFileId::from_raw(0),
                index: &index,
            },
            work,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut indexed_sink,
        )
        .unwrap();
    }
    if let Err(error) = sink.finish(&index) {
        panic!(
            "common FIR sink failed: {error:?}; callable realizations={:?}; declarations={:?}",
            ir.checked_callable_functions,
            (0..index.declaration_count())
                .map(|raw| {
                    let declaration = DeclarationId::from_raw(raw as u32);
                    (
                        declaration,
                        index.declaration_anchor(declaration),
                        index.declaration_header(declaration),
                        index
                            .callable_for_declaration(declaration)
                            .and_then(|callable| index.callable_name(callable.id)),
                    )
                })
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(inline_bodies.len(), retained_inline_count);
    ir
}

pub(crate) fn lower_source_from_set(sources: &[(&str, &str)], active_source: usize) -> IrFile {
    #[derive(Default)]
    struct DeferredBodies(Vec<(crate::fir::BodyOwnerId, FirBody)>);

    impl crate::fir::CheckedBodySink for DeferredBodies {
        fn accept_finalized(&mut self, owner: crate::fir::BodyOwnerId, body: FirBody) {
            self.0.push((owner, body));
        }
    }

    let inputs = sources
        .iter()
        .map(|(source, stem)| crate::source::SourceInput::kotlin(*source).with_file_stem(*stem))
        .collect::<Vec<_>>();
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let source_id = crate::fir::SourceFileId::from_raw(active_source as u32);
    let ordinary = streamed.ordinary_body_work(&analysis.files[active_source], source_id);
    let (index, inline_bodies, mut default_arguments, mut source_map) =
        streamed.module.into_parts();
    let mut ir = IrFile::default();
    let mut sink = super::super::CommonIrBodySink::new(&index, source_id, &mut ir).unwrap();
    sink.accept_default_arguments(&index, &mut default_arguments)
        .unwrap();
    sink.accept_inline_bodies(&index, &inline_bodies).unwrap();
    for work in ordinary {
        let mut deferred = DeferredBodies::default();
        crate::fir::check_and_dispatch_body(
            crate::fir::BodyCheckUnit {
                file: &analysis.files[active_source],
                info: analysis.types[active_source]
                    .as_ref()
                    .expect("checked type info"),
                source: source_id,
                index: &index,
            },
            work,
            source_map.origins_mut(),
            &mut crate::fir::InlineBodyStore::default(),
            &mut deferred,
        )
        .unwrap();
        for (owner, body) in deferred.0 {
            sink.accept_streamed_body(&index, &inline_bodies, owner, body)
                .unwrap();
        }
    }
    sink.finish(&index).unwrap();
    ir
}

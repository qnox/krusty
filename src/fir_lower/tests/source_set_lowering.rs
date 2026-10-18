//! Multi-source checked-FIR lowering support for common-IR tests.

use super::*;

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
        .map(|(source, stem)| crate::source::SourceInput::kotlin(source).with_file_stem(stem))
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
    let source_id = crate::fir::SourceFileId::from_raw(
        u32::try_from(active_source).expect("source-set test source index fits u32"),
    );
    let ordinary = streamed.ordinary_body_work(&analysis.files[active_source], source_id);
    let (mut index, inline_bodies, mut default_arguments, mut source_map) =
        streamed.module.into_parts();
    for (source_index, (file, info)) in analysis.files.iter().zip(analysis.types.iter()).enumerate()
    {
        let source = crate::fir::SourceFileId::from_raw(
            u32::try_from(source_index).expect("source-set test source index fits u32"),
        );
        crate::resolve::publish_checked_local_signatures(
            file,
            source,
            &analysis.symbols,
            info.as_ref().expect("checked type info"),
            &mut index,
        )
        .expect("checked local signatures");
    }
    let mut ir = IrFile::default();
    let mut sink = super::super::CommonIrBodySink::new(&index, source_id, &mut ir).unwrap();
    sink.accept_inline_bodies(&index, &inline_bodies).unwrap();
    sink.accept_default_arguments(&index, &inline_bodies, &mut default_arguments)
        .unwrap();
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
    sink.accept_inline_bodies(&index, &inline_bodies).unwrap();
    sink.accept_default_arguments(&index, &inline_bodies, &mut default_arguments)
        .unwrap();
    sink.finish(&index).unwrap();
    ir
}

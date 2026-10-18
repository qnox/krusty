use super::*;
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

#[test]
fn completed_local_owner_stays_complete_when_a_generated_child_is_added() {
    let source = "fun build() {\n    class Local { fun value(): Int = 1 }\n}\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("MonotonicLocalOwner")],
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 module");
    let (mut index, _, _, _) = streamed.module.into_parts();
    let source = crate::fir::SourceFileId::from_raw(0);
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        source,
        &analysis.symbols,
        analysis.types[0].as_ref().expect("checked source"),
        &mut index,
    )
    .expect("Pass 2 local signatures");
    let value = index
        .declarations_named("value")
        .first()
        .copied()
        .expect("stable local method");
    let local = index
        .classifier_and_outer_class_declarations(value)
        .first()
        .copied()
        .expect("stable local classifier");
    assert!(matches!(
        semantic_owner(&index, local),
        SemanticOwner::Complete(Some(_))
    ));

    let generated = index.intern_checked_local_declaration(
        crate::fir::DeclarationAnchor {
            source,
            range: index
                .declaration_range(local)
                .expect("local classifier source range"),
            owner: Some(local),
            kind: crate::fir::DeclarationKind::Function,
            sibling: u32::MAX,
        },
        crate::fir::ResolvedDeclarationHeader {
            kind: crate::fir::DeclarationKind::Function,
            owner: Some(local),
            name: None,
            visibility: crate::types::Visibility::Private,
            flags: crate::fir::DeclarationFlags::default()
                .with(crate::fir::DeclarationFlags::LOCAL_CLASS, true)
                .with(crate::fir::DeclarationFlags::COMPILER_GENERATED, true),
            initialization_order: None,
        },
        "generated",
    );
    assert!(!index.declaration_semantic_header_complete(generated));
    assert!(index.body_local_owner_complete(local));
    assert!(matches!(
        semantic_owner(&index, generated),
        SemanticOwner::Complete(Some(owner)) if owner.declaration == local
    ));
}

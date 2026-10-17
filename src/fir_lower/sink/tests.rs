use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::ir::IrFile;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;
use crate::types::Ty;

#[test]
fn body_local_members_wait_for_their_semantic_classifier_owner() {
    let source = r#"
class Outer {
    fun build() {
        class Local {
            val mark: Int = 1
            fun read(): Int = 1
        }
    }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DeferredLocalMemberOwner")];
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 module");
    assert!(!streamed.diagnostic_recovery);
    let (mut index, _, _, _) = streamed.module.into_parts();
    let source_id = crate::fir::SourceFileId::from_raw(0);
    let read = index
        .declarations_named("read")
        .first()
        .copied()
        .expect("stable read declaration");
    let mark = index
        .declarations_named("mark")
        .first()
        .copied()
        .expect("stable mark declaration");
    let local = index
        .classifier_and_outer_class_declarations(read)
        .first()
        .copied()
        .expect("stable local classifier");
    assert!(index.signature(read).is_some());
    assert!(index.declaration_header(mark).is_some());
    assert!(index.signature(mark).is_none());
    assert!(index.enclosing_classifier(read).is_some());
    assert!(index.enclosing_classifier(mark).is_some());
    assert_eq!(
        index
            .classifier_and_outer_class_declarations(read)
            .first()
            .copied(),
        Some(local)
    );
    assert!(index.classifier_header(local).is_none());
    assert!(matches!(
        super::semantic_owner(&index, read),
        super::SemanticOwner::Deferred
    ));
    assert!(matches!(
        super::semantic_owner(&index, mark),
        super::SemanticOwner::Deferred
    ));

    let mut ir = IrFile::default();
    let mut sink = super::CommonIrBodySink::new(&index, source_id, &mut ir)
        .expect("deferred owners do not block the initial sink");
    assert!(sink
        .ir_mut()
        .functions
        .iter()
        .all(|function| function.name != "read"));
    assert!(sink
        .ir_mut()
        .checked_properties
        .values()
        .all(|property| property.name != "mark"));

    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        source_id,
        &analysis.symbols,
        analysis.types[0]
            .as_ref()
            .expect("Pass 2 checked local declarations"),
        &mut index,
    )
    .expect("checked local declarations publish their stable owners");
    sink.refresh_body_local_declarations(&index)
        .expect("refresh realizes the newly complete members");

    let ir = sink.ir_mut();
    let (read_id, read) = ir
        .functions
        .iter()
        .enumerate()
        .find(|(_, function)| function.name == "read")
        .expect("refreshed read realization");
    let owner = read
        .dispatch_receiver
        .expect("a local member has a dispatch receiver");
    assert_eq!(
        owner,
        index
            .classifier_header(local)
            .expect("Pass 2 local classifier header")
            .classifier
    );
    assert!(!read.is_static);
    let owner_class = ir
        .classes
        .iter()
        .position(|class| class.fq_name == owner)
        .expect("local classifier IR skeleton");
    assert!(ir.classes[owner_class].methods.contains(&(read_id as u32)));
    assert!(ir
        .checked_properties
        .values()
        .any(|property| { property.name == "mark" && property.class == Some(owner_class as u32) }));
}

#[test]
fn finalized_body_local_property_waits_for_its_exact_classifier_owner() {
    let source = r#"
class Outer {
    fun build() {
        class Local {
            val mark: Int = 1
        }
    }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DeferredLocalPropertyOwner")];
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 module");
    assert!(!streamed.diagnostic_recovery);
    let (mut index, _, _, _) = streamed.module.into_parts();
    let mark = index
        .declarations_named("mark")
        .first()
        .copied()
        .expect("stable mark declaration");
    let local = index
        .classifier_and_outer_class_declarations(mark)
        .first()
        .copied()
        .expect("stable local classifier");
    assert!(index.classifier_header(local).is_none());
    assert!(index.signature(mark).is_none());

    index
        .publish_signature(mark, [], Ty::Int)
        .expect("synthetic finalized property signature");
    let property = crate::fir::PropertyId::from_raw(mark.raw());
    index.publish_property_shape(property, mark, 0, 0, None, false);
    index.publish_property_parameter_identities(
        property,
        std::iter::empty::<(&str, crate::types::ContextParameterKind)>(),
        None,
    );

    let mut ir = IrFile::default();
    let mut sink =
        super::CommonIrBodySink::new(&index, crate::fir::SourceFileId::from_raw(0), &mut ir)
            .expect("an incomplete local owner defers its otherwise finalized property");
    assert!(sink
        .ir_mut()
        .checked_properties
        .values()
        .all(|checked| checked.name != "mark"));
}

#[test]
fn body_local_callable_without_classifier_ancestry_is_static() {
    let local = crate::fir::DeclarationId::from_raw(0);
    let mut index = crate::fir::ResolvedModuleIndex::default();
    index.publish_declaration_header(
        local,
        crate::fir::ResolvedDeclarationHeader {
            kind: crate::fir::DeclarationKind::Function,
            owner: None,
            name: None,
            visibility: crate::types::Visibility::Private,
            flags: crate::fir::DeclarationFlags::default()
                .with(crate::fir::DeclarationFlags::LOCAL_CLASS, true),
            initialization_order: None,
        },
        Some("local"),
    );
    assert!(index.is_body_local_declaration(local));
    assert!(index
        .classifier_and_outer_class_declarations(local)
        .is_empty());
    assert!(matches!(
        super::semantic_owner(&index, local),
        super::SemanticOwner::Complete(None)
    ));
}

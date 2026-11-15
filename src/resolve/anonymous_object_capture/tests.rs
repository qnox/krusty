use crate::diag::DiagSink;
use crate::fir::{FirBody, FirExprId, FirExprKind, FirLocalClassCaptureSource, SourceFileId};
use crate::types::Ty;

fn retained_inline_body(source: &str) -> FirBody {
    let inputs = [crate::source::SourceInput::kotlin(source).with_file_stem("PublishedCapture")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
        &crate::features::LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());
    let streamed = analysis
        .streamed
        .expect("Pass 1 retains the inline declaration");
    let bodies = streamed
        .module
        .inline_bodies()
        .bodies_for_source(streamed.module.index(), SourceFileId::from_raw(0));
    let [(_, body)] = bodies.as_slice() else {
        panic!("one retained inline body expected: {bodies:?}")
    };
    body.clone()
}

fn anonymous_object(body: &FirBody) -> &crate::fir::FirAnonymousObject {
    let objects = (0..body.expression_count())
        .filter_map(
            |raw| match &body.expr(FirExprId::from_raw(raw as u32))?.kind {
                FirExprKind::AnonymousObject(object) => Some(object),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    let [object] = objects.as_slice() else {
        panic!("one anonymous construction expected: {objects:?}")
    };
    object
}

#[test]
fn published_inline_capture_keeps_its_selected_value_and_member_storage() {
    let body = retained_inline_body(
        "interface Value { fun read(): Int }\n\
         inline fun make(crossinline value: () -> Int): Value = object : Value {\n\
             override fun read(): Int = value()\n\
         }\n",
    );
    let object = anonymous_object(&body);
    let [capture] = object.captures.as_ref() else {
        panic!("one selected capture expected: {:?}", object.captures)
    };
    assert_eq!(capture.ty.get(), Ty::fun(vec![], Ty::Int));
    assert_eq!(
        capture.source,
        FirLocalClassCaptureSource::Value(body.parameters()[0].value)
    );
    assert!(capture.stored);
    assert!(!capture.shared_cell);
    let reads = body
        .inline_nested_declaration_bodies()
        .iter()
        .flat_map(|member| {
            (0..member.expression_count()).filter_map(move |raw| {
                match &member.expr(FirExprId::from_raw(raw as u32))?.kind {
                    FirExprKind::ClassStorageRead { owner, field } => Some((*owner, *field)),
                    _ => None,
                }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(reads, [(object.declaration, 0)]);
}

#[test]
fn published_inline_empty_selection_does_not_capture_an_unused_enclosing_value() {
    let body = retained_inline_body(
        "interface Value { fun read(): Int }\n\
         inline fun make(value: Int): Value = object : Value {\n\
             override fun read(): Int = 1\n\
         }\n",
    );
    assert_eq!(body.parameters().len(), 1);
    let object = anonymous_object(&body);
    assert!(object.captures.is_empty(), "{object:?}");
    let reads = body
        .inline_nested_declaration_bodies()
        .iter()
        .flat_map(|member| {
            (0..member.expression_count()).filter_map(move |raw| {
                match &member.expr(FirExprId::from_raw(raw as u32))?.kind {
                    FirExprKind::ClassStorageRead { owner, field } => Some((*owner, *field)),
                    _ => None,
                }
            })
        })
        .collect::<Vec<_>>();
    assert!(reads.is_empty(), "{reads:?}");
}

#[test]
fn published_replay_refreshes_parameter_and_local_bindings_after_unrelated_declarations() {
    let source = include_str!("../../../tests/box_regressions/PublishedInlineCaptureBindings.kt");
    let body = retained_inline_body(source);
    let object = anonymous_object(&body);
    assert_eq!(object.captures.len(), 2);
    assert_eq!(
        object
            .captures
            .iter()
            .filter(|capture| capture.source
                == FirLocalClassCaptureSource::Value(body.parameters()[0].value))
            .count(),
        1
    );
    assert!(object
        .captures
        .iter()
        .all(|capture| capture.stored && !capture.shared_cell));
    let fields = body
        .inline_nested_declaration_bodies()
        .iter()
        .flat_map(|member| {
            (0..member.expression_count()).filter_map(move |raw| {
                match &member.expr(FirExprId::from_raw(raw as u32))?.kind {
                    FirExprKind::ClassStorageRead { owner, field } => Some((*owner, *field)),
                    _ => None,
                }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(fields, [(object.declaration, 0), (object.declaration, 1)]);
}

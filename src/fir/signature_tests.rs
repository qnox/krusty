use super::*;

use crate::diag::DiagSink;
use crate::source::SourceInput;

#[test]
fn compact_calls_preserve_mapping_spread_trailing_lambda_and_type_arguments() {
    let text = r#"
fun inferred() = transform<String>(
    value = "first",
    values = *arrayOf("second"),
) { input: String -> input }
"#;
    let sources = [SourceInput::kotlin(text).with_file_stem("CallShape")];
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
    let headers = inventory_parsed_source_headers(&sources, std::slice::from_ref(&file));
    let mut origins = OriginStore::default();
    let mut extractor = SignatureConstraintExtractor::default();
    extractor.extract_file(&file, SourceFileId::from_raw(0), &headers.stubs, |span| {
        origins.source(SourceFileId::from_raw(0), span)
    });

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(extractor.failures(), []);
    let graph = extractor.finish().unwrap();
    let constraint = graph.constraints()[0];
    let SigExpr::Call { target, arguments } = graph.expr(constraint.result).unwrap() else {
        panic!("inferred signature root must remain a deferred call")
    };
    let selection = graph.callable_selection(target).unwrap();
    assert!(selection.trailing_lambda);
    assert_eq!(graph.operands(selection.type_arguments).len(), 1);
    let arguments = graph.call_arguments(arguments);
    assert_eq!(arguments.len(), 3);
    assert_eq!(
        arguments[0].name.and_then(|name| graph.name(name)),
        Some("value")
    );
    assert_eq!(
        arguments[1].name.and_then(|name| graph.name(name)),
        Some("values")
    );
    assert!(arguments[1].spread);
    assert_eq!(arguments[2].name, None);
    assert!(!arguments[2].spread);
    drop(file);
    assert_eq!(graph.constraints()[0], constraint);
}

#[test]
fn compact_call_chain_keeps_classifier_qualified_base_call() {
    let text = r#"
fun cmp(d: JDerived) =
    Comparator.comparing(d::is1).thenComparing(d::is2)
"#;
    let sources = [SourceInput::kotlin(text).with_file_stem("QualifiedCallChain")];
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
    let headers = inventory_parsed_source_headers(&sources, std::slice::from_ref(&file));
    let mut origins = OriginStore::default();
    let mut extractor = SignatureConstraintExtractor::default();
    extractor.extract_file(&file, SourceFileId::from_raw(0), &headers.stubs, |span| {
        origins.source(SourceFileId::from_raw(0), span)
    });

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(extractor.failures(), []);
    let graph = extractor.finish().unwrap();
    let constraint = graph.constraints()[0];
    let SigExpr::MemberCall { receiver, .. } = graph.expr(constraint.result).unwrap() else {
        panic!("outer call must remain a member call")
    };
    let SigExpr::Call { target, .. } = graph.expr(receiver).unwrap() else {
        panic!("classifier-qualified base must remain a deferred call")
    };
    let selection = graph.callable_selection(target).unwrap();
    assert_eq!(graph.name(selection.spelling), Some("Comparator.comparing"));
}

#[test]
fn compact_applied_fun_interface_member_calls_remain_constraints() {
    let text = r#"
fun interface IFoo<T> { fun foo(x: T): T }
fun interface IBar<T : Any> { fun bar(x: T): T }
fun foo1(foo: IFoo<Int>) = foo.foo(1)
fun bar1(bar: IBar<Int>) = bar.bar(1)
"#;
    let sources = [SourceInput::kotlin(text).with_file_stem("GenericFunInterfaces")];
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
    let headers = inventory_parsed_source_headers(&sources, std::slice::from_ref(&file));
    let mut origins = OriginStore::default();
    let mut extractor = SignatureConstraintExtractor::default();
    extractor.extract_file(&file, SourceFileId::from_raw(0), &headers.stubs, |span| {
        origins.source(SourceFileId::from_raw(0), span)
    });

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(extractor.failures(), []);
    let graph = extractor.finish().unwrap();
    assert_eq!(graph.constraints().len(), 2);
    assert!(graph.constraints().iter().all(|constraint| matches!(
        graph.expr(constraint.result),
        Some(SigExpr::MemberCall { .. })
    )));
}

#[test]
fn compact_class_literals_are_not_callable_reference_lookups() {
    let text = r#"
fun unbound() = String::class
fun bound(value: String) = value::class
"#;
    let sources = [SourceInput::kotlin(text).with_file_stem("ClassLiteralSignatures")];
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
    let headers = inventory_parsed_source_headers(&sources, std::slice::from_ref(&file));
    let mut origins = OriginStore::default();
    let mut extractor = SignatureConstraintExtractor::default();
    extractor.extract_file(&file, SourceFileId::from_raw(0), &headers.stubs, |span| {
        origins.source(SourceFileId::from_raw(0), span)
    });

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(extractor.failures(), []);
    let graph = extractor.finish().unwrap();
    let constraints = graph.constraints();
    assert_eq!(constraints.len(), 2);
    assert!(matches!(
        graph.expr(constraints[0].result),
        Some(SigExpr::ClassLiteral {
            classifier: Some(_),
            ..
        })
    ));
    assert!(matches!(
        graph.expr(constraints[1].result),
        Some(SigExpr::ClassLiteral {
            classifier: None,
            ..
        })
    ));
}

#[test]
fn compact_smartcast_of_this_scopes_the_refined_implicit_receiver() {
    let text = r#"
fun Any.lengthOrMinusOne() = if (this is Array<*>) size else -1
"#;
    let sources = [SourceInput::kotlin(text).with_file_stem("ThisSmartcastSignature")];
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
    let headers = inventory_parsed_source_headers(&sources, std::slice::from_ref(&file));
    let mut origins = OriginStore::default();
    let mut extractor = SignatureConstraintExtractor::default();
    extractor.extract_file(&file, SourceFileId::from_raw(0), &headers.stubs, |span| {
        origins.source(SourceFileId::from_raw(0), span)
    });

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(extractor.failures(), []);
    let graph = extractor.finish().unwrap();
    let constraint = graph.constraints()[0];
    let SigExpr::Join { operands, .. } = graph.expr(constraint.result).unwrap() else {
        panic!("if-expression signature must retain its branch join")
    };
    let [then_branch, _] = graph.operands(operands) else {
        panic!("if-expression signature must retain both branches")
    };
    assert!(matches!(
        graph.expr(*then_branch),
        Some(SigExpr::ScopedReceiver { .. })
    ));
}

#[test]
fn compact_smartcast_retains_expected_type_classifier_selection() {
    let text = r#"
// LANGUAGE: +ContextSensitiveResolutionUsingExpectedType
sealed interface Either<out E, out A> {
    data class Left<out E>(val error: E) : Either<E, Nothing>
    data class Right<out A>(val value: A) : Either<Nothing, A>
}
fun <E, A> Either<E, A>.getOrElse(default: A) = when (this) {
    is Left -> default
    is Right -> value
}
"#;
    let sources = [SourceInput::kotlin(text).with_file_stem("ContextualTypeSignature")];
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
    let headers = inventory_parsed_source_headers(&sources, std::slice::from_ref(&file));
    let mut origins = OriginStore::default();
    let mut extractor = SignatureConstraintExtractor::default();
    extractor.extract_file(&file, SourceFileId::from_raw(0), &headers.stubs, |span| {
        origins.source(SourceFileId::from_raw(0), span)
    });

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(extractor.failures(), []);
    let graph = extractor.finish().unwrap();
    let constraint = graph.constraints()[0];
    let SigExpr::Join { operands, .. } = graph.expr(constraint.result).unwrap() else {
        panic!("when-expression signature must retain its branch join")
    };
    assert!(graph.operands(operands).iter().any(|branch| {
        let Some(SigExpr::ScopedReceiver { receiver, .. }) = graph.expr(*branch) else {
            return false;
        };
        matches!(graph.expr(receiver), Some(SigExpr::ContextualType { .. }))
    }));
}

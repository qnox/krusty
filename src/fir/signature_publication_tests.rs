use super::tests::{inferred_stub, CallPublicationInput, TestSignatureSemantics};
use super::*;
use crate::types::Ty;

fn published_signature(
    declaration: DeclarationId,
    origin: OriginId,
    result: SigExprId,
    mut graph: SignatureGraph,
    semantics: &TestSignatureSemantics,
) -> Ty {
    graph.add_inferred_constraint(
        &inferred_stub(declaration, InferredSignatureKind::ExpressionFunction),
        result,
        origin,
    );
    SignatureSolver::new(graph, [declaration])
        .finalize(&ResolverBackedSignatureEvaluator::new(semantics))
        .unwrap()
        .signature(declaration)
        .unwrap()
        .result
        .get()
}

#[test]
fn compact_call_publication_replaces_the_rigid_evaluated_result() {
    let declaration = DeclarationId::from_raw(27);
    let origin = OriginId::from_raw(9);
    let function = Ty::fun(vec![Ty::String], Ty::String);
    let mut graph = SignatureGraph::default();
    let scope = graph.add_scope(SignatureScope {
        owner: declaration,
        source: SourceFileId::from_raw(0),
    });
    let argument = graph.add_expr(SigExpr::Known(ResolvedTy::new(function).unwrap()));
    let arguments = graph.add_call_arguments([SigCallArgument {
        value: argument,
        origin,
        name: None,
        spread: false,
        lambda: false,
    }]);
    let spelling = graph.intern_name("selectedJavaResult");
    let target = graph.add_callable_selection(DeferredCallableSelection {
        scope,
        spelling,
        lexical_classifier: None,
        origin,
        expected: None,
        type_arguments: OperandRange::default(),
        trailing_lambda: false,
    });
    let result = graph.add_expr(SigExpr::Call {
        target,
        arguments,
        qualified: None,
    });
    let semantics =
        TestSignatureSemantics::with_call_publication(origin, Ty::platform_nullable(function));

    assert_eq!(
        published_signature(declaration, origin, result, graph, &semantics),
        Ty::platform_nullable(function),
    );
    let function = ResolvedTy::new(function).unwrap();
    assert_eq!(
        semantics.call_publication_inputs(),
        vec![CallPublicationInput {
            receiver: None,
            arguments: vec![(function, function)],
        }],
    );
}

#[test]
fn compact_member_call_publication_replaces_the_rigid_evaluated_result() {
    let declaration = DeclarationId::from_raw(28);
    let origin = OriginId::from_raw(10);
    let function = Ty::fun(vec![Ty::String], Ty::String);
    let mut graph = SignatureGraph::default();
    let scope = graph.add_scope(SignatureScope {
        owner: declaration,
        source: SourceFileId::from_raw(0),
    });
    let receiver = graph.add_expr(SigExpr::Known(ResolvedTy::new(function).unwrap()));
    let spelling = graph.intern_name("selectedJavaMember");
    let target = graph.add_member_selection(DeferredMemberSelection {
        scope,
        spelling,
        origin,
        expected: None,
        type_arguments: OperandRange::default(),
        trailing_lambda: false,
    });
    let arguments = graph.add_call_arguments([]);
    let result = graph.add_expr(SigExpr::MemberCall {
        receiver,
        target,
        arguments,
        origin,
    });
    let semantics =
        TestSignatureSemantics::with_call_publication(origin, Ty::platform_nullable(function));

    assert_eq!(
        published_signature(declaration, origin, result, graph, &semantics),
        Ty::platform_nullable(function),
    );
    let function = ResolvedTy::new(function).unwrap();
    assert_eq!(
        semantics.call_publication_inputs(),
        vec![CallPublicationInput {
            receiver: Some((function, function)),
            arguments: Vec::new(),
        }],
    );
}

#[test]
fn compact_member_property_publication_replaces_the_rigid_evaluated_result() {
    let declaration = DeclarationId::from_raw(29);
    let origin = OriginId::from_raw(11);
    let function = Ty::fun(vec![Ty::String], Ty::String);
    let mut graph = SignatureGraph::default();
    let scope = graph.add_scope(SignatureScope {
        owner: declaration,
        source: SourceFileId::from_raw(0),
    });
    let receiver = graph.add_expr(SigExpr::Known(ResolvedTy::new(function).unwrap()));
    let spelling = graph.intern_name("selectedJavaProperty");
    let lookup = graph.add_member_selection(DeferredMemberSelection {
        scope,
        spelling,
        origin,
        expected: None,
        type_arguments: OperandRange::default(),
        trailing_lambda: false,
    });
    let result = graph.add_expr(SigExpr::Member {
        receiver,
        lookup,
        origin,
    });
    let semantics =
        TestSignatureSemantics::with_call_publication(origin, Ty::platform_nullable(function));

    assert_eq!(
        published_signature(declaration, origin, result, graph, &semantics),
        Ty::platform_nullable(function),
    );
    let function = ResolvedTy::new(function).unwrap();
    assert_eq!(
        semantics.call_publication_inputs(),
        vec![CallPublicationInput {
            receiver: Some((function, function)),
            arguments: Vec::new(),
        }],
    );
}

#[test]
fn compact_qualified_value_call_publication_receives_the_bound_receiver() {
    let declaration = DeclarationId::from_raw(30);
    let origin = OriginId::from_raw(12);
    let function = Ty::fun(vec![Ty::String], Ty::String);
    let mut graph = SignatureGraph::default();
    let scope = graph.add_scope(SignatureScope {
        owner: declaration,
        source: SourceFileId::from_raw(0),
    });
    let receiver = graph.add_expr(SigExpr::Known(ResolvedTy::new(function).unwrap()));
    let member_spelling = graph.intern_name("selectedJavaMember");
    let member = graph.add_member_selection(DeferredMemberSelection {
        scope,
        spelling: member_spelling,
        origin,
        expected: None,
        type_arguments: OperandRange::default(),
        trailing_lambda: false,
    });
    let root = graph.intern_name("receiver");
    let first_selector = graph.intern_name("selectedJavaMember");
    let qualified = graph.add_qualified_call_coordinate(QualifiedCallCoordinate {
        receiver,
        root,
        first_selector,
        member,
        origin,
    });
    let qualified_spelling = graph.intern_name("receiver.selectedJavaMember");
    let target = graph.add_callable_selection(DeferredCallableSelection {
        scope,
        spelling: qualified_spelling,
        lexical_classifier: None,
        origin,
        expected: None,
        type_arguments: OperandRange::default(),
        trailing_lambda: false,
    });
    let arguments = graph.add_call_arguments([]);
    let result = graph.add_expr(SigExpr::Call {
        target,
        arguments,
        qualified: Some(qualified),
    });
    let semantics = TestSignatureSemantics::with_qualified_call_publication(
        origin,
        Ty::platform_nullable(function),
    );

    assert_eq!(
        published_signature(declaration, origin, result, graph, &semantics),
        Ty::platform_nullable(function),
    );
    let function = ResolvedTy::new(function).unwrap();
    assert_eq!(
        semantics.call_publication_inputs(),
        vec![CallPublicationInput {
            receiver: Some((function, function)),
            arguments: Vec::new(),
        }],
    );
}

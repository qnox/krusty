use super::*;
use crate::fir::ClassCaptureIdentity;
use crate::resolve::scope::ScopeKind;
use crate::resolve::{
    make_checker, AnonymousObjectCapture, AnonymousObjectCaptureSource, ClassStorageSource,
    SymbolTable,
};
use crate::types::type_name;

fn selected_capture(declaration: LexicalValueDeclaration, identity: u32) -> AnonymousObjectCapture {
    AnonymousObjectCapture {
        enum_entry_storage: None,
        name: "value".into(),
        ty: Ty::String,
        shared_cell: false,
        storage_ty: None,
        source: AnonymousObjectCaptureSource::LexicalValue,
        receiver_label: None,
        receiver: None,
        semantic_receiver: None,
        lexical_shadow_depth: 0,
        lexical_binding: Some(identity),
        lexical_declaration: Some(declaration),
        capture_dependency: Some(ClassCaptureIdentity::Value {
            owner: type_name("CaptureOwner"),
            binding: identity,
        }),
        constructor_projections: vec![],
        receiver_capture: None,
        receiver_role: None,
    }
}

#[test]
fn published_operand_refresh_uses_declaration_and_keeps_original_transport_family() {
    let mut diagnostics = crate::diag::DiagSink::new();
    let tokens = crate::lexer::lex("", &mut diagnostics);
    let file = crate::parser::parse("", &tokens, &mut diagnostics);
    let symbols = SymbolTable::default();
    let mut checker = make_checker(&file, 0, None, &symbols, &mut diagnostics);
    let roles = [
        LexicalValueRole::Parameter(Span::new(1, 3)),
        LexicalValueRole::Statement {
            declaration: Span::new(4, 8),
            slot: 0,
        },
        LexicalValueRole::Statement {
            declaration: Span::new(4, 8),
            slot: 1,
        },
        LexicalValueRole::LambdaParameter {
            expression: Span::new(9, 20),
            slot: 0,
        },
        LexicalValueRole::LambdaParameter {
            expression: Span::new(9, 20),
            slot: 1,
        },
        LexicalValueRole::Catch(Span::new(21, 25)),
        LexicalValueRole::Setter(Span::new(26, 30)),
    ];
    for role in roles {
        let root = CheckerScope::root();
        let outer = root.child(ScopeKind::Block);
        // This earlier value owns the old checker's numeric id but not its declaration role.
        checker.declare(&outer, "value", Ty::Int, false);
        let selected = outer.child(ScopeKind::Block);
        checker.declare(&selected, "value", Ty::String, false);
        checker.record_lexical_declaration(&selected, "value", role);
        let actual = checker.lookup(&selected, "value").unwrap();
        let declaration = actual.lexical_declaration.unwrap();
        let inner = selected.child(ScopeKind::Block);
        checker.declare(&inner, "value", Ty::Int, false);
        checker.record_lexical_declaration(
            &inner,
            "value",
            LexicalValueRole::Parameter(Span::new(31, 33)),
        );
        let mut captures = [selected_capture(declaration, 0)];
        let original = captures[0].capture_dependency;
        assert!(checker.refresh_published_lexical_captures(
            &inner,
            &mut captures,
            Span::new(40, 42)
        ));
        assert_eq!(captures[0].lexical_binding, actual.lexical_capture_identity);
        assert_ne!(captures[0].lexical_binding, Some(0));
        assert_eq!(captures[0].lexical_shadow_depth, 1);
        assert_eq!(captures[0].ty, Ty::String);
        assert_eq!(captures[0].capture_dependency, original);
        assert!(checker.check_anonymous_capture_arguments(&inner, &captures, Span::new(40, 42)));
        assert_eq!(
            checker
                .lexical_binding_by_identity(
                    &inner,
                    &captures[0].name,
                    captures[0].lexical_binding.unwrap()
                )
                .map(|(local, _)| local.ty),
            Some(Ty::String)
        );
        assert_eq!(
            checker
                .lexical_binding_by_declaration(&inner, LexicalValueDeclaration { file: 1, role })
                .map(|(_, local, _)| local.ty),
            None
        );
    }
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn published_class_storage_refresh_retargets_the_stale_value_binding() {
    let mut diagnostics = crate::diag::DiagSink::new();
    let tokens = crate::lexer::lex("", &mut diagnostics);
    let file = crate::parser::parse("", &tokens, &mut diagnostics);
    let symbols = SymbolTable::default();
    let mut checker = make_checker(&file, 0, None, &symbols, &mut diagnostics);
    let root = CheckerScope::root();
    let outer = root.child(ScopeKind::Block);
    // The other checker's value id is live here on a different declaration.
    checker.declare(&outer, "text", Ty::Int, false);
    checker.record_lexical_declaration(
        &outer,
        "text",
        LexicalValueRole::Parameter(Span::new(1, 2)),
    );
    let owner = type_name("__krusty_local_3");
    let current = ClassCaptureIdentity::Value { owner, binding: 4 };
    let storage_scope = outer.child(ScopeKind::Block);
    checker.declare_class_storage(
        &storage_scope,
        "text",
        Ty::String,
        false,
        ClassStorageSource {
            owner,
            identity: current,
        },
        false,
    );
    let declaration = LexicalValueDeclaration {
        file: 0,
        role: LexicalValueRole::Statement {
            declaration: Span::new(10, 14),
            slot: 0,
        },
    };
    let mut carrier = selected_capture(declaration, 4);
    carrier.name = "text".into();
    checker.retain_capture_declaration(&storage_scope, &carrier);
    let stale = ClassCaptureIdentity::Value { owner, binding: 0 };
    let mut capture = selected_capture(declaration, 0);
    capture.name = "text".into();
    capture.source = AnonymousObjectCaptureSource::ClassStorage(ClassStorageSource {
        owner,
        identity: stale,
    });
    capture.capture_dependency = Some(stale);
    let mut captures = [capture];
    assert!(checker.refresh_published_lexical_captures(
        &storage_scope,
        &mut captures,
        Span::new(20, 22)
    ));
    assert_eq!(
        captures[0].source,
        AnonymousObjectCaptureSource::ClassStorage(ClassStorageSource {
            owner,
            identity: current,
        })
    );
    assert_eq!(captures[0].lexical_binding, Some(4));
    assert_eq!(captures[0].capture_dependency, Some(stale));
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn strict_current_identity_query_does_not_retry_the_valid_declaration_by_name_or_role() {
    let mut diagnostics = crate::diag::DiagSink::new();
    let tokens = crate::lexer::lex("", &mut diagnostics);
    let file = crate::parser::parse("", &tokens, &mut diagnostics);
    let symbols = SymbolTable::default();
    let mut checker = make_checker(&file, 0, None, &symbols, &mut diagnostics);
    let root = CheckerScope::root();
    let scope = root.child(ScopeKind::Block);
    let role = LexicalValueRole::Parameter(Span::new(1, 3));
    checker.declare(&scope, "value", Ty::String, false);
    checker.record_lexical_declaration(&scope, "value", role);
    let capture = selected_capture(LexicalValueDeclaration { file: 0, role }, u32::MAX);
    assert!(checker
        .lexical_binding_by_identity(&scope, &capture.name, capture.lexical_binding.unwrap())
        .is_none());
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn strict_current_binding_does_not_retry_the_valid_declaration_by_name_or_role() {
    let mut diagnostics = crate::diag::DiagSink::new();
    let tokens = crate::lexer::lex("", &mut diagnostics);
    let file = crate::parser::parse("", &tokens, &mut diagnostics);
    let symbols = SymbolTable::default();
    let mut checker = make_checker(&file, 0, None, &symbols, &mut diagnostics);
    let root = CheckerScope::root();
    let scope = root.child(ScopeKind::Block);
    let role = LexicalValueRole::Parameter(Span::new(1, 3));
    checker.declare(&scope, "value", Ty::String, false);
    checker.record_lexical_declaration(&scope, "value", role);
    let capture = selected_capture(LexicalValueDeclaration { file: 0, role }, u32::MAX);
    assert!(!checker.check_anonymous_capture_arguments(&scope, &[capture], Span::new(4, 8)));
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| (diagnostic.span, diagnostic.msg.as_str()))
            .collect::<Vec<_>>(),
        [(
            Span::new(4, 8),
            "anonymous object capture has no selected value binding"
        )]
    );
}

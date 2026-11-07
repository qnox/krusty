use super::*;
use crate::lexer::lex;
use crate::parser::parse;

fn with_checker(test: impl FnOnce(&mut Checker<'_>)) {
    let mut diagnostics = DiagSink::new();
    let source = "class Storage
fun value() = 1";
    let tokens = lex(source, &mut diagnostics);
    let file = parse(source, &tokens, &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let symbols = SymbolTable::default();
    let mut checker = make_checker(&file, 0, None, &symbols, &mut diagnostics);
    test(&mut checker);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

fn stale_storage() -> super::super::ClassStorageSource {
    let owner = type_name("Storage");
    super::super::ClassStorageSource {
        owner,
        identity: crate::fir::ClassCaptureIdentity::Value { owner, binding: 0 },
    }
}

fn stale_capture_read(checker: &mut Checker<'_>, expression: ExprId, local: Local) {
    checker
        .expr_lowers
        .insert(expression, ExprLowering::ClassStorageRead(stale_storage()));
    checker
        .read_flow_roots
        .insert(expression, scope::PathRoot::Value(local.flow_identity));
}

fn property() -> EnumEntryProperty {
    EnumEntryProperty {
        owner: None,
        sibling: 0,
    }
}

fn entry() -> EnumEntryStorage {
    EnumEntryStorage {
        owner: type_name("Choice$First"),
        property: property(),
    }
}

#[test]
fn selected_member_read_replaces_value_storage_and_records_the_exact_receiver() {
    with_checker(|checker| {
        let root = CheckerScope::root();
        let owner = root.child(ScopeKind::Class {
            ty: Ty::obj("Choice"),
            carries_outer: false,
        });
        checker.declare_enum_entry_property_storage(&owner, "member", Ty::String, false, entry());
        let receiver = owner.innermost_class_receiver_identity().unwrap();
        let nested = owner.child(ScopeKind::Class {
            ty: Ty::obj("Anonymous"),
            carries_outer: true,
        });
        assert_eq!(
            checker.declare_enum_entry_dispatch_property(
                &nested,
                "member",
                Ty::String,
                false,
                EnumEntryPropertyBinding {
                    owner: entry().owner,
                    property: property(),
                    receiver: Some(receiver),
                },
            ),
            Ok(())
        );
        let expression = ExprId(0);
        let local = checker.lookup(&owner, "member").unwrap();
        stale_capture_read(checker, expression, local);
        assert!(checker.read_flow_roots.contains_key(&expression));
        assert_eq!(checker.implicit_receiver_identity_use_count(receiver), 0);
        assert!(checker.publish_enum_entry_property_read(
            &nested,
            expression,
            entry().owner,
            property(),
            receiver
        ));
        assert!(matches!(checker.expr_lowers.get(&expression),
            Some(ExprLowering::EnumEntryPropertyRead { property: selected }) if *selected == property()));
        assert!(!checker.read_flow_roots.contains_key(&expression));
        assert_eq!(
            checker.implicit_receiver_identities.get(&expression),
            Some(&receiver)
        );
        assert_eq!(checker.implicit_receiver_identity_use_count(receiver), 1);
    });
}

#[test]
fn wrong_owner_sibling_or_receiver_does_not_publish_or_erase_prior_facts() {
    with_checker(|checker| {
        let root = CheckerScope::root();
        let owner = root.child(ScopeKind::Class {
            ty: Ty::obj("Choice"),
            carries_outer: false,
        });
        checker.declare_enum_entry_property_storage(&owner, "member", Ty::String, false, entry());
        let receiver = owner.innermost_class_receiver_identity().unwrap();
        let nearer = owner.child(ScopeKind::Class {
            ty: Ty::obj("Choice"),
            carries_outer: true,
        });
        let other = nearer.innermost_class_receiver_identity().unwrap();
        let expression = ExprId(0);
        let local = checker.lookup(&owner, "member").unwrap();
        stale_capture_read(checker, expression, local);
        let flow = checker.read_flow_roots[&expression].clone();
        assert_eq!(
            checker.declare_enum_entry_dispatch_property(
                &nearer,
                "unbound",
                Ty::String,
                false,
                EnumEntryPropertyBinding {
                    owner: entry().owner,
                    property: property(),
                    receiver: None,
                },
            ),
            Err(MissingEnumEntryReceiver),
        );
        assert!(checker.lookup(&nearer, "unbound").is_none());
        assert_eq!(checker.read_flow_roots.get(&expression), Some(&flow));
        for (selected_owner, selected_property, selected_receiver) in [
            (type_name("Choice$Second"), property(), receiver),
            (
                entry().owner,
                EnumEntryProperty {
                    sibling: 1,
                    ..property()
                },
                receiver,
            ),
            (entry().owner, property(), other),
            (entry().owner, property(), (0, 0)),
            (
                entry().owner,
                EnumEntryProperty {
                    owner: Some(crate::fir::DeclarationId::from_raw(9)),
                    ..property()
                },
                receiver,
            ),
        ] {
            assert!(!checker.publish_enum_entry_property_read(
                &nearer,
                expression,
                selected_owner,
                selected_property,
                selected_receiver
            ));
            assert!(matches!(
                checker.expr_lowers.get(&expression),
                Some(ExprLowering::ClassStorageRead(storage)) if *storage == stale_storage()
            ));
            assert_eq!(checker.read_flow_roots.get(&expression), Some(&flow));
            assert!(!checker
                .implicit_receiver_selections
                .contains_key(&expression));
            assert_eq!(checker.implicit_receiver_identity_use_count(receiver), 0);
            assert_eq!(checker.implicit_receiver_identity_use_count(other), 0);
        }
    });
}

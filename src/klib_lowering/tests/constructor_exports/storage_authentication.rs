//! Declared storage authentication does not turn initializer payloads into executed uses.

use super::*;

/// The fixture whose second declared field repeats the first field's storage identity.
fn repeated_field_fixture(
    before_second: impl FnOnce(&mut Vec<KlibIrMember>, &mut KlibIrArena),
) -> Fixture {
    fixture_with_members(false, RootCase::Known, |members, arena| {
        let KlibIrMember::Field(first) = &members[1] else {
            panic!("the first declared field")
        };
        let first = first.base.symbol.clone();
        let KlibIrMember::Field(second) = &mut members[2] else {
            panic!("the second declared field")
        };
        second.base.symbol = first;
        before_second(members, arena);
    })
}

fn class_decline(f: &Fixture) -> KlibBodyDeclineReason {
    let selected = f.constructor.klib_body_callable().unwrap();
    let facts = KlibCalleeFacts::new([selected]);
    let error = facts
        .constructor_initialization_callees(selected.identity(), &f.bodies)
        .expect_err("the class must decline before it is planned");
    let mut unit = DependencyBodyUnit::default();
    let lowered = unit
        .lower_function(selected, &f.bodies, &facts)
        .unwrap_err();
    assert_eq!(lowered.reason(), &error);
    assert_unit_is_empty(&unit);
    error
}

#[test]
fn declared_storage_authentication_does_not_open_an_unsupported_initializer() {
    let f = fixture(true);
    let selected = f.constructor.klib_body_callable().unwrap();
    let facts = KlibCalleeFacts::new([selected]);
    assert_eq!(
        facts
            .constructor_initialization_callees(selected.identity(), &f.bodies)
            .unwrap(),
        Some(Vec::new())
    );
    let mut unit = DependencyBodyUnit::default();
    let error = unit
        .lower_function(selected, &f.bodies, &facts)
        .unwrap_err();
    assert_eq!(
        error.reason(),
        &KlibBodyDeclineReason::UnsupportedOperation("an omitted argument")
    );
    assert_unit_is_empty(&unit);
}

#[test]
fn storage_authentication_rejects_a_repeated_declared_symbol() {
    let f = repeated_field_fixture(|_, _| {});
    assert_eq!(
        class_decline(&f),
        KlibBodyDeclineReason::SignatureMismatch(
            "a class field has an invalid or repeated storage identity".into()
        )
    );
}

#[test]
fn member_validation_remains_before_the_next_fields_authentication() {
    let f = repeated_field_fixture(|members, _| {
        members.insert(
            2,
            KlibIrMember::AnonymousInitializer {
                base: KlibIrDeclarationBase {
                    flags: 1,
                    ..base(local_symbol(KlibIrSymbolKind::AnonymousInit, 30))
                },
                statements: Vec::new(),
            },
        );
    });
    assert_eq!(
        class_decline(&f),
        KlibBodyDeclineReason::UnsupportedOperation("a static or flagged class initializer")
    );
}

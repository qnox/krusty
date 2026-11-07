//! Declared storage authentication does not turn initializer payloads into executed uses.

use super::*;
use crate::klib_lowering::{class_headers, class_storage};

#[test]
fn declared_storage_authentication_does_not_open_an_unsupported_initializer() {
    let f = fixture(true);
    let selected = f.constructor.klib_body_callable().unwrap();
    let header = class_headers::authenticate(selected, &f.bodies).unwrap();
    let mut coordinates = Vec::new();
    let plan = class_storage::authenticate(
        &header,
        Ty::obj_name(crate::types::wk::any()),
        |_, field| {
            if let Some((index, field)) = field {
                coordinates.push((index, field.base.symbol.clone()));
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        plan.storage.declaration,
        *selected.class_contract().unwrap()
    );
    assert_eq!(
        plan.storage
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.ty))
            .collect::<Vec<_>>(),
        vec![("saved0", Ty::Int), ("saved1", Ty::Int)]
    );
    assert_eq!(
        coordinates,
        plan.field_declarations
            .iter()
            .enumerate()
            .map(|(index, field)| (index as u32, field.base.symbol.clone()))
            .collect::<Vec<_>>()
    );
    let facts = KlibCalleeFacts::new([selected]);
    let mut unit = DependencyBodyUnit::default();
    let error = unit
        .lower_function(selected, &f.bodies, &facts)
        .unwrap_err();
    assert_eq!(
        error.reason(),
        &KlibBodyDeclineReason::UnsupportedOperation("an omitted argument")
    );
    assert!(unit.ir().classes.is_empty());
    assert!(unit.ir().functions.is_empty());
}

#[test]
fn storage_authentication_rejects_a_repeated_declared_symbol() {
    let f = fixture(false);
    let header =
        class_headers::authenticate(f.constructor.klib_body_callable().unwrap(), &f.bodies)
            .unwrap();
    let mut class = header.class.clone();
    let first = class_storage::member_field(&class.members[1])
        .unwrap()
        .base
        .symbol
        .clone();
    let KlibIrMember::Field(second) = &mut class.members[2] else {
        panic!("the second declared field")
    };
    second.base.symbol = first;
    let header = class_headers::ClassMemberHeader {
        class: &class,
        ..header
    };
    let error = class_storage::authenticate(
        &header,
        Ty::obj_name(crate::types::wk::any()),
        |_, _| Ok(()),
    )
    .err()
    .expect("duplicate storage must decline");
    assert_eq!(
        error,
        KlibBodyDeclineReason::SignatureMismatch(
            "a class field has an invalid or repeated storage identity".into()
        )
    );
}

#[test]
fn member_validation_remains_before_the_next_fields_authentication() {
    let f = fixture(false);
    let header =
        class_headers::authenticate(f.constructor.klib_body_callable().unwrap(), &f.bodies)
            .unwrap();
    let mut class = header.class.clone();
    let first = class_storage::member_field(&class.members[1])
        .unwrap()
        .base
        .symbol
        .clone();
    let KlibIrMember::Field(second) = &mut class.members[2] else {
        panic!("the second declared field")
    };
    second.base.symbol = first;
    class.members.insert(
        2,
        KlibIrMember::AnonymousInitializer {
            base: KlibIrDeclarationBase {
                flags: 1,
                ..base(local_symbol(KlibIrSymbolKind::AnonymousInit, 30))
            },
            statements: Vec::new(),
        },
    );
    let header = class_headers::ClassMemberHeader {
        class: &class,
        ..header
    };
    let mut visited_fields = Vec::new();
    let error = class_storage::authenticate(
        &header,
        Ty::obj_name(crate::types::wk::any()),
        |member, field| {
            if let KlibIrMember::AnonymousInitializer { base, .. } = member {
                if base.flags != 0 {
                    return Err(KlibBodyDeclineReason::UnsupportedOperation(
                        "a static or flagged class initializer",
                    ));
                }
            }
            if let Some((index, _)) = field {
                visited_fields.push(index);
            }
            Ok(())
        },
    )
    .err()
    .expect("the earlier initializer marker must decline first");
    assert_eq!(
        error,
        KlibBodyDeclineReason::UnsupportedOperation("a static or flagged class initializer")
    );
    assert_eq!(visited_fields, vec![0]);
}

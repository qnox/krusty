//! A public class of a shape the class path does not materialize keeps its target realization:
//! the class is declined as not this path's before any authentication step can fail.

use super::*;
use crate::metadata::id_signature::KlibAccessorIdSignature;
use crate::metadata::klib_ir::tree::KlibIrProperty;

fn assert_left_to_its_target(f: &Fixture) {
    let selected = f.constructor.klib_body_callable().unwrap();
    let facts = KlibCalleeFacts::new([selected]);
    assert_eq!(
        facts
            .constructor_initialization_callees(selected.identity(), &f.bodies)
            .unwrap(),
        None
    );
}

#[test]
fn a_vararg_constructor_leaves_its_class_to_the_target() {
    let f = fixture_with_members(false, RootCase::Known, |members, arena| {
        let KlibIrMember::Function(constructor) = members[0] else {
            panic!("the constructor")
        };
        let element = arena.push_type(T::Int.klib());
        arena.function_mut(constructor).regular_parameters[0].vararg_element_type = Some(element);
    });
    assert_left_to_its_target(&f);
}

#[test]
fn an_interface_property_override_leaves_its_class_to_the_target() {
    let f = fixture_with_members(false, RootCase::Known, |members, arena| {
        let int = arena.push_type(T::Int.klib());
        let value = arena.push_expr(Some(int), KlibIrExprKind::Const(KlibIrConstant::Int(1)));
        let property = operator(&["demo"], Some("Shape"), "size", &[]).0;
        let getter_symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::Function,
            signature: KlibIrSignature::Accessor(KlibAccessorIdSignature::new(
                operator(&["demo"], Some("Act"), "size", &[]).0,
                "<get-size>".to_owned(),
                1,
                0,
            )),
        };
        let mut getter = function(
            getter_symbol,
            "<get-size>",
            Vec::new(),
            int,
            Some(vec![KlibIrStatement::Expression(value)]),
        );
        getter.overridden = vec![KlibIrSymbol {
            kind: KlibIrSymbolKind::Function,
            signature: KlibIrSignature::Accessor(KlibAccessorIdSignature::new(
                property,
                "<get-size>".to_owned(),
                2,
                0,
            )),
        }];
        let getter = arena.push_function(getter);
        members.push(KlibIrMember::Property(KlibIrProperty {
            base: base(public_symbol(
                KlibIrSymbolKind::Property,
                operator(&["demo"], Some("Act"), "size", &[]).0,
            )),
            name: "size".to_owned(),
            backing_field: None,
            getter: Some(getter),
            setter: None,
        }));
    });
    assert_left_to_its_target(&f);
}

#[test]
fn a_bodyless_member_leaves_its_class_to_the_target() {
    let f = fixture_with_members(false, RootCase::Known, |members, arena| {
        let unit = arena.push_type(T::Unit.klib());
        let member = arena.push_function(function(
            public_symbol(
                KlibIrSymbolKind::Function,
                operator(&["demo"], Some("Act"), "run", &[]).0,
            ),
            "run",
            Vec::new(),
            unit,
            None,
        ));
        members.push(KlibIrMember::Function(member));
    });
    assert_left_to_its_target(&f);
}

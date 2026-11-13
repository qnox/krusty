//! A public class of a shape the class path does not materialize keeps its target realization:
//! the class is declined as not this path's before any authentication step can fail.

use super::*;

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

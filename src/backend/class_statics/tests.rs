//! The class static plan over common IR built by hand.

use super::{ClassActivationStep, ClassStaticPlan, UnorderedClassStatic};
use crate::ir::{ClassId, IrAccessorLines, IrConst, IrExpr, IrFile, IrStatic, IrStaticAccessors};
use crate::types::{type_name, Ty, Visibility};

fn class(ir: &mut IrFile, name: &str) -> ClassId {
    ir.add_class(crate::ir::test_support::blank_class(name))
}

/// A static of `owner` (the file when `None`), initialized to `value` when it has one.
fn static_of(ir: &mut IrFile, name: &str, owner: Option<&str>, value: Option<i32>) -> u32 {
    let init = value.map(|value| ir.add_expr(IrExpr::Const(IrConst::Int(value))));
    let index = u32::try_from(ir.statics.len()).expect("few statics");
    ir.statics.push(IrStatic {
        name: name.to_owned(),
        ty: Ty::Int,
        init,
        is_var: false,
        is_const: false,
        is_lateinit: false,
        owner: owner.map(type_name),
        visibility: Visibility::Public,
        erased_declared_ty: None,
        accessors: IrStaticAccessors::DEFAULT,
        line: 0,
        accessor_lines: IrAccessorLines::default(),
        source_order: index,
        property_delegate: false,
    });
    index
}

#[test]
fn a_class_runs_its_own_statics_in_declaration_order_at_each_activation_point() {
    let mut ir = IrFile::default();
    let plain = class(&mut ir, "p/Plain");
    let owner = class(&mut ir, "p/Owner");
    let facade = static_of(&mut ir, "top", None, Some(1));
    let first = static_of(&mut ir, "first", Some("p/Owner"), Some(2));
    let constant = static_of(&mut ir, "constant", Some("p/Owner"), Some(3));
    ir.statics[constant as usize].is_const = true;
    let uninitialized = static_of(&mut ir, "uninitialized", Some("p/Owner"), None);
    let second = static_of(&mut ir, "second", Some("p/Owner"), Some(4));

    let plan = ClassStaticPlan::new(&ir).expect("an ordinary class orders its statics");

    let groups: Vec<(ClassId, Vec<u32>)> = plan
        .classes()
        .iter()
        .map(|group| (group.class, group.statics.to_vec()))
        .collect();
    assert_eq!(groups, vec![(owner, vec![first, second])]);
    let deferred: Vec<bool> = [facade, first, constant, uninitialized, second]
        .map(|index| plan.is_deferred(index))
        .to_vec();
    assert_eq!(deferred, vec![false, true, false, false, true]);
    let accesses: Vec<Option<ClassId>> = [facade, first, constant, uninitialized, second]
        .map(|index| plan.access_activation(index))
        .to_vec();
    assert_eq!(accesses, vec![None, Some(owner), None, None, Some(owner)]);
    assert_eq!(
        (plan.constructor_entry(owner), plan.constructor_entry(plain)),
        (Some(owner), None)
    );
    assert_eq!(
        plan.activation_steps(owner).collect::<Vec<_>>(),
        vec![
            ClassActivationStep::OwnStatics(owner),
            ClassActivationStep::Companion(owner),
        ]
    );
    assert_eq!(
        plan.activation_steps(plain).collect::<Vec<_>>(),
        vec![ClassActivationStep::Companion(plain)]
    );
}

#[test]
fn a_class_with_other_initialization_parts_declines_an_owned_static() {
    for shape in ["object", "interface", "value", "companion", "enum"] {
        let mut ir = IrFile::default();
        let mut declaration = crate::ir::test_support::blank_class("p/Owner");
        match shape {
            "object" => declaration.is_object = true,
            "interface" => declaration.is_interface = true,
            "value" => declaration.is_value = true,
            "companion" => declaration.companion_class = Some(type_name("p/Owner$Companion")),
            _ => declaration.superclass = crate::types::wk::kotlin_enum(),
        }
        ir.add_class(declaration);
        static_of(&mut ir, "owned", Some("p/Owner"), Some(1));

        let declined = ClassStaticPlan::new(&ir).map(|_| ());

        assert_eq!(
            declined,
            Err(UnorderedClassStatic {
                owner: type_name("p/Owner")
            }),
            "{shape}"
        );
    }
}

#[test]
fn an_initialization_order_nothing_observes_runs_with_the_file_even_on_an_object() {
    let mut ir = IrFile::default();
    let mut declaration = crate::ir::test_support::blank_class("p/Owner");
    declaration.is_object = true;
    ir.add_class(declaration);
    let uninitialized = static_of(&mut ir, "uninitialized", Some("p/Owner"), None);
    let constant = static_of(&mut ir, "constant", Some("p/Owner"), Some(1));
    ir.statics[constant as usize].is_const = true;

    let plan = ClassStaticPlan::new(&ir).expect("nothing waits for the object");

    assert!(plan.classes().is_empty());
    assert_eq!(
        (plan.is_deferred(uninitialized), plan.is_deferred(constant)),
        (false, false)
    );
}

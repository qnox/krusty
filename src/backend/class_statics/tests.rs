//! The class static plan over common IR built by hand.

use super::{ClassStaticPlan, UnorderedClassStatic};
use crate::ir::{
    ClassId, IrAccessorLines, IrClassActivationStep, IrConst, IrExpr, IrFile, IrStatic,
    IrStaticAccessors,
};
use crate::types::{type_name, Ty, Visibility};

/// A class of this file whose recorded activation runs its own statics when `statics` holds.
fn class(ir: &mut IrFile, name: &str, statics: bool) -> ClassId {
    let mut declaration = crate::ir::test_support::blank_class(name);
    if statics {
        declaration.activation = Box::new([IrClassActivationStep::OwnStatics(type_name(name))]);
    }
    ir.add_class(declaration)
}

/// A static of `owner` (the file when `None`), waiting for its class when `deferred` holds.
fn static_of(ir: &mut IrFile, name: &str, owner: Option<&str>, deferred: bool) -> u32 {
    let init = Some(ir.add_expr(IrExpr::Const(IrConst::Int(1))));
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
        deferred,
    });
    index
}

#[test]
fn a_class_runs_its_deferred_statics_in_declaration_order_at_each_activation_point() {
    let mut ir = IrFile::default();
    let plain = class(&mut ir, "p/Plain", false);
    let owner = class(&mut ir, "p/Owner", true);
    let facade = static_of(&mut ir, "top", None, false);
    let first = static_of(&mut ir, "first", Some("p/Owner"), true);
    let with_the_file = static_of(&mut ir, "constant", Some("p/Owner"), false);
    let second = static_of(&mut ir, "second", Some("p/Owner"), true);

    let plan = ClassStaticPlan::new(&ir).expect("an ordinary class orders its statics");

    let groups: Vec<(ClassId, Vec<u32>)> = plan
        .classes()
        .iter()
        .map(|group| (group.class, group.statics.to_vec()))
        .collect();
    assert_eq!(groups, vec![(owner, vec![first, second])]);
    let accesses: Vec<Option<ClassId>> = [facade, first, with_the_file, second]
        .map(|index| plan.access_activation(index))
        .to_vec();
    assert_eq!(accesses, vec![None, Some(owner), None, Some(owner)]);
    assert_eq!(
        (plan.constructor_entry(owner), plan.constructor_entry(plain)),
        (Some(owner), None)
    );
}

#[test]
fn a_class_whose_activation_runs_its_statics_has_an_initializer_with_none_deferred() {
    let mut ir = IrFile::default();
    let owner = class(&mut ir, "p/Owner", true);
    let with_the_file = static_of(&mut ir, "reference", Some("p/Owner"), false);

    let plan = ClassStaticPlan::new(&ir).expect("an empty initializer is still one");

    assert!(plan.has_statics(owner));
    assert_eq!(plan.classes()[0].statics.len(), 0);
    assert_eq!(plan.access_activation(with_the_file), None);
}

#[test]
fn a_class_with_other_initialization_parts_declines_a_deferred_static() {
    for shape in ["object", "value", "enum"] {
        let mut ir = IrFile::default();
        let mut declaration = crate::ir::test_support::blank_class("p/Owner");
        match shape {
            "object" => declaration.is_object = true,
            "value" => declaration.is_value = true,
            _ => declaration.is_enum = true,
        }
        declaration.activation =
            Box::new([IrClassActivationStep::OwnStatics(type_name("p/Owner"))]);
        ir.add_class(declaration);
        static_of(&mut ir, "owned", Some("p/Owner"), true);

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
fn a_static_with_the_file_runs_with_the_file_even_on_an_object() {
    let mut ir = IrFile::default();
    let mut declaration = crate::ir::test_support::blank_class("p/Owner");
    declaration.is_object = true;
    ir.add_class(declaration);
    let owned = static_of(&mut ir, "owned", Some("p/Owner"), false);

    let plan = ClassStaticPlan::new(&ir).expect("nothing waits for the object");

    assert!(plan.classes().is_empty());
    assert!(!plan.is_deferred(owned));
}

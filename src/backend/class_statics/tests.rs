//! The class static plan over common IR built by hand.

use super::{ClassActivationStep, ClassStaticPlan, UnorderedClassStatic};
use crate::ir::{
    ClassId, IrAccessorLines, IrConst, IrExpr, IrFile, IrFunction, IrStatic, IrStaticAccessors,
};
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

    let plan = ClassStaticPlan::new(&ir, true).expect("an ordinary class orders its statics");

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
    for shape in ["object", "value", "enum"] {
        let mut ir = IrFile::default();
        let mut declaration = crate::ir::test_support::blank_class("p/Owner");
        match shape {
            "object" => declaration.is_object = true,
            "value" => declaration.is_value = true,
            _ => declaration.superclass = crate::types::wk::kotlin_enum(),
        }
        ir.add_class(declaration);
        static_of(&mut ir, "owned", Some("p/Owner"), Some(1));

        let declined = ClassStaticPlan::new(&ir, true).map(|_| ());

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

    let plan = ClassStaticPlan::new(&ir, true).expect("nothing waits for the object");

    assert!(plan.classes().is_empty());
    assert_eq!(
        (plan.is_deferred(uninitialized), plan.is_deferred(constant)),
        (false, false)
    );
}

#[test]
fn an_interface_runs_its_own_statics_when_its_storage_or_block_functions_are_used() {
    for companion in [false, true] {
        let mut ir = IrFile::default();
        let mut declaration = crate::ir::test_support::blank_class("p/Owner");
        declaration.is_interface = true;
        if companion {
            declaration.companion_class = Some(type_name("p/Owner$Companion"));
        }
        let owner = ir.add_class(declaration);
        let first = static_of(&mut ir, "first", Some("p/Owner"), Some(1));
        let second = static_of(&mut ir, "second", Some("p/Owner"), Some(2));

        let plan = ClassStaticPlan::new(&ir, true).expect("an interface orders its statics");

        let groups: Vec<(ClassId, Vec<u32>)> = plan
            .classes()
            .iter()
            .map(|group| (group.class, group.statics.to_vec()))
            .collect();
        assert_eq!(groups, vec![(owner, vec![first, second])], "{companion}");
        assert_eq!(
            [first, second].map(|index| plan.access_activation(index)),
            [Some(owner), Some(owner)],
            "{companion}"
        );
    }
}

#[test]
fn a_class_with_a_companion_object_runs_its_statics_before_the_companion() {
    let mut ir = IrFile::default();
    let mut declaration = crate::ir::test_support::blank_class("p/Owner");
    declaration.companion_class = Some(type_name("p/Owner$Companion"));
    let owner = ir.add_class(declaration);
    let owned = static_of(&mut ir, "owned", Some("p/Owner"), Some(1));

    let plan =
        ClassStaticPlan::new(&ir, true).expect("a companion object follows the class's statics");

    assert_eq!(plan.access_activation(owned), Some(owner));
    assert_eq!(
        plan.activation_steps(owner).collect::<Vec<_>>(),
        vec![
            ClassActivationStep::OwnStatics(owner),
            ClassActivationStep::Companion(owner),
        ]
    );
}

#[test]
fn a_class_runs_its_statics_around_the_companion_in_program_order() {
    let mut ir = IrFile::default();
    let mut declaration = crate::ir::test_support::blank_class("p/Owner");
    declaration.companion_class = Some(type_name("p/Owner$Companion"));
    let owner = ir.add_class(declaration);
    let companion = class(&mut ir, "p/Owner$Companion");
    ir.record_class_source_order(owner, 0);
    ir.record_class_source_order(companion, 3);
    // Written after the companion, then before it: the plan keeps each side in declaration order.
    let after = static_of(&mut ir, "after", Some("p/Owner"), Some(1));
    ir.statics[after as usize].source_order = 5;
    let early = static_of(&mut ir, "early", Some("p/Owner"), Some(2));
    ir.statics[early as usize].source_order = 1;
    let late = static_of(&mut ir, "late", Some("p/Owner"), Some(3));
    ir.statics[late as usize].source_order = 6;

    let plan =
        ClassStaticPlan::new(&ir, true).expect("a class with a companion orders its statics");

    let groups: Vec<(Vec<u32>, Option<usize>)> = plan
        .classes()
        .iter()
        .map(|group| (group.statics.to_vec(), group.before_companion))
        .collect();
    assert_eq!(groups, vec![(vec![early, after, late], Some(1))]);

    // Before 2.5 the companion's body runs after every static, whatever order they were written.
    let plan =
        ClassStaticPlan::new(&ir, false).expect("a class with a companion orders its statics");
    let groups: Vec<(Vec<u32>, Option<usize>)> = plan
        .classes()
        .iter()
        .map(|group| (group.statics.to_vec(), group.before_companion))
        .collect();
    assert_eq!(groups, vec![(vec![after, early, late], None)]);
}

/// An interface of this file, with a non-abstract member when `default_member` holds.
fn interface(ir: &mut IrFile, name: &str, extends: &[&str], default_member: bool) -> ClassId {
    let mut declaration = crate::ir::test_support::blank_class(name);
    declaration.is_interface = true;
    for parent in extends {
        declaration.interfaces.push(parent);
    }
    let body = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let method = ir.add_fun(IrFunction {
        name: "member".to_owned(),
        params: Vec::new(),
        ret: Ty::Int,
        body: default_member.then_some(body),
        is_static: false,
        dispatch_receiver: Some(type_name(name)),
        param_checks: Vec::new(),
    });
    declaration.methods.push(method);
    ir.add_class(declaration)
}

#[test]
fn a_class_initializes_its_superinterfaces_with_statics_and_default_members_first() {
    let mut ir = IrFile::default();
    let root = interface(&mut ir, "p/Root", &[], true);
    let first = interface(&mut ir, "p/First", &["p/Root"], true);
    let abstract_only = interface(&mut ir, "p/AbstractOnly", &[], false);
    let without_statics = interface(&mut ir, "p/WithoutStatics", &[], true);
    let second = interface(&mut ir, "p/Second", &["p/Root"], true);
    let mut declaration = crate::ir::test_support::blank_class("p/Impl");
    for name in ["p/First", "p/AbstractOnly", "p/WithoutStatics", "p/Second"] {
        declaration.interfaces.push(name);
    }
    let implementation = ir.add_class(declaration);
    for owner in ["p/Root", "p/First", "p/AbstractOnly", "p/Second", "p/Impl"] {
        static_of(&mut ir, "value", Some(owner), Some(1));
    }

    let plan = ClassStaticPlan::new(&ir, true).expect("interfaces order their statics");

    assert!(!plan.has_statics(without_statics));
    assert!(plan.has_statics(abstract_only));
    assert_eq!(
        plan.activation_steps(implementation).collect::<Vec<_>>(),
        vec![
            ClassActivationStep::OwnStatics(root),
            ClassActivationStep::Companion(root),
            ClassActivationStep::OwnStatics(first),
            ClassActivationStep::Companion(first),
            ClassActivationStep::OwnStatics(second),
            ClassActivationStep::Companion(second),
            ClassActivationStep::OwnStatics(implementation),
            ClassActivationStep::Companion(implementation),
        ]
    );
    assert_eq!(
        plan.activation_steps(first).collect::<Vec<_>>(),
        vec![
            ClassActivationStep::OwnStatics(first),
            ClassActivationStep::Companion(first),
        ],
        "an interface does not initialize its superinterfaces"
    );
}

use super::*;
use crate::ir::IrAnnotationConstruction;
use crate::libraries::AnnotationArgumentProvenance;
use crate::types::type_name;

struct Fixture {
    root: ExprId,
    allocation: ExprId,
    values: [ExprId; 2],
    bindings: [ExprId; 2],
    ty: Ty,
}

fn reversed_members() -> (IrFile, Fixture) {
    let mut ir = IrFile::default();
    let choice = type_name("sample/Choice");
    let interface = type_name("sample/Pair");
    let ty = Ty::obj_name(interface);
    let values = ["LEFT", "RIGHT"].map(|name| {
        ir.add_expr(IrExpr::EnumEntry {
            classifier: choice,
            name: name.into(),
        })
    });
    ir.set_source_line(values[0], 20);
    ir.set_source_line(values[1], 19);
    for &value in &values {
        ir.set_logical_type(value, Ty::obj_name(choice));
    }
    let bindings = [1, 0].map(|member| {
        let binding = ir.add_expr(IrExpr::Variable {
            index: member as u32,
            ty: Ty::obj_name(choice),
            init: Some(values[member]),
            named: false,
        });
        ir.call_operand_bindings.insert(binding);
        ir.argument_evaluation_bindings.insert(binding);
        binding
    });
    let args = [0, 1]
        .map(|index| ir.add_expr(IrExpr::GetValue(index)))
        .to_vec();
    let allocation = ir.add_expr(IrExpr::New {
        internal: interface,
        args,
        ctor_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    ir.annotation_constructions.insert(
        allocation,
        IrAnnotationConstruction {
            interface,
            members: vec![
                ("first".into(), Ty::obj_name(choice)),
                ("second".into(), Ty::obj_name(choice)),
            ],
            defaults: vec![None, None],
            arguments: AnnotationArgumentProvenance::from_selected(2, &[1, 0], &[]).unwrap(),
            scopes: vec![None],
        },
    );
    let root = ir.add_expr(IrExpr::Block {
        stmts: bindings.to_vec(),
        value: Some(allocation),
    });
    (
        ir,
        Fixture {
            root,
            allocation,
            values,
            bindings,
            ty,
        },
    )
}

#[test]
fn declaration_order_uses_original_children_and_source_facts() {
    let (mut ir, fixture) = reversed_members();
    let positions = fixture.values.map(|value| ir.position(value));
    let types = ir
        .logical_types()
        .collect::<std::collections::BTreeMap<_, _>>();
    let arguments = ir.annotation_constructions[&fixture.allocation]
        .arguments
        .clone();
    Plan::build(&ir, fixture.root, fixture.ty)
        .unwrap()
        .apply(&mut ir);
    let IrExpr::Block { stmts, value } = ir.expr(fixture.root) else {
        panic!("block root")
    };
    assert!(stmts.is_empty());
    assert_eq!(*value, Some(fixture.allocation));
    let IrExpr::New {
        args,
        defaults,
        default_prefix_count,
        ..
    } = ir.expr(fixture.allocation)
    else {
        panic!("original annotation construction")
    };
    assert_eq!(args, &fixture.values);
    assert!(defaults.is_empty());
    assert_eq!(*default_prefix_count, 0);
    assert_eq!(fixture.values.map(|value| ir.position(value)), positions);
    assert_eq!(
        ir.logical_types()
            .collect::<std::collections::BTreeMap<_, _>>(),
        types
    );
    assert_eq!(
        ir.annotation_constructions[&fixture.allocation].arguments,
        arguments
    );
}

#[test]
fn omitted_arguments_keep_the_runtime_mask_and_sparse_arity() {
    let (mut ir, fixture) = reversed_members();
    let IrExpr::Block { stmts, .. } = &mut ir.exprs[fixture.root as usize] else {
        panic!("block")
    };
    stmts.pop();
    let IrExpr::New { args, defaults, .. } = &mut ir.exprs[fixture.allocation as usize] else {
        panic!("new")
    };
    args.remove(0);
    *defaults = Box::new([0]);
    let site = ir
        .annotation_constructions
        .get_mut(&fixture.allocation)
        .unwrap();
    site.defaults[0] = Some(fixture.values[0]);
    site.arguments = AnnotationArgumentProvenance::from_selected(2, &[1], &[0]).unwrap();
    let arguments = site.arguments.clone();
    let declaration_defaults = site.defaults.clone();
    Plan::build(&ir, fixture.root, fixture.ty)
        .unwrap()
        .apply(&mut ir);
    let IrExpr::New { args, defaults, .. } = ir.expr(fixture.allocation) else {
        panic!("new")
    };
    assert_eq!(args, &[fixture.values[1]]);
    assert_eq!(defaults.as_ref(), [0]);
    assert_eq!(
        ir.annotation_constructions[&fixture.allocation].arguments,
        arguments
    );
    assert_eq!(
        ir.annotation_constructions[&fixture.allocation].defaults,
        declaration_defaults
    );
}

#[test]
fn inline_formal_bindings_are_not_argument_order_holders() {
    let (mut ir, fixture) = reversed_members();
    ir.argument_evaluation_bindings.remove(&fixture.bindings[0]);
    assert!(Plan::build(&ir, fixture.root, fixture.ty).is_none());
}

#[test]
fn duplicated_reads_do_not_duplicate_an_initializer() {
    let (mut ir, fixture) = reversed_members();
    let IrExpr::New { args, .. } = &mut ir.exprs[fixture.allocation as usize] else {
        panic!("new")
    };
    args[1] = args[0];
    assert!(Plan::build(&ir, fixture.root, fixture.ty).is_none());
}

#[test]
fn metadata_omission_does_not_hide_an_unclosed_supplied_runtime_value() {
    let (mut ir, fixture) = reversed_members();
    let unknown = ir.add_expr(IrExpr::GetValue(99));
    let IrExpr::New { args, .. } = &mut ir.exprs[fixture.allocation as usize] else {
        panic!("new")
    };
    args[0] = unknown;
    args[1] = fixture.values[1];
    ir.annotation_constructions
        .get_mut(&fixture.allocation)
        .unwrap()
        .arguments = AnnotationArgumentProvenance::from_selected(2, &[1], &[0]).unwrap();
    assert!(super::super::element_value(&ir, fixture.allocation, fixture.ty).is_none());
}

#[test]
fn shared_ordinary_parents_leave_the_entire_default_unchanged() {
    let (mut ir, fixture) = reversed_members();
    normalize_roots(
        &mut ir,
        vec![(fixture.root, fixture.ty)],
        vec![fixture.allocation],
    );
    let IrExpr::Block { stmts, .. } = ir.expr(fixture.root) else {
        panic!("block")
    };
    assert_eq!(stmts, &fixture.bindings);
    let IrExpr::New { args, .. } = ir.expr(fixture.allocation) else {
        panic!("new")
    };
    assert!(args
        .iter()
        .all(|&arg| matches!(ir.expr(arg), IrExpr::GetValue(_))));
}

fn shared_delegate_template(is_getter: bool) {
    use crate::fir::{BodyOwnerId, FirLiftingSite, LocalDelegatedPropertyId, SourceFileId};
    use crate::ir::{
        IrLocalDelegateAccessorPlan, IrLocalDelegatePlan, IrLocalPropertyReference, IrModuleSource,
    };

    let (mut ir, fixture) = reversed_members();
    let other = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
    let accessor = |body| IrLocalDelegateAccessorPlan {
        body,
        parameters: Vec::new(),
        parameter_identities: Vec::new(),
        type_parameters: Vec::new(),
        captured_receivers: Vec::new(),
        result: fixture.ty,
        source_order: 0,
        site: FirLiftingSite {
            owner: "".into(),
            container: "test".into(),
            path: Box::new([]),
            lifted: true,
        },
        line: 1,
    };
    ir.local_delegate_plans.push(IrLocalDelegatePlan {
        inline_declaration: None,
        declaration_lambda: None,
        reference: IrLocalPropertyReference {
            name: "p".into(),
            property_type: fixture.ty,
            declaration: LocalDelegatedPropertyId::new(BodyOwnerId::from_raw(0), 0),
            mutable: true,
            class: None,
            source: IrModuleSource {
                source: SourceFileId::from_raw(0),
                package: type_name("sample"),
            },
            ordinal: 0,
            member_order: 0,
        },
        storage_name: "p$delegate".into(),
        getter: accessor(if is_getter { fixture.allocation } else { other }),
        setter: Some(accessor(if is_getter { other } else { fixture.allocation })),
    });
    let original = format!("{:?}", ir.exprs);
    let protected = ordinary_roots(&ir);
    normalize_roots(&mut ir, vec![(fixture.root, fixture.ty)], protected);
    assert_eq!(format!("{:?}", ir.exprs), original);
}

#[test]
fn retained_delegate_getter_protects_the_shared_default() {
    shared_delegate_template(true);
}

#[test]
fn retained_delegate_setter_protects_the_shared_default() {
    shared_delegate_template(false);
}

#[test]
fn incompatible_scoped_bindings_protect_both_shared_roots() {
    let (mut ir, fixture) = reversed_members();
    let bindings = [1, 0].map(|index| {
        let binding = ir.add_expr(IrExpr::Variable {
            index,
            ty: Ty::obj("sample/Choice"),
            init: Some(fixture.values[1 - index as usize]),
            named: false,
        });
        ir.argument_evaluation_bindings.insert(binding);
        ir.call_operand_bindings.insert(binding);
        binding
    });
    let other = ir.add_expr(IrExpr::Block {
        stmts: bindings.to_vec(),
        value: Some(fixture.allocation),
    });
    let original = format!("{:?}", ir.exprs);
    normalize_roots(
        &mut ir,
        vec![(fixture.root, fixture.ty), (other, fixture.ty)],
        Vec::new(),
    );
    assert_eq!(format!("{:?}", ir.exprs), original);
}

#[test]
fn excluded_root_protection_reaches_an_earlier_nested_plan() {
    let (mut ir, fixture) = reversed_members();
    let binding = ir.add_expr(IrExpr::Variable {
        index: 42,
        ty: Ty::obj("sample/Choice"),
        init: Some(fixture.values[1]),
        named: false,
    });
    ir.argument_evaluation_bindings.insert(binding);
    ir.call_operand_bindings.insert(binding);
    let read = ir.add_expr(IrExpr::GetValue(42));
    let nested = ir.add_expr(IrExpr::Block {
        stmts: vec![binding],
        value: Some(read),
    });
    let IrExpr::Variable { init, .. } = &mut ir.exprs[fixture.bindings[0] as usize] else {
        panic!("holder")
    };
    *init = Some(nested);
    assert!(Plan::build(&ir, nested, Ty::obj("sample/Choice")).is_some());
    let original = format!("{:?}", ir.exprs);
    normalize_roots(
        &mut ir,
        vec![
            (nested, Ty::obj("sample/Choice")),
            (fixture.root, fixture.ty),
        ],
        vec![fixture.allocation],
    );
    assert_eq!(format!("{:?}", ir.exprs), original);
}

#[test]
fn nested_spreads_and_coercions_keep_unsigned_identity_and_element_order() {
    let mut ir = IrFile::default();
    let array_ty = Ty::array(Ty::UInt);
    let maximum = ir.add_expr(IrExpr::Const(crate::ir::IrConst::UInt(u32::MAX)));
    ir.set_logical_type(maximum, Ty::UInt);
    let binding = ir.add_expr(IrExpr::Variable {
        index: 0,
        ty: Ty::UInt,
        init: Some(maximum),
        named: false,
    });
    ir.call_operand_bindings.insert(binding);
    ir.argument_evaluation_bindings.insert(binding);
    let read = ir.add_expr(IrExpr::GetValue(0));
    let conversion = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: read,
        type_operand: Ty::UInt,
    });
    let inner = ir.add_expr(IrExpr::Vararg {
        array_type: array_ty,
        elements: vec![conversion],
        spreads: Vec::new(),
    });
    let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::UInt(0)));
    let outer = ir.add_expr(IrExpr::Vararg {
        array_type: array_ty,
        elements: vec![inner, zero],
        spreads: vec![true, false],
    });
    let root = ir.add_expr(IrExpr::Block {
        stmts: vec![binding],
        value: Some(outer),
    });
    Plan::build(&ir, root, array_ty).unwrap().apply(&mut ir);
    let IrExpr::Vararg {
        elements,
        spreads,
        array_type,
    } = ir.expr(outer)
    else {
        panic!("array")
    };
    assert_eq!(elements, &[inner, zero]);
    assert_eq!(spreads, &[true, false]);
    assert_eq!(*array_type, array_ty);
    let IrExpr::TypeOp {
        arg, type_operand, ..
    } = ir.expr(conversion)
    else {
        panic!("coercion")
    };
    assert_eq!(*arg, maximum);
    assert_eq!(*type_operand, Ty::UInt);
    assert!(matches!(
        ir.expr(maximum),
        IrExpr::Const(crate::ir::IrConst::UInt(u32::MAX))
    ));
    assert_eq!(ir.logical_type(maximum), Some(Ty::UInt));
}

#[test]
fn normalization_is_idempotent_and_retains_every_construction_id() {
    let (mut ir, fixture) = reversed_members();
    let count = ir.exprs.len();
    normalize_roots(&mut ir, vec![(fixture.root, fixture.ty)], Vec::new());
    assert_eq!(ir.exprs.len(), count);
    assert!(Plan::build(&ir, fixture.root, fixture.ty)
        .unwrap()
        .edits
        .is_empty());
    normalize_roots(&mut ir, vec![(fixture.root, fixture.ty)], Vec::new());
    assert_eq!(ir.exprs.len(), count);
    assert!(ir
        .annotation_constructions
        .contains_key(&fixture.allocation));
}

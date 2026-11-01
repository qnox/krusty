//! Constructor entries: one more constructor of the class, reached by its exact place.

use super::*;
use crate::ir::{
    ClassId, CtorDelegateTarget, IrConstructorAccess, IrConstructorTarget, IrCtorArg,
    IrCtorParameterProvenance, IrEnumEntry, IrSecondaryCtor,
};

fn parameter(ty: Ty, provenance: IrCtorParameterProvenance) -> IrCtorArg {
    IrCtorArg {
        name: None,
        context_kind: crate::types::ContextParameterKind::None,
        ty,
        declared_ty: None,
        is_field: false,
        field_index: None,
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture: None,
        provenance,
        capture_identity: None,
    }
}

/// A class of this file whose primary constructor takes `prefix` compiler-supplied parameters
/// and then `parameters`, the latter defaulting to `defaults`.
fn class_with_primary(
    ir: &mut IrFile,
    name: &str,
    prefix: Vec<Ty>,
    parameters: Vec<Ty>,
    defaults: Vec<Option<ExprId>>,
) -> ClassId {
    let mut class = crate::ir::test_support::blank_class(name);
    class.constructor_prefix_count = u32::try_from(prefix.len()).expect("a short prefix");
    class.ctor_args = prefix
        .iter()
        .map(|&ty| parameter(ty, IrCtorParameterProvenance::EnclosingInstance))
        .chain(
            parameters
                .iter()
                .map(|&ty| parameter(ty, IrCtorParameterProvenance::Value)),
        )
        .collect();
    let class = ir.add_class(class);
    let mut physical = vec![None; prefix.len()];
    physical.extend(defaults);
    ir.insert_class_ctor_defaults_name(type_name(name), physical);
    class
}

fn secondary(
    prefix: Vec<Ty>,
    params: Vec<Ty>,
    defaults: Vec<Option<ExprId>>,
    delegate: CtorDelegateTarget,
    delegate_args: Vec<ExprId>,
    default_parameters: Vec<u32>,
) -> IrSecondaryCtor {
    IrSecondaryCtor {
        annotations: Default::default(),
        source_order: 0,
        lines: Default::default(),
        prefix_params: prefix,
        params,
        declared_spellings: Default::default(),
        named_params: Vec::new(),
        metadata_visibility: Some(crate::types::Visibility::Public),
        generated_debug: Default::default(),
        vararg_index: None,
        defaults,
        delegate_prelude: Vec::new(),
        delegate_args,
        default_parameters,
        body: None,
        delegate,
        synthetic: false,
        vc_params: false,
        param_checks: Vec::new(),
    }
}

fn target(ordinal: u32) -> IrConstructorTarget {
    IrConstructorTarget {
        ordinal,
        access: IrConstructorAccess::Unrestricted,
    }
}

/// `New(name, args)` selecting constructor `ordinal`, leaving `defaults` out, in a function body.
fn construction(
    ir: &mut IrFile,
    name: &str,
    args: Vec<ExprId>,
    ctor_params: Option<Vec<Ty>>,
    defaults: &[u32],
    prefix: u32,
    ordinal: Option<u32>,
) -> ExprId {
    let site = ir.add_expr(IrExpr::New {
        internal: type_name(name),
        args,
        ctor_params,
        ctor_desc: None,
        external_target: None,
        defaults: defaults.into(),
        default_prefix_count: prefix,
    });
    if let Some(ordinal) = ordinal {
        ir.construction_targets.insert(site, target(ordinal));
    }
    declare(ir, "caller", Vec::new(), Ty::Unit, Some(site), None);
    site
}

fn construction_shape(ir: &IrFile, site: ExprId) -> (String, Option<Vec<Ty>>, Vec<u32>) {
    let IrExpr::New {
        args,
        ctor_params,
        defaults,
        ..
    } = ir.expr(site)
    else {
        panic!("a construction");
    };
    (render_all(ir, args), ctor_params.clone(), defaults.to_vec())
}

fn delegation_of(constructor: &IrSecondaryCtor) -> (u32, Vec<Ty>) {
    match &constructor.delegate {
        CtorDelegateTarget::This {
            target,
            target_params,
            ..
        }
        | CtorDelegateTarget::Super {
            target,
            target_params,
            ..
        } => (target.ordinal, target_params.clone()),
        CtorDelegateTarget::ImplicitEnumBase => panic!("a written delegation"),
    }
}

#[test]
fn a_construction_reaches_a_constructor_entry_that_fills_and_delegates() {
    let mut ir = IrFile::default();
    let earlier = ir.add_expr(IrExpr::GetValue(1));
    let one = int(&mut ir, 1);
    let default = ir.add_arithmetic(IrBinOp::Add, earlier, one, Ty::Int);
    let class = class_with_primary(
        &mut ir,
        "demo/C",
        Vec::new(),
        vec![Ty::Int, Ty::Int],
        vec![None, Some(default)],
    );
    let five = int(&mut ir, 5);
    let site = construction(&mut ir, "demo/C", vec![five], None, &[1], 0, Some(0));

    realize(&mut ir, &mut CheckedBackendCallables::default());

    let constructors = &ir.classes[class as usize].secondary_ctors;
    assert_eq!(constructors.len(), 1);
    let entry = &constructors[0];
    assert_eq!(entry.prefix_params, Vec::<Ty>::new());
    assert_eq!(entry.params, vec![Ty::Int; 3]);
    assert_eq!(entry.defaults, vec![None; 3]);
    assert_eq!(entry.metadata_visibility, None);
    assert!(entry.synthetic);
    assert_eq!(entry.default_parameters, Vec::<u32>::new());
    assert_eq!(entry.body, None);
    assert_eq!(
        render_all(&ir, &entry.delegate_prelude),
        fill(3, 2, 2, "($1 Add 1)")
    );
    assert_eq!(render_all(&ir, &entry.delegate_args), "$1, $2");
    assert!(matches!(entry.delegate, CtorDelegateTarget::This { .. }));
    assert_eq!(delegation_of(entry), (0, vec![Ty::Int, Ty::Int]));
    assert_eq!(
        construction_shape(&ir, site),
        (
            "5, absent Int, 2".to_owned(),
            Some(vec![Ty::Int; 3]),
            Vec::new()
        )
    );
    assert_eq!(ir.construction_targets[&site], target(1));
    assert_eq!(
        crate::backend::selected_constructor(&ir, site, class, Some(&[Ty::Int; 3])),
        Some(1)
    );
}

#[test]
fn an_entry_whose_parameter_list_a_declared_constructor_has_is_reached_by_its_place() {
    let mut ir = IrFile::default();
    let seven = int(&mut ir, 7);
    let class = class_with_primary(
        &mut ir,
        "demo/C",
        Vec::new(),
        vec![Ty::Int, Ty::Int],
        vec![None, Some(seven)],
    );
    // `constructor(a: Int, b: Int, c: Int) : this(a, b)` takes the entry's list.
    let (a, b) = (
        ir.add_expr(IrExpr::GetValue(1)),
        ir.add_expr(IrExpr::GetValue(2)),
    );
    ir.classes[class as usize].secondary_ctors.push(secondary(
        Vec::new(),
        vec![Ty::Int; 3],
        vec![None; 3],
        CtorDelegateTarget::This {
            target_params: vec![Ty::Int, Ty::Int],
            target: target(0),
            default_masks: Vec::new(),
        },
        vec![a, b],
        Vec::new(),
    ));
    let one = int(&mut ir, 1);
    let defaulted = construction(&mut ir, "demo/C", vec![one], None, &[1], 0, Some(0));
    let (x, y, z) = (int(&mut ir, 1), int(&mut ir, 2), int(&mut ir, 3));
    let declared = construction(
        &mut ir,
        "demo/C",
        vec![x, y, z],
        Some(vec![Ty::Int; 3]),
        &[],
        0,
        Some(1),
    );

    realize(&mut ir, &mut CheckedBackendCallables::default());

    let constructors = &ir.classes[class as usize].secondary_ctors;
    assert_eq!(constructors.len(), 2);
    assert_eq!(constructors[0].params, constructors[1].params);
    assert_eq!(ir.construction_targets[&defaulted], target(2));
    assert_eq!(
        crate::backend::selected_constructor(&ir, defaulted, class, Some(&[Ty::Int; 3])),
        Some(2)
    );
    assert_eq!(
        crate::backend::selected_constructor(&ir, declared, class, Some(&[Ty::Int; 3])),
        Some(1)
    );
    assert_eq!(
        construction_shape(&ir, declared),
        ("1, 2, 3".to_owned(), Some(vec![Ty::Int; 3]), Vec::new())
    );
}

#[test]
fn an_entry_takes_the_prefix_first_and_counts_masks_after_it() {
    let mut ir = IrFile::default();
    let outer = Ty::obj("demo/Outer");
    let class = class_with_primary(
        &mut ir,
        "demo/Outer$Inner",
        vec![outer],
        Vec::new(),
        Vec::new(),
    );
    let text = ir.add_expr(IrExpr::Const(IrConst::String("x".into())));
    // `inner class Inner { constructor(s: String = "x") : this() }` with the outer instance first.
    ir.classes[class as usize].secondary_ctors.push(secondary(
        vec![outer],
        vec![Ty::String],
        vec![Some(text)],
        CtorDelegateTarget::This {
            target_params: Vec::new(),
            target: target(0),
            default_masks: Vec::new(),
        },
        Vec::new(),
        Vec::new(),
    ));
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let site = construction(
        &mut ir,
        "demo/Outer$Inner",
        vec![receiver],
        Some(vec![Ty::String]),
        &[0],
        1,
        Some(1),
    );

    realize(&mut ir, &mut CheckedBackendCallables::default());

    let entry = &ir.classes[class as usize].secondary_ctors[1];
    assert_eq!(entry.prefix_params, vec![outer]);
    assert_eq!(entry.params, vec![Ty::String, Ty::Int]);
    assert_eq!(
        render_all(&ir, &entry.delegate_prelude),
        fill(3, 1, 2, "\"x\"")
    );
    assert_eq!(render_all(&ir, &entry.delegate_args), "$2");
    assert_eq!(delegation_of(entry), (1, vec![Ty::String]));
    assert_eq!(
        construction_shape(&ir, site),
        (
            "$0, absent String, 1".to_owned(),
            Some(vec![Ty::String, Ty::Int]),
            Vec::new()
        )
    );
    assert_eq!(ir.construction_targets[&site], target(2));
}

#[test]
fn a_superclass_call_drops_its_placeholders_for_the_entry() {
    let mut ir = IrFile::default();
    let seven = int(&mut ir, 7);
    let parent = class_with_primary(
        &mut ir,
        "demo/Base",
        Vec::new(),
        vec![Ty::Int, Ty::String],
        vec![Some(seven), None],
    );
    let mut child = crate::ir::test_support::blank_class("demo/Child");
    child.superclass = type_name("demo/Base");
    let placeholder = int(&mut ir, 0);
    let text = ir.add_expr(IrExpr::Const(IrConst::String("s".into())));
    child.super_args = vec![placeholder, text];
    child.super_ctor_params = vec![Ty::Int, Ty::String];
    let child = ir.add_class(child);
    ir.super_constructor_default_arguments
        .insert(type_name("demo/Child"), vec![0]);

    realize(&mut ir, &mut CheckedBackendCallables::default());

    let declaration = &ir.classes[child as usize];
    assert_eq!(
        render_all(&ir, &declaration.super_args),
        "absent Int, \"s\", 1"
    );
    assert_eq!(
        declaration.super_ctor_params,
        vec![Ty::Int, Ty::String, Ty::Int]
    );
    assert_eq!(declaration.super_ctor, target(1));
    assert!(ir.super_constructor_default_arguments.is_empty());
    let entry = &ir.classes[parent as usize].secondary_ctors[0];
    assert_eq!(render_all(&ir, &entry.delegate_prelude), fill(3, 1, 1, "7"));
}

#[test]
fn a_delegation_and_an_enum_constant_share_one_entry() {
    let mut ir = IrFile::default();
    let text = ir.add_expr(IrExpr::Const(IrConst::String("x".into())));
    let class = class_with_primary(
        &mut ir,
        "demo/E",
        Vec::new(),
        vec![Ty::Int, Ty::String],
        vec![None, Some(text)],
    );
    // `constructor(c: Int) : this(c)`, and the constant `A(1)`.
    let read = ir.add_expr(IrExpr::GetValue(1));
    let placeholder = ir.add_expr(IrExpr::Const(IrConst::Null));
    ir.classes[class as usize].secondary_ctors.push(secondary(
        Vec::new(),
        vec![Ty::Int],
        vec![None],
        CtorDelegateTarget::This {
            target_params: vec![Ty::Int, Ty::String],
            target: target(0),
            default_masks: Vec::new(),
        },
        vec![read, placeholder],
        vec![1],
    ));
    let one = int(&mut ir, 1);
    ir.classes[class as usize].is_enum = true;
    ir.classes[class as usize].enum_entries.push(IrEnumEntry {
        name: "A".to_owned(),
        diagnostic_span: None,
        argument_prelude: Vec::new(),
        args: vec![one],
        constructor_parameter_types: vec![Ty::Int, Ty::String],
        constructor: target(0),
        default_parameters: vec![1],
        decl_line: 0,
        source_order: 0,
        subclass: None,
    });

    realize(&mut ir, &mut CheckedBackendCallables::default());

    let declaration = &ir.classes[class as usize];
    assert_eq!(declaration.secondary_ctors.len(), 2);
    let entry_params = vec![Ty::Int, Ty::String, Ty::Int];
    assert_eq!(declaration.secondary_ctors[1].params, entry_params);
    let delegating = &declaration.secondary_ctors[0];
    assert_eq!(delegation_of(delegating), (2, entry_params.clone()));
    assert_eq!(
        render_all(&ir, &delegating.delegate_args),
        "$1, absent String, 2"
    );
    assert!(delegating.default_parameters.is_empty());
    let constant = &declaration.enum_entries[0];
    assert_eq!(render_all(&ir, &constant.args), "1, absent String, 2");
    assert_eq!(constant.constructor, target(2));
    assert_eq!(constant.constructor_parameter_types, entry_params);
    assert!(constant.default_parameters.is_empty());
}

#[test]
fn constructor_calls_this_lowering_cannot_realize_keep_their_checked_form() {
    let mut ir = IrFile::default();
    let seven = int(&mut ir, 7);
    class_with_primary(
        &mut ir,
        "demo/C",
        Vec::new(),
        vec![Ty::Int],
        vec![Some(seven)],
    );
    // Another file's class, and a construction with no recorded selection.
    let elsewhere = construction(
        &mut ir,
        "demo/Elsewhere",
        Vec::new(),
        None,
        &[0],
        0,
        Some(0),
    );
    let unselected = construction(&mut ir, "demo/C", Vec::new(), None, &[0], 0, None);
    // A parameter without a default.
    let bare = class_with_primary(&mut ir, "demo/Bare", Vec::new(), vec![Ty::Int], vec![None]);
    let omitting = construction(&mut ir, "demo/Bare", Vec::new(), None, &[0], 0, Some(0));
    // A `super(…)` to a dependency's constructor.
    let mut child = crate::ir::test_support::blank_class("demo/Child");
    child.superclass = type_name("demo/C");
    let placeholder = int(&mut ir, 0);
    child.super_args = vec![placeholder];
    child.super_ctor_params = vec![Ty::Int];
    ir.add_class(child);
    ir.super_constructor_default_arguments
        .insert(type_name("demo/Child"), vec![0]);
    ir.external_super_constructors.insert(
        type_name("demo/Child"),
        crate::ir::IrExternalConstructorTarget::unresolved(
            crate::fir::ExternalCallableId::from_raw(1),
        ),
    );

    realize(&mut ir, &mut CheckedBackendCallables::default());

    for site in [elsewhere, unselected, omitting] {
        assert_eq!(construction_shape(&ir, site).2, vec![0]);
    }
    assert!(ir
        .classes
        .iter()
        .all(|class| class.secondary_ctors.is_empty()));
    assert_eq!(ir.classes[bare as usize].secondary_ctors.len(), 0);
    assert_eq!(
        ir.super_constructor_default_arguments[&type_name("demo/Child")],
        vec![0]
    );
}

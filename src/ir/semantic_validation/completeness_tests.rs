use super::IncompleteIrFact;
use crate::fir::{PropertyId, SourceFileId};
use crate::ir::{
    IrCheckedOperation, IrExpr, IrFile, IrFunction, IrLocalPropertyLayout, IrModuleProperty,
    IrModuleSource,
};
use crate::types::{type_name, Ty};

const HERE: SourceFileId = SourceFileId::from_raw(0);
const ELSEWHERE: SourceFileId = SourceFileId::from_raw(1);

fn read(ir: &mut IrFile, target: PropertyId) {
    ir.add_expr(IrExpr::Checked(IrCheckedOperation::PropertyRead {
        target,
        dispatch_receiver: None,
        extension_receiver: None,
        context_arguments: Vec::new(),
        substitutions: Vec::new(),
    }));
}

fn module_property(source: SourceFileId) -> IrModuleProperty {
    IrModuleProperty {
        source: IrModuleSource {
            source,
            package: type_name(""),
        },
        name: "count".to_owned(),
        ty: Ty::Int,
        context_parameters: Vec::new(),
        extension_receiver: None,
        mutable: false,
        owner: None,
        owner_kind: None,
        companion_associated: false,
        companion_owner: None,
        visibility: crate::types::Visibility::Public,
        setter_visibility: crate::types::Visibility::Public,
        setter_parameter: None,
        annotations: Box::new([]),
        flags: Default::default(),
        placement: crate::ir::IrStaticPlacement::Package,
        compile_time_constant: None,
        overridden_types: Box::new([]),
    }
}

fn storage_layout() -> IrLocalPropertyLayout {
    IrLocalPropertyLayout::TopLevelStorage {
        storage: 0,
        getter: None,
        setter: None,
        qualifier: None,
    }
}

#[test]
fn a_property_this_file_declares_without_a_layout_is_rejected() {
    let property = PropertyId::from_raw(4);
    let mut ir = IrFile::default();
    ir.referenced_module_properties
        .insert(property, module_property(HERE));
    read(&mut ir, property);
    assert_eq!(
        ir.validate_complete_facts(HERE),
        Err(IncompleteIrFact::PropertyLayout(property))
    );
    ir.local_property_layouts.insert(property, storage_layout());
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

#[test]
fn a_property_another_file_declares_needs_no_layout_here() {
    let property = PropertyId::from_raw(4);
    let mut ir = IrFile::default();
    ir.referenced_module_properties
        .insert(property, module_property(ELSEWHERE));
    read(&mut ir, property);
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

#[test]
fn a_read_of_a_property_with_neither_a_layout_nor_a_module_fact_is_rejected() {
    let property = PropertyId::from_raw(9);
    let mut ir = IrFile::default();
    read(&mut ir, property);
    assert_eq!(
        ir.validate_complete_facts(HERE),
        Err(IncompleteIrFact::PropertyLayout(property))
    );
}

/// A function whose body reads value slot 1 as a shared capture holder: its second parameter
/// when it is static, its first when slot 0 is its dispatch receiver.
fn holder_reader(ir: &mut IrFile, dispatch_receiver: Option<Ty>) -> u32 {
    let holder = ir.add_expr(IrExpr::GetValue(1));
    let body = ir.add_expr(IrExpr::RefGet {
        holder,
        elem: Ty::Int,
    });
    ir.add_fun(IrFunction {
        name: "reader".to_owned(),
        params: vec![Ty::String, Ty::Int],
        ret: Ty::Int,
        body: Some(body),
        is_static: dispatch_receiver.is_none(),
        dispatch_receiver: dispatch_receiver.and_then(Ty::obj_internal),
        param_checks: vec![None, None],
    })
}

#[test]
fn a_holder_parameter_that_is_not_recorded_is_rejected() {
    let mut ir = IrFile::default();
    let function = holder_reader(&mut ir, None);
    assert_eq!(
        ir.validate_complete_facts(HERE),
        Err(IncompleteIrFact::SharedCaptureParameter {
            function,
            parameter: 1,
        })
    );
    ir.shared_capture_parameters.insert((function, 1), Ty::Int);
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

#[test]
fn a_dispatch_receiver_shifts_the_holder_parameter_by_one_slot() {
    let mut ir = IrFile::default();
    let function = holder_reader(&mut ir, Some(Ty::obj("Host")));
    assert_eq!(
        ir.validate_complete_facts(HERE),
        Err(IncompleteIrFact::SharedCaptureParameter {
            function,
            parameter: 0,
        })
    );
    ir.shared_capture_parameters.insert((function, 0), Ty::Int);
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

#[test]
fn a_lambda_inline_body_reads_the_lambda_s_own_slots() {
    let mut ir = IrFile::default();
    // The lambda's inline body reads slot 0 as a holder: the lambda's own first parameter, not the
    // enclosing function's, which holds no holder.
    let holder = ir.add_expr(IrExpr::GetValue(0));
    let inline_body = ir.add_expr(IrExpr::RefGet {
        holder,
        elem: Ty::Int,
    });
    let lambda_body = ir.add_expr(IrExpr::UnitInstance);
    let lambda = ir.add_fun(IrFunction {
        name: "lambda".to_owned(),
        params: vec![Ty::Int],
        ret: Ty::Int,
        body: Some(lambda_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None],
    });
    ir.shared_capture_parameters.insert((lambda, 0), Ty::Int);
    let body = ir.add_expr(IrExpr::Lambda {
        impl_fn: lambda,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(inline_body),
    });
    ir.add_fun(IrFunction {
        name: "enclosing".to_owned(),
        params: vec![Ty::String],
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None],
    });
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

/// A lambda that takes one capture and never reads it: its body only yields `Unit`.
fn forwarding_lambda(ir: &mut IrFile, body: Option<u32>) -> u32 {
    let body = body.unwrap_or_else(|| ir.add_expr(IrExpr::UnitInstance));
    ir.add_fun(IrFunction {
        name: "lambda".to_owned(),
        params: vec![Ty::Int],
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None],
    })
}

fn capture(ir: &mut IrFile, lambda: u32, slot: u32) -> u32 {
    let captured = ir.add_expr(IrExpr::GetValue(slot));
    ir.add_expr(IrExpr::Lambda {
        impl_fn: lambda,
        arity: 0,
        captures: vec![captured],
        sam: None,
        inline_body: None,
    })
}

#[test]
fn a_lambda_that_captures_a_declared_holder_must_record_it_without_reading_it() {
    let mut ir = IrFile::default();
    let lambda = forwarding_lambda(&mut ir, None);
    // `var count = 1` captured by the lambda: slot 0 is declared with a new holder.
    let initial = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
    let holder = ir.add_expr(IrExpr::RefNew {
        elem: Ty::Int,
        init: Some(initial),
    });
    let declaration = ir.add_expr(IrExpr::Variable {
        index: 0,
        ty: Ty::Int,
        init: Some(holder),
        named: true,
    });
    let creation = capture(&mut ir, lambda, 0);
    // A plain local in slot 1, captured by a second lambda, obliges nothing.
    let plain = ir.add_expr(IrExpr::Variable {
        index: 1,
        ty: Ty::Int,
        init: Some(initial),
        named: true,
    });
    let other = forwarding_lambda(&mut ir, None);
    let plain_creation = capture(&mut ir, other, 1);
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![declaration, creation, plain, plain_creation],
        value: None,
    });
    ir.add_fun(IrFunction {
        name: "outer".to_owned(),
        params: Vec::new(),
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    assert_eq!(
        ir.validate_complete_facts(HERE),
        Err(IncompleteIrFact::SharedCaptureParameter {
            function: lambda,
            parameter: 0,
        })
    );
    ir.shared_capture_parameters.insert((lambda, 0), Ty::Int);
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

#[test]
fn a_recorded_holder_parameter_passed_to_a_nested_lambda_obliges_its_parameter() {
    let mut ir = IrFile::default();
    let inner = forwarding_lambda(&mut ir, None);
    // The outer lambda's recorded holder parameter 0 is its slot 0; it only hands it on.
    let hand_on = capture(&mut ir, inner, 0);
    let outer = forwarding_lambda(&mut ir, Some(hand_on));
    ir.shared_capture_parameters.insert((outer, 0), Ty::Int);
    assert_eq!(
        ir.validate_complete_facts(HERE),
        Err(IncompleteIrFact::SharedCaptureParameter {
            function: inner,
            parameter: 0,
        })
    );
    ir.shared_capture_parameters.insert((inner, 0), Ty::Int);
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

/// `var count` in slot 0, then `body` in the same static function.
fn function_holding(ir: &mut IrFile, name: &str, body: u32) -> u32 {
    let initial = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
    let holder = ir.add_expr(IrExpr::RefNew {
        elem: Ty::Int,
        init: Some(initial),
    });
    let declaration = ir.add_expr(IrExpr::Variable {
        index: 0,
        ty: Ty::Int,
        init: Some(holder),
        named: true,
    });
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![declaration, body],
        value: None,
    });
    ir.add_fun(IrFunction {
        name: name.to_owned(),
        params: Vec::new(),
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    })
}

#[test]
fn a_local_call_that_forwards_a_holder_obliges_the_callee_parameter() {
    let mut ir = IrFile::default();
    let make = forwarding_lambda(&mut ir, None);
    let captured = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: crate::ir::Callee::Local(make),
        dispatch_receiver: None,
        args: vec![captured],
    });
    function_holding(&mut ir, "outer", call);
    assert_eq!(
        ir.validate_complete_facts(HERE),
        Err(IncompleteIrFact::SharedCaptureParameter {
            function: make,
            parameter: 0,
        })
    );
    ir.shared_capture_parameters.insert((make, 0), Ty::Int);
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

#[test]
fn a_callable_reference_capture_obliges_the_adapter_parameter() {
    let mut ir = IrFile::default();
    let adapter = forwarding_lambda(&mut ir, None);
    let captured = ir.add_expr(IrExpr::GetValue(0));
    let reference = ir.add_expr(IrExpr::CallableReference(crate::ir::IrCallableReference {
        target: crate::ir::IrCallableReferenceTarget::FunctionValueConversion { ordinal: 0 },
        adapter,
        captures: vec![captured],
        bound_receiver: None,
        function_type: Ty::fun(Vec::new(), Ty::Unit),
        declaration_parameters: Box::new([]),
        declaration_result: Ty::Unit,
        declaration_suspend: false,
        adaptation: None,
        reflection_owner: None,
    }));
    function_holding(&mut ir, "outer", reference);
    assert_eq!(
        ir.validate_complete_facts(HERE),
        Err(IncompleteIrFact::SharedCaptureParameter {
            function: adapter,
            parameter: 0,
        })
    );
    ir.shared_capture_parameters.insert((adapter, 0), Ty::Int);
    assert_eq!(ir.validate_complete_facts(HERE), Ok(()));
}

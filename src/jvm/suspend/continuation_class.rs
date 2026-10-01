//! The continuation class of a named suspend function's state machine: a `ContinuationImpl` with
//! the `result` and `label` every machine keeps, its spill fields, and `invokeSuspend`, which
//! re-enters the function with the continuation.

use super::spill_layout::SpillLayout;
use super::value_class_results::unboxed_carrier;
use super::{continuation_ty, int_ty, object_ty, zero_value, CONTINUATION_IMPL, I32_MIN};
use super::{SuspendedResultReturn, SuspendedResultReturns};
use crate::ir::{
    Callee, ClassId, ExprId, IrBinOp, IrClass, IrConst, IrCtorArg, IrExpr, IrFile, IrFunction,
    IrTypeOp,
};
use crate::types::{type_name, Ty, TypeName};

pub(super) fn build_continuation_class(
    ir: &mut IrFile,
    internal: &str,
    outer_fid: u32,
    layout: &SpillLayout,
    suspended_result_returns: &mut SuspendedResultReturns,
    receiver: Option<TypeName>,
    params: &[Ty],
) -> ClassId {
    let class_id = ir.classes.len() as ClassId;
    let layout_fields = layout.fields();
    // result(0), label(1), spill slots(2..), and — for a member — the captured receiver `this$0` last.
    let recv_field_idx = 2 + layout_fields.len() as u32;

    // invokeSuspend(Object result): this.result = result; this.label |= MIN_VALUE; re-enter the outer
    // function. For a top-level fn that's `outer(this)`; for a member it's `this.this$0.m(this)`.
    let this0 = ir.add_expr(IrExpr::GetValue(0));
    let arg1 = ir.add_expr(IrExpr::GetValue(1));
    let set_result = ir.add_expr(IrExpr::SetField {
        receiver: this0,
        class: class_id,
        index: 0,
        value: arg1,
    });
    let this_lbl_recv = ir.add_expr(IrExpr::GetValue(0));
    let old_lbl = ir.add_expr(IrExpr::GetField {
        receiver: this_lbl_recv,
        class: class_id,
        index: 1,
    });
    let min = ir.add_expr(IrExpr::Const(IrConst::Int(I32_MIN)));
    let or_lbl = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::BitOr,
        lhs: old_lbl,
        rhs: min,
    });
    let this_set_lbl = ir.add_expr(IrExpr::GetValue(0));
    let set_label = ir.add_expr(IrExpr::SetField {
        receiver: this_set_lbl,
        class: class_id,
        index: 1,
        value: or_lbl,
    });
    let this_call = ir.add_expr(IrExpr::GetValue(0));
    let this_as_cont = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: this_call,
        type_operand: continuation_ty(),
    });
    // The outer fn now takes its real value parameters before the continuation. On re-entry the values
    // are irrelevant (the loop-top restore overwrites them from the captured fields), so pass type-
    // correct placeholders, exactly as kotlinc passes `iconst_0`/`aconst_null`.
    let mut reentry_args: Vec<ExprId> = params.iter().map(|t| zero_value(ir, t)).collect();
    reentry_args.push(this_as_cont);
    let call_outer = match receiver {
        None => ir.add_expr(IrExpr::Call {
            callee: Callee::Local(outer_fid),
            dispatch_receiver: None,
            args: reentry_args,
        }),
        Some(owner) => {
            // `((C)this.this$0).m(<params…>, (Continuation)this)` — invokevirtual the member on the receiver.
            let cont_this = ir.add_expr(IrExpr::GetValue(0));
            let recv = ir.add_expr(IrExpr::GetField {
                receiver: cont_this,
                class: class_id,
                index: recv_field_idx,
            });
            let name = ir.functions[outer_fid as usize].name.clone();
            // Build the member's CPS descriptor: its value params, then the trailing `Continuation`.
            let mut p_jvm: Vec<crate::types::Ty> = params
                .iter()
                .map(crate::jvm::physical_type::ir_ty_to_jvm)
                .collect();
            p_jvm.push(crate::jvm::physical_type::ir_ty_to_jvm(&continuation_ty()));
            let descriptor = crate::jvm::names::method_descriptor(
                &p_jvm,
                crate::jvm::physical_type::ir_ty_to_jvm(&object_ty()),
            );
            let owner_cid = ir.classes.iter().position(|c| c.fq_name == owner);
            let owner_midx = owner_cid
                .and_then(|cid| ir.classes[cid].methods.iter().position(|&m| m == outer_fid));
            if crate::jvm::suspend_impls::moves_to_suspend_impl(ir, outer_fid) {
                // The machine lives in the member's `$suspendImpl`: re-enter the static with the
                // captured receiver first, never the member, which an override may replace.
                let mut args = vec![recv];
                args.extend(reentry_args);
                ir.add_expr(IrExpr::Call {
                    callee: Callee::ClassStatic {
                        owner,
                        function: outer_fid,
                    },
                    dispatch_receiver: None,
                    args,
                })
            } else if let (true, Some(cid), Some(midx)) = (
                ir.method_visibility(outer_fid).is_private(),
                owner_cid,
                owner_midx,
            ) {
                // A PRIVATE member is called as itself; the continuation class is another class,
                // so the call goes through the owner's `access$<name>` bridge like any other.
                ir.add_expr(IrExpr::MethodCall {
                    class: cid as u32,
                    index: midx as u32,
                    receiver: recv,
                    args: reentry_args.into_iter().map(Some).collect(),
                })
            } else {
                // A suspend DEFAULT method lives on an interface: the re-entry call must be an
                // `invokeinterface` — an `invokevirtual` on an interface methodref fails linkage
                // with `IncompatibleClassChangeError` (coroutines/suspendDefaultImpl).
                let interface = owner_cid.is_some_and(|cid| ir.classes[cid].is_interface);
                ir.add_expr(IrExpr::Call {
                    callee: Callee::realized_virtual(owner, name, descriptor, None, interface),
                    dispatch_receiver: Some(recv),
                    args: reentry_args,
                })
            }
        }
    };
    let ret = ir.add_expr(IrExpr::Return(Some(call_outer)));
    // The function returns a value class's carrier where kotlinc's value-class ABI says so; its
    // completion takes the value as `Any?`, so the carrier goes on as its box.
    if let Some((classifier, carrier)) =
        unboxed_carrier(ir.value_class_suspend_returns.get(&outer_fid).copied())
    {
        suspended_result_returns.insert(
            ret,
            SuspendedResultReturn::ValueClassBox {
                classifier,
                carrier,
            },
        );
    }
    let inv_body = ir.add_expr(IrExpr::Block {
        stmts: vec![set_result, set_label, ret],
        value: None,
    });
    let inv_fid = ir.add_fun(IrFunction {
        name: "invokeSuspend".to_string(),
        params: vec![Ty::obj("kotlin/Any")],
        ret: object_ty(),
        body: Some(inv_body),
        is_static: false,
        dispatch_receiver: Some(type_name(internal)),
        param_checks: vec![None],
    });

    // State-machine fields: `result`/`label`/`L$i` are mutable and non-private (read/written
    // cross-class by the resume machinery).
    let mut fields = vec![
        crate::ir::IrField::new("result".to_string(), object_ty()).with_is_private(false),
        crate::ir::IrField::new("label".to_string(), int_ty()).with_is_private(false),
    ];
    for (name, field_ty) in &layout_fields {
        fields.push(crate::ir::IrField::new(name.clone(), *field_ty).with_is_private(false));
    }

    // Constructor value-indices: `this`=0, then (member) the receiver, then each captured value
    // parameter, then the completion `Continuation`. Store the receiver to `this$0` and each captured
    // param to its `L$i` field, then `super(completion)`. A top-level fn with no live params is just
    // `<init>(Continuation)`.
    let mut ctor_args: Vec<IrCtorArg> = Vec::new();
    let mut pre_super_param_fields = Vec::new();
    let mut arg_idx = 1u32; // value-index of the next ctor argument (`this` is 0)
    if let Some(owner) = receiver {
        let recv_ty = Ty::obj_name(owner);
        let receiver_field = fields.len() as u32;
        fields.push(
            crate::ir::IrField::new("this$0".to_string(), recv_ty)
                .with_is_final(true)
                .with_is_private(false),
        );
        // The continuation ABI stores its member receiver before `ContinuationImpl.<init>`, but the
        // receiver field follows result/label/spills and is not a primary-constructor property. Carry
        // the exact parameter/field edge so emission needs neither a field-name rule nor a leading-field
        // assumption. This is independent of language-level inner/static nesting.
        pre_super_param_fields.push((0, receiver_field));
        ctor_args.push(IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty: recv_ty,
            declared_ty: None,
            is_field: false,
            field_index: None,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: crate::ir::IrCtorParameterProvenance::Value,
            capture_identity: None,
        });
        arg_idx += 1;
    }
    ctor_args.push(IrCtorArg {
        name: None,
        context_kind: crate::types::ContextParameterKind::None,
        ty: continuation_ty(),
        declared_ty: None,
        is_field: false,
        field_index: None,
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture: None,
        provenance: crate::ir::IrCtorParameterProvenance::Value,
        capture_identity: None,
    });
    let super_completion_idx = arg_idx;

    let super_arg = ir.add_expr(IrExpr::GetValue(super_completion_idx));
    let class = IrClass {
        fq_name: crate::types::type_name(internal),
        is_source_declared: false,
        is_anonymous_object: false,
        enclosure: None,
        is_inner_class: false,
        is_local_class: false,
        is_value: false,
        is_data: false,
        decl_line: 0,
        decl_start_line: 0,
        decl_end_line: 0,
        type_param_bounds: vec![],
        type_params: Vec::new(),
        captured_type_params: Vec::new(),
        supertypes: vec![],
        properties: Vec::new(),
        fields,
        ctor_param_count: 0,
        constructor_prefix_count: 0,
        ctor_args,
        ctor_param_annotations: Vec::new(),
        init_body: None,
        pre_super_param_fields,
        explicit_param_stores: false,
        methods: vec![inv_fid],
        is_interface: false,
        is_fun_interface: false,
        is_annotation: false,
        annotation_impl_of: None,
        is_sealed: false,
        sealed_subclasses: Default::default(),
        is_abstract: false,
        is_open: false,
        superclass: crate::types::type_name(CONTINUATION_IMPL),
        super_arg_prelude: Vec::new(),
        super_args: vec![super_arg],
        // The generated class delegates to `ContinuationImpl(Continuation)`. This field is now the
        // backend's exact selected-constructor contract, so it must describe the target parameter,
        // not the continuation class's `label: Int` storage field.
        super_ctor_params: vec![continuation_ty()],
        super_ctor: crate::ir::IrConstructorTarget::UNRESTRICTED_PRIMARY,
        is_enum: false,
        enum_entries: vec![],
        enum_entry_of: None,
        prop_ref: None,
        func_ref: None,
        bridges: vec![],
        interfaces: Default::default(),
        is_object: false,
        is_companion: false,
        companion_class: None,
        published_nested_classifiers: Vec::new(),
        secondary_ctors: vec![],
        has_primary_ctor: true,
        applied_annotations: crate::ir::DeclarationAnnotations::default(),
        primary_ctor_annotations: crate::ir::DeclarationAnnotations::default(),
        field_annotations: Vec::new(),
        property_annotations: Vec::new(),
        annotation_retention: None,
    };
    ir.add_class(class)
}

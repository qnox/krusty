//! The continuation class of a named suspend function's state machine: a `ContinuationImpl` with
//! the `result` and `label` every machine keeps, its spill fields, and `invokeSuspend`, which
//! re-enters the function with the continuation.

use super::spill_layout::SpillLayout;
use super::{
    add_static_call, continuation_ty, int_ty, object_ty, zero_value, CONTINUATION_IMPL, I32_MIN,
};
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
    _param_caps: &[(u32, Ty)],
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
            let owner_internal = owner.render();
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
                .map(crate::jvm::ir_emit::ir_ty_to_jvm)
                .collect();
            p_jvm.push(crate::jvm::ir_emit::ir_ty_to_jvm(&continuation_ty()));
            let descriptor = crate::jvm::names::method_descriptor(
                &p_jvm,
                crate::jvm::ir_emit::ir_ty_to_jvm(&object_ty()),
            );
            // A PRIVATE member can't be invoked from the continuation class (a separate class,
            // pre-nestmates). kotlinc emits a `PUBLIC|STATIC|FINAL|SYNTHETIC access$<name>` bridge on the
            // owner that `invokespecial`s the private member; the continuation calls the bridge.
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
                ir.private_methods.contains(&outer_fid),
                owner_cid,
                owner_midx,
            ) {
                let access_name = format!("access${name}");
                // Bridge body (static frame): 0 = the owner receiver, 1..=n the value params,
                // n+1 the continuation — `return receiver.<private m>(args…, cont)` (the private
                // `MethodCall` emits as `invokespecial`).
                let recv0 = ir.add_expr(IrExpr::GetValue(0));
                let margs: Vec<Option<ExprId>> = (1..=params.len() + 1)
                    .map(|i| Some(ir.add_expr(IrExpr::GetValue(i as u32))))
                    .collect();
                let call = ir.add_expr(IrExpr::MethodCall {
                    class: cid as u32,
                    index: midx as u32,
                    receiver: recv0,
                    args: margs,
                });
                let aret = ir.add_expr(IrExpr::Return(Some(call)));
                let abody = ir.add_expr(IrExpr::Block {
                    stmts: vec![aret],
                    value: None,
                });
                let mut aparams = vec![Ty::obj_name(owner)];
                aparams.extend(params.iter().copied());
                aparams.push(continuation_ty());
                let afid = ir.add_fun(IrFunction {
                    name: access_name.clone(),
                    params: aparams.clone(),
                    ret: object_ty(),
                    body: Some(abody),
                    is_static: true,
                    dispatch_receiver: Some(owner),
                    param_checks: Vec::new(),
                });
                ir.classes[cid].methods.push(afid);
                ir.synthetic_methods.insert(afid); // kotlinc: 0x1019 PUBLIC|STATIC|FINAL|SYNTHETIC
                let a_jvm: Vec<crate::types::Ty> = aparams
                    .iter()
                    .map(crate::jvm::ir_emit::ir_ty_to_jvm)
                    .collect();
                let adesc = crate::jvm::names::method_descriptor(
                    &a_jvm,
                    crate::jvm::ir_emit::ir_ty_to_jvm(&object_ty()),
                );
                let mut aargs = vec![recv];
                aargs.extend(reentry_args);
                add_static_call(ir, &owner_internal, &access_name, &adesc, aargs)
            } else {
                // A suspend DEFAULT method lives on an interface: the re-entry call must be an
                // `invokeinterface` — an `invokevirtual` on an interface methodref fails linkage
                // with `IncompatibleClassChangeError` (coroutines/suspendDefaultImpl).
                let interface = owner_cid.is_some_and(|cid| ir.classes[cid].is_interface);
                ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner,
                        name,
                        descriptor,
                        params: None,
                        interface,
                    },
                    dispatch_receiver: Some(recv),
                    args: reentry_args,
                })
            }
        }
    };
    let ret = ir.add_expr(IrExpr::Return(Some(call_outer)));
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

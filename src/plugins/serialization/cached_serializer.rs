//! A serializer a declaration builds for itself once and caches: an enum's and an object's.
//!
//! kotlinc gives such a declaration a `private static final Lazy $cachedSerializer$delegate`,
//! initialized with `LazyKt.lazy(PUBLICATION) { … }` whose body is a private static
//! `_init_$_anonymous_()`. An enum's body is its `EnumSerializer` factory; an object's is an
//! `ObjectSerializer` over its own `INSTANCE`. Neither has a generated `$serializer` class.

use super::{class_ty, kserializer_of, serial_name};
use crate::ir::{Callee, ClassId, ExprId, IrConst, IrExpr, IrFile, IrFunction, IrTypeOp};
use crate::libraries::InlineKind;
use crate::types::{type_name, Ty, TypeName};

const OBJECT_SERIALIZER_FQ: &str = "kotlinx/serialization/internal/ObjectSerializer";
const CACHED_SERIALIZER_DELEGATE: &str = "$cachedSerializer$delegate";

/// `$cachedSerializer$delegate = LazyKt.lazy(PUBLICATION, ::_init_$_anonymous_)` on `class_id`,
/// where `_init_$_anonymous_()` returns `serializer`.
pub(super) fn add_cached_serializer_delegate(
    ir: &mut IrFile,
    class_id: ClassId,
    class_fq: &str,
    serializer: ExprId,
) {
    // The ANNOTATED start line (the `@Serializable` line), not the declaration keyword's — kotlinc
    // maps every generated member, and the delegate's `<clinit>` store, to where the declaration
    // begins, annotations included.
    let owner_line = ir.classes[class_id as usize].decl_start_line;
    // The delegate's DECLARED type carries its arguments so the field gets kotlinc's generic
    // `Signature` (`Lkotlin/Lazy<Lkotlinx/serialization/KSerializer<Ljava/lang/Object;>;>;`);
    // the descriptor still erases to `Lkotlin/Lazy;`.
    let lazy_ty = Ty::obj_args(
        "kotlin/Lazy",
        &[Ty::obj_args(
            "kotlinx/serialization/KSerializer",
            // `kotlin/Any`, not `java/lang/Object`: the checked-symbol validation only knows
            // Kotlin classifiers, and the signature formatter maps this to `Ljava/lang/Object;`.
            &[class_ty("kotlin/Any")],
        )],
    );
    // kotlinc does not build the delegate eagerly with `lazyOf`. It compiles the initializer to
    // a private static `_init_$_anonymous_()` holding the serializer construction, binds it
    // with an `invokedynamic` `Function0`, and passes that to
    // `LazyKt.lazy(LazyThreadSafetyMode.PUBLICATION, …)` — so the serializer is constructed on
    // first use, and the class carries a `BootstrapMethods` attribute.
    let anon_ret = ir.add_expr(IrExpr::Return(Some(serializer)));
    let anon_body = ir.add_expr(IrExpr::Block {
        stmts: vec![anon_ret],
        value: None,
    });
    let anonymous = ir.add_fun(IrFunction {
        name: "_init_$_anonymous_".to_string(),
        params: vec![],
        ret: Ty::obj("kotlinx/serialization/KSerializer"),
        body: Some(anon_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    // kotlinc emits every standalone lambda impl `private static final` — reachable only through
    // the same-class `invokedynamic`, never part of the public ABI.
    ir.private_methods.insert(anonymous);
    // Compiler-invented, like the cached-serializer helper: ACC_SYNTHETIC, and therefore absent
    // from `@Metadata`.
    ir.synthetic_methods.insert(anonymous);
    ir.classes[class_id as usize].methods.push(anonymous);
    // Generated in the BACKEND, past the frontend's declaration-line transfer, so the emitter
    // would attach no debug tables without a line of its own. kotlinc maps it to the annotated
    // owner's declaration line, which the class already carries by now.
    if owner_line != 0 {
        ir.fn_decl_lines.insert(anonymous, owner_line);
        ir.fn_sig_lines.insert(anonymous, owner_line);
    }
    let block = ir.add_expr(IrExpr::Lambda {
        impl_fn: anonymous,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: None,
    });
    let mode = ir.add_expr(IrExpr::EnumEntry {
        classifier: type_name("kotlin/LazyThreadSafetyMode"),
        name: "PUBLICATION".into(),
    });
    let lazy = ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: type_name("kotlin/LazyKt"),
            name: "lazy".to_string(),
            descriptor:
                "(Lkotlin/LazyThreadSafetyMode;Lkotlin/jvm/functions/Function0;)Lkotlin/Lazy;"
                    .to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![mode, block],
    });
    ir.statics.push(crate::ir::IrStatic {
        name: CACHED_SERIALIZER_DELEGATE.to_string(),
        ty: lazy_ty,
        init: lazy,
        is_var: false,
        is_const: false,
        owner: Some(type_name(class_fq)),
        visibility: crate::types::Visibility::Private,
        setter_jvm_name: None,
        erased_declared_ty: None,
        custom_accessor: true,
        line: owner_line,
        source_order: u32::MAX,
    });
}

/// `new ObjectSerializer(<serial name>, <object>.INSTANCE, new Annotation[0])`.
///
/// The annotation array is the object's `@SerialInfo` annotations; materializing retained
/// annotation values is a separate gap (the enum factory passes `null` for the same reason), so it
/// is empty here.
pub(super) fn object_serializer(ir: &mut IrFile, object: TypeName, name: ExprId) -> ExprId {
    let instance = ir.add_expr(IrExpr::ExternalStaticInstance {
        owner: object,
        ty: object,
        field: "INSTANCE".to_string(),
    });
    let annotations = ir.add_expr(IrExpr::Vararg {
        array_type: Ty::obj_args("kotlin/Array", &[class_ty("kotlin/Annotation")]),
        spreads: Vec::new(),
        elements: Vec::new(),
    });
    ir.new_external(
        OBJECT_SERIALIZER_FQ,
        "(Ljava/lang/String;Ljava/lang/Object;[Ljava/lang/annotation/Annotation;)V",
        vec![name, instance, annotations],
    )
}

/// The `@Serializable object` this file declares as `classifier`, when its serializer is the
/// generated `ObjectSerializer` (an object naming its own serializer with `with =` uses that one).
pub(super) fn local_serializable_object(
    ir: &IrFile,
    ctx: &crate::plugins::PluginContext,
    classifier: TypeName,
) -> Option<ClassId> {
    let class_id = ir
        .classes
        .iter()
        .position(|class| class.fq_name_id() == classifier && class.is_object)?
        as ClassId;
    (ctx.has_annotation(class_id, type_name(super::SERIALIZABLE_FQ))
        && super::custom_serializer_of(ctx, ir, class_id).is_none())
    .then_some(class_id)
}

/// A `@Serializable object`: `serializer()` is a member of the object returning the cached
/// `ObjectSerializer`, through a private `get$cachedSerializer()` that reads the delegate.
pub(super) fn add_object_serializer(ir: &mut IrFile, class_id: ClassId, class_fq: &str) {
    let object = type_name(class_fq);
    let owner_line = ir.classes[class_id as usize].decl_start_line;

    // `get$cachedSerializer()`: `(KSerializer) $cachedSerializer$delegate.getValue()`.
    let read = ir.external_static_field(class_fq, CACHED_SERIALIZER_DELEGATE, "Lkotlin/Lazy;");
    let get_value = ir.add_expr(IrExpr::Call {
        callee: Callee::Virtual {
            owner: type_name("kotlin/Lazy"),
            name: "getValue".to_string(),
            descriptor: String::new(),
            params: Some((vec![], class_ty("kotlin/Any"))),
            interface: true,
        },
        dispatch_receiver: Some(read),
        args: vec![],
    });
    let cast = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: get_value,
        type_operand: Ty::obj("kotlinx/serialization/KSerializer"),
    });
    let cached_ret = ir.add_expr(IrExpr::Return(Some(cast)));
    let cached_body = ir.add_expr(IrExpr::Block {
        stmts: vec![cached_ret],
        value: None,
    });
    // The helper's descriptor is the RAW `KSerializer` (no generic Signature); only the public
    // `serializer()` carries the type argument.
    let cached = ir.add_fun(IrFunction {
        name: "get$cachedSerializer".to_string(),
        params: vec![],
        ret: Ty::obj("kotlinx/serialization/KSerializer"),
        body: Some(cached_body),
        is_static: false,
        dispatch_receiver: Some(object),
        param_checks: Vec::new(),
    });
    // `private` makes the emitter mark it ACC_PRIVATE and reach it with `invokespecial`; that
    // dispatch reads a RESOLVED `IrExpr::MethodCall`, which is why `serializer()` calls it by index.
    ir.private_methods.insert(cached);
    ir.synthetic_methods.insert(cached);
    if owner_line != 0 {
        ir.fn_decl_lines.insert(cached, owner_line);
        ir.fn_sig_lines.insert(cached, owner_line);
    }

    // `serializer()` delegates to the helper. The frontend already declared it on the object;
    // kotlinc's generated members follow in this order: `serializer()`, the helper, then the
    // delegate's initializer.
    let placeholder = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let accessor = match super::complete_frontend_serializer_accessor(ir, object, placeholder) {
        Some(accessor) => accessor,
        None => {
            let accessor = ir.add_fun(IrFunction {
                name: "serializer".to_string(),
                params: vec![],
                ret: kserializer_of(class_ty(class_fq)),
                body: Some(placeholder),
                is_static: false,
                dispatch_receiver: Some(object),
                param_checks: Vec::new(),
            });
            ir.classes[class_id as usize].methods.push(accessor);
            accessor
        }
    };
    // Generated past the frontend's declaration-line transfer: kotlinc maps the accessor to the
    // annotated declaration's first line, like every other member it generates for the object.
    if owner_line != 0 {
        ir.fn_decl_lines.insert(accessor, owner_line);
        ir.fn_sig_lines.insert(accessor, owner_line);
    }
    // A plugin-generated member has no source position: kotlinc appends it after the object's own
    // declarations, in the class file and in `@Metadata` alike.
    ir.fn_source_order.remove(&accessor);
    let cached_index = ir.classes[class_id as usize].methods.len() as u32;
    ir.classes[class_id as usize].methods.push(cached);
    let this = ir.add_expr(IrExpr::GetValue(0));
    let delegate = ir.add_expr(IrExpr::MethodCall {
        class: class_id,
        index: cached_index,
        receiver: this,
        args: vec![],
    });
    let ret = ir.add_expr(IrExpr::Return(Some(delegate)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![ret],
        value: None,
    });
    ir.functions[accessor as usize].body = Some(body);

    let name = ir.add_expr(IrExpr::Const(IrConst::String(serial_name(ir, class_id))));
    let serializer = object_serializer(ir, object, name);
    // kotlinc narrows the initializer's value to the `KSerializer` it returns.
    let serializer = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: serializer,
        type_operand: class_ty(super::KSERIALIZER_FQ),
    });
    add_cached_serializer_delegate(ir, class_id, class_fq, serializer);
    // An object's delegate is generated storage with no declaration of its own; kotlinc emits it
    // `ACC_SYNTHETIC` (an enum's is an ordinary private field).
    let delegate = u32::try_from(ir.statics.len() - 1).expect("static index fits u32");
    ir.mark_synthetic_static(delegate);
}

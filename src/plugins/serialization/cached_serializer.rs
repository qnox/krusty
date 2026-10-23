//! A serializer a declaration builds for itself once and caches: an enum's, an object's, and an
//! abstract or sealed class's.
//!
//! kotlinc gives such a declaration a `private static final Lazy $cachedSerializer$delegate`,
//! initialized with `LazyKt.lazy(PUBLICATION) { … }` whose body is a private static
//! `_init_$_anonymous_()`. An enum's body is its `EnumSerializer` factory; an object's is an
//! `ObjectSerializer` over its own `INSTANCE`; an abstract class's is a `PolymorphicSerializer`
//! over its own class; a sealed class's is a `SealedClassSerializer` over its subclasses. None has
//! a generated `$serializer` class.

use super::class_ty;
use crate::ir::{Callee, ClassId, ExprId, FunId, IrConst, IrExpr, IrFile, IrFunction, IrTypeOp};
use crate::libraries::InlineKind;
use crate::types::{type_name, Ty, TypeName};

const OBJECT_SERIALIZER_FQ: &str = "kotlinx/serialization/internal/ObjectSerializer";
const CACHED_SERIALIZER_DELEGATE: &str = "$cachedSerializer$delegate";

/// `$cachedSerializer$delegate = LazyKt.lazy(PUBLICATION, ::_init_$_anonymous_)` on `class_id`,
/// where `_init_$_anonymous_()` returns `serializer`. Returns that initializer function.
pub(super) fn add_cached_serializer_delegate(
    ir: &mut IrFile,
    class_id: ClassId,
    class_name: TypeName,
    serializer: ExprId,
) -> FunId {
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
    ir.set_method_visibility(anonymous, crate::types::Visibility::Private);
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
    ir.note_synthesized_lambda(block);
    let mode = ir.add_expr(IrExpr::EnumEntry {
        classifier: type_name("kotlin/LazyThreadSafetyMode"),
        name: "PUBLICATION".into(),
    });
    let lazy = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Static {
            owner: type_name("kotlin/LazyKt"),
            name: "lazy".to_string(),
            descriptor:
                "(Lkotlin/LazyThreadSafetyMode;Lkotlin/jvm/functions/Function0;)Lkotlin/Lazy;"
                    .to_string(),
            inline: InlineKind::None,
        }),
        dispatch_receiver: None,
        args: vec![mode, block],
    });
    ir.statics.push(crate::ir::IrStatic {
        name: CACHED_SERIALIZER_DELEGATE.to_string(),
        ty: lazy_ty,
        init: Some(lazy),
        is_var: false,
        is_const: false,
        is_lateinit: false,
        owner: Some(class_name),
        visibility: crate::types::Visibility::Private,
        getter_jvm_name: None,
        setter_jvm_name: None,
        erased_declared_ty: None,
        accessors: crate::ir::IrStaticAccessors::ABSENT,
        line: owner_line,
        accessor_lines: crate::ir::IrAccessorLines::default(),
        source_order: u32::MAX,
        property_delegate: false,
    });
    anonymous
}

/// A serializer cached on `class_id` and served by its `Companion`, as kotlinc compiles an enum's
/// and an abstract class's: a `private static final Lazy $cachedSerializer$delegate` on the class
/// holding `serializer`, read through a `public static final Lazy
/// access$get$cachedSerializer$delegate$cp()` accessor, and a `Companion.serializer()` delegating
/// to a private `get$cachedSerializer()`. Returns the initializer and the accessor, which the
/// class itself declares.
pub(super) fn add_companion_cached_serializer(
    ir: &mut IrFile,
    class_id: ClassId,
    class_name: TypeName,
    serializer: ExprId,
) -> [FunId; 2] {
    // The ANNOTATED start line (the `@Serializable` line), not the declaration keyword's — kotlinc
    // maps every generated member, and the delegate's `<clinit>` store, to where the declaration
    // begins, annotations included.
    let owner_line = ir.classes[class_id as usize].decl_start_line;
    let initializer = add_cached_serializer_delegate(ir, class_id, class_name, serializer);
    // `public static final Lazy access$get$cachedSerializer$delegate$cp()` — reads the private field.
    let read =
        ir.external_static_field_name(class_name, CACHED_SERIALIZER_DELEGATE, "Lkotlin/Lazy;");
    let acc_ret = ir.add_expr(IrExpr::Return(Some(read)));
    let acc_body = ir.add_expr(IrExpr::Block {
        stmts: vec![acc_ret],
        value: None,
    });
    let acc = ir.add_fun(IrFunction {
        name: "access$get$cachedSerializer$delegate$cp".to_string(),
        params: vec![],
        // RAW `Lazy` on purpose: the FIELD carries the generic `Signature`, the synthetic
        // accessor does not — kotlinc gives this bridge neither a `Signature` nor nullability.
        ret: class_ty("kotlin/Lazy"),
        body: Some(acc_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.synthetic_methods.insert(acc);
    ir.classes[class_id as usize].methods.push(acc);
    if owner_line != 0 {
        ir.fn_decl_lines.insert(acc, owner_line);
        ir.fn_sig_lines.insert(acc, owner_line);
    }
    // `Companion.serializer()` returns `(KSerializer) access$…$cp().getValue()` (the cached instance).
    let acc_call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Static {
            owner: class_name,
            name: "access$get$cachedSerializer$delegate$cp".to_string(),
            descriptor: "()Lkotlin/Lazy;".to_string(),
            inline: InlineKind::None,
        }),
        dispatch_receiver: None,
        args: vec![],
    });
    let get_value = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::realized_virtual(
            type_name("kotlin/Lazy"),
            "getValue".to_string(),
            String::new(),
            Some((vec![], class_ty("kotlin/Any"))),
            true,
        )),
        dispatch_receiver: Some(acc_call),
        args: vec![],
    });
    let cast = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: get_value,
        type_operand: Ty::obj("kotlinx/serialization/KSerializer"),
    });
    let sret = ir.add_expr(IrExpr::Return(Some(cast)));
    let cached_body = ir.add_expr(IrExpr::Block {
        stmts: vec![sret],
        value: None,
    });
    // kotlinc does NOT inline the cached lookup into `serializer()`: it puts that body in a
    // private `get$cachedSerializer()` on the companion and has `serializer()` delegate to it.
    // The helper's descriptor is the RAW `KSerializer` (no generic Signature); only the public
    // `serializer()` carries the type argument.
    let companion = super::serializer_accessor_owner(ir, class_id);
    let cached = ir.add_fun(IrFunction {
        name: "get$cachedSerializer".to_string(),
        params: vec![],
        ret: Ty::obj("kotlinx/serialization/KSerializer"),
        body: Some(cached_body),
        is_static: false,
        dispatch_receiver: Some(companion),
        param_checks: Vec::new(),
    });
    // `private` makes the emitter mark it ACC_PRIVATE and reach it with `invokespecial` — which
    // is what the delegating call has to become. That dispatch decision reads a RESOLVED
    // `IrExpr::MethodCall` (class + member index), so the delegation below cannot be a
    // by-name `Callee::Virtual`: that form emits `invokevirtual` and never consults the set.
    ir.set_method_visibility(cached, crate::types::Visibility::Private);
    // kotlinc marks the helper ACC_SYNTHETIC, which also keeps it OUT of `@Metadata`: it is a
    // compiler-invented member, not a declaration the reflection layer should see.
    ir.synthetic_methods.insert(cached);
    // The frontend declared `serializer()` on the companion ahead of this helper — kotlinc's
    // member order is `<init>`, `serializer()`, `get$cachedSerializer()`, then the synthetic
    // marker ctor the emitter appends. Its body is completed once the helper's member index
    // is known.
    let comp_id = ir
        .class_id_by_name(companion)
        .expect("a companion-served serializer's companion is a class of this file")
        as usize;
    let cached_index = ir.classes[comp_id].methods.len() as u32;
    ir.classes[comp_id].methods.push(cached);
    // The helper exists only here, in the backend, so the frontend's generated-declaration line
    // transfer never sees it — and the emitter attaches neither debug table without a line.
    // The companion itself already carries the annotated owner's line by this point, which is
    // the line kotlinc maps every one of its generated members to.
    let companion_line = ir.classes[comp_id].decl_line;
    if companion_line != 0 {
        ir.fn_decl_lines.insert(cached, companion_line);
        ir.fn_sig_lines.insert(cached, companion_line);
    }
    let this = ir.add_expr(IrExpr::GetValue(0));
    let delegate = ir.add_expr(IrExpr::MethodCall {
        class: comp_id as u32,
        index: cached_index,
        receiver: this,
        args: vec![],
    });
    let dret = ir.add_expr(IrExpr::Return(Some(delegate)));
    let sbody = ir.add_expr(IrExpr::Block {
        stmts: vec![dret],
        value: None,
    });
    super::complete_frontend_serializer_accessor(ir, companion, sbody);
    [initializer, acc]
}

/// Whether kotlinc serializes `class` through a runtime serializer over its subclasses: an abstract
/// class (a cached `PolymorphicSerializer`) or a sealed class (a cached `SealedClassSerializer`).
/// It has no `$serializer`, but its subclasses still extend its `write$Self` and deserialization
/// constructor. A generic one, and a sealed interface, are not read here.
pub(super) fn is_polymorphic_base(class: &crate::ir::IrClass) -> bool {
    (class.is_abstract || class.is_sealed)
        && !class.is_interface
        && !class.is_enum
        && !class.is_object
}

/// [`is_polymorphic_base`] for the frontend's view of a source class, which decides whether a
/// `$serializer` classifier is published at all. A sealed class publishes none either way.
pub(super) fn frontend_is_polymorphic_base(ctx: &crate::plugins::FrontendClassContext<'_>) -> bool {
    ctx.is_abstract && !ctx.is_sealed && ctx.kind == crate::libraries::TypeKind::Class
}

/// `Reflection.getOrCreateKotlinClass(<classifier>.class)`: the `KClass` literal kotlinc passes to
/// a runtime serializer.
pub(super) fn kclass_literal(ir: &mut IrFile, classifier: TypeName) -> ExprId {
    let classlit = ir.add_expr(IrExpr::ClassConst {
        internal: Some(classifier),
    });
    ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Static {
            owner: type_name("kotlin/jvm/internal/Reflection"),
            name: "getOrCreateKotlinClass".to_string(),
            descriptor: "(Ljava/lang/Class;)Lkotlin/reflect/KClass;".to_string(),
            inline: InlineKind::None,
        }),
        dispatch_receiver: None,
        args: vec![classlit],
    })
}

/// `new PolymorphicSerializer(<classifier>::class, <annotations>)`.
pub(super) fn polymorphic_serializer(
    ir: &mut IrFile,
    classifier: TypeName,
    annotations: ExprId,
) -> ExprId {
    let kclass = kclass_literal(ir, classifier);
    ir.new_external(
        "kotlinx/serialization/PolymorphicSerializer",
        "(Lkotlin/reflect/KClass;[Ljava/lang/annotation/Annotation;)V",
        vec![kclass, annotations],
    )
}

/// A polymorphic base's `Companion.serializer()`: the cached
/// `new PolymorphicSerializer(<class>::class, <serial-info annotations>)` of an abstract class, or
/// the cached `SealedClassSerializer` of a sealed one.
pub(super) fn add_polymorphic_serializer(
    ir: &mut IrFile,
    ctx: &crate::plugins::PluginContext,
    class_id: ClassId,
    class_name: TypeName,
) {
    let serializer = if ir.classes[class_id as usize].is_sealed {
        super::sealed_serializer::sealed_class_serializer(ir, ctx, class_id)
    } else {
        let serial_info_unsupported = super::annotations::class_has_serial_info(ctx, ir, class_id);
        let annotations = serial_info_annotations(ir, class_name, serial_info_unsupported);
        polymorphic_serializer(ir, class_name, annotations)
    };
    // kotlinc narrows the initializer's value to the `KSerializer` it returns.
    let serializer = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: serializer,
        type_operand: class_ty(super::KSERIALIZER_FQ),
    });
    // kotlinc generates the cached serializer after the deserialization constructor, beside the
    // child-serializer cache, so the JVM emits both of its class members with that cache.
    for function in add_companion_cached_serializer(ir, class_id, class_name, serializer) {
        ir.serialization_cache_methods.insert(function);
    }
    // The class initializer ends with that store, and kotlinc returns from it on the class's last
    // line.
    let delegate = ir.statics.len() - 1;
    ir.statics[delegate].accessor_lines.declaration_end =
        ir.classes[class_id as usize].decl_end_line;
}

/// A generic polymorphic base's `Companion.serializer(typeSerial…)`: a new
/// `PolymorphicSerializer(<class>::class, <serial-info annotations>)` for an abstract class, or a
/// new `SealedClassSerializer` for a sealed one, on every call. Nothing is cached.
pub(super) fn add_generic_polymorphic_serializer_accessor(
    ir: &mut IrFile,
    ctx: &crate::plugins::PluginContext,
    class_id: ClassId,
    class_name: TypeName,
) {
    if ir.classes[class_id as usize].is_sealed {
        super::sealed_serializer::add_sealed_serializer_accessor(ir, ctx, class_id);
        return;
    }
    let serial_info_unsupported = super::annotations::class_has_serial_info(ctx, ir, class_id);
    let annotations = serial_info_annotations(ir, class_name, serial_info_unsupported);
    let serializer = polymorphic_serializer(ir, class_name, annotations);
    let serializer = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: serializer,
        type_operand: class_ty(super::KSERIALIZER_FQ),
    });
    let ret = ir.add_expr(IrExpr::Return(Some(serializer)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![ret],
        value: None,
    });
    let owner = super::serializer_accessor_owner(ir, class_id);
    super::complete_frontend_serializer_accessor(ir, owner, body);
}

/// `new ObjectSerializer(<serial name>, <object>.INSTANCE, <serial-info annotations>)`.
///
/// Typed retained annotation facts identify `@SerialInfo` applications. Runtime annotation
/// construction is not yet available to the serialization plugin, so preserve that exact case as
/// a plugin residual; emitting an empty array would silently change the descriptor contract.
pub(super) fn object_serializer(
    ir: &mut IrFile,
    object: TypeName,
    name: ExprId,
    serial_info_unsupported: bool,
) -> ExprId {
    let instance = ir.add_expr(IrExpr::ExternalStaticInstance {
        owner: object,
        ty: object,
        field: "INSTANCE".to_string(),
    });
    let annotations = serial_info_annotations(ir, object, serial_info_unsupported);
    ir.new_external(
        OBJECT_SERIALIZER_FQ,
        "(Ljava/lang/String;Ljava/lang/Object;[Ljava/lang/annotation/Annotation;)V",
        vec![name, instance, annotations],
    )
}

pub(super) fn serial_info_annotations(
    ir: &mut IrFile,
    owner: TypeName,
    serial_info_unsupported: bool,
) -> ExprId {
    if serial_info_unsupported {
        return ir.add_expr(IrExpr::PluginPlaceholder {
            plugin: "serialization",
            kind: "object-serial-info-annotations",
            exprs: Vec::new(),
            data: vec![owner],
            types: Vec::new(),
        });
    }
    ir.add_expr(IrExpr::Vararg {
        array_type: Ty::obj_args("kotlin/Array", &[class_ty("kotlin/Annotation")]),
        spreads: Vec::new(),
        elements: Vec::new(),
    })
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

/// A generic `@Serializable` class declared in this file with its generated `$serializer` (not a
/// sealed or abstract one, which has none): an element
/// typed by it constructs that serializer class from the type arguments' serializers
/// (`new G$$serializer(IntSerializer.INSTANCE)`), where a non-generic class reads a singleton.
pub(super) fn local_generic_serializable_class(
    ir: &IrFile,
    ctx: &crate::plugins::PluginContext,
    classifier: TypeName,
) -> bool {
    ir.classes
        .iter()
        .position(|class| {
            class.fq_name_id() == classifier
                && !class.is_object
                && !class.type_params.is_empty()
                && !class.is_sealed
                && !is_polymorphic_base(class)
        })
        .is_some_and(|index| {
            let class_id = index as ClassId;
            ctx.has_annotation(class_id, type_name(super::SERIALIZABLE_FQ))
                && super::custom_serializer_of(ctx, ir, class_id).is_none()
        })
}

/// A `@Serializable` sealed class or interface, or abstract class, declared in this file: its
/// `serializer()` returns a runtime `SealedClassSerializer` or `PolymorphicSerializer`.
pub(super) fn local_polymorphic_serializable_class(
    ir: &IrFile,
    ctx: &crate::plugins::PluginContext,
    classifier: TypeName,
) -> bool {
    ir.classes
        .iter()
        .position(|class| {
            class.fq_name_id() == classifier && (class.is_sealed || is_polymorphic_base(class))
        })
        .is_some_and(|index| {
            let class_id = index as ClassId;
            ctx.has_annotation(class_id, type_name(super::SERIALIZABLE_FQ))
                && super::custom_serializer_of(ctx, ir, class_id).is_none()
        })
}

/// A `@Serializable object`: `serializer()` is a member of the object returning the cached
/// `ObjectSerializer`, through a private `get$cachedSerializer()` that reads the delegate.
pub(super) fn add_object_serializer(
    ir: &mut IrFile,
    ctx: &crate::plugins::PluginContext,
    class_id: ClassId,
    object: TypeName,
) {
    let owner_line = ir.classes[class_id as usize].decl_start_line;

    // `get$cachedSerializer()`: `(KSerializer) $cachedSerializer$delegate.getValue()`.
    let read = ir.add_expr(IrExpr::ExternalStaticField {
        owner: object,
        name: CACHED_SERIALIZER_DELEGATE.to_string(),
        descriptor: "Lkotlin/Lazy;".to_string(),
    });
    let get_value = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::realized_virtual(
            type_name("kotlin/Lazy"),
            "getValue".to_string(),
            String::new(),
            Some((vec![], class_ty("kotlin/Any"))),
            true,
        )),
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
    ir.set_method_visibility(cached, crate::types::Visibility::Private);
    ir.synthetic_methods.insert(cached);
    if owner_line != 0 {
        ir.fn_decl_lines.insert(cached, owner_line);
        ir.fn_sig_lines.insert(cached, owner_line);
    }

    // `serializer()` delegates to the helper. The frontend already declared it on the object;
    // kotlinc's generated members follow in this order: `serializer()`, the helper, then the
    // delegate's initializer.
    let accessor = super::frontend_serializer_accessor(ir, object)
        .expect("a serializable object has its checked frontend serializer declaration");
    // Generated past the frontend's declaration-line transfer: kotlinc maps the accessor to the
    // annotated declaration's first line, like every other member it generates for the object.
    if owner_line != 0 {
        ir.fn_decl_lines.insert(accessor, owner_line);
        ir.fn_sig_lines.insert(accessor, owner_line);
    }
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
    super::complete_frontend_serializer_accessor(ir, object, body);

    let name = ir.add_expr(IrExpr::Const(IrConst::String(
        super::annotations::class_serial_name(ir, class_id),
    )));
    let serial_info_unsupported = super::annotations::class_has_serial_info(ctx, ir, class_id);
    let serializer = object_serializer(ir, object, name, serial_info_unsupported);
    // kotlinc narrows the initializer's value to the `KSerializer` it returns.
    let serializer = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: serializer,
        type_operand: class_ty(super::KSERIALIZER_FQ),
    });
    add_cached_serializer_delegate(ir, class_id, object, serializer);
    // An object's delegate is generated storage with no declaration of its own; kotlinc emits it
    // `ACC_SYNTHETIC` (an enum's is an ordinary private field).
    let delegate = u32::try_from(ir.statics.len() - 1).expect("static index fits u32");
    ir.mark_compiler_generated_static(delegate);
    ir.place_static_initializer_after_source(delegate);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_info_never_silently_becomes_an_empty_annotation_array() {
        let object = type_name("sample/StampedObject");
        let mut ir = IrFile::default();

        let unsupported = serial_info_annotations(&mut ir, object, true);
        assert!(matches!(
            ir.expr(unsupported),
            IrExpr::PluginPlaceholder {
                plugin: "serialization",
                kind: "object-serial-info-annotations",
                exprs,
                data,
                types,
            } if exprs.is_empty() && data == &[object] && types.is_empty()
        ));

        let empty = serial_info_annotations(&mut ir, object, false);
        assert!(matches!(
            ir.expr(empty),
            IrExpr::Vararg {
                array_type,
                spreads,
                elements,
            } if *array_type == Ty::obj_args("kotlin/Array", &[class_ty("kotlin/Annotation")])
                && spreads.is_empty()
                && elements.is_empty()
        ));
    }
}

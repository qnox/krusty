//! The `SealedClassSerializer` kotlinc builds for a `@Serializable` sealed class or interface.
//!
//! It names the base, every concrete subclass reached through the sealed hierarchy, and each
//! subclass's serializer: `new SealedClassSerializer(<serial name>, Base::class, [Sub::class…],
//! [<Sub serializer>…], <serial-info annotations>)`. A sealed class caches it in its
//! `$cachedSerializer$delegate` ([`super::cached_serializer`]); a sealed interface, and a generic
//! sealed class, build it in their `serializer()` accessor.

use super::annotations;
use super::cached_serializer::{
    kclass_literal, local_generic_serializable_class, polymorphic_serializer,
    serial_info_annotations,
};
use super::element_serializer::{element_serializer_expr, unsupported_element_serializer};
use super::serial_elements::type_parameter_identities;
use super::{
    class_ty, complete_frontend_serializer_accessor, kserializer_of, serializer_accessor_owner,
};
use crate::fx_hash::HashMap;
use crate::ir::{ClassId, ExprId, IrConst, IrExpr, IrFile, IrTypeOp, IrTypeParameter};
use crate::plugins::PluginContext;
use crate::types::{ty_subst_keep_unbound, Ty, TypeName};

const SEALED_CLASS_SERIALIZER_FQ: &str = "kotlinx/serialization/SealedClassSerializer";

/// `new SealedClassSerializer(…)` for the sealed `class_id`.
pub(super) fn sealed_class_serializer(
    ir: &mut IrFile,
    ctx: &PluginContext,
    class_id: ClassId,
) -> ExprId {
    let class_name = ir.classes[class_id as usize].fq_name_id();
    let subclasses = concrete_subclasses(ir, class_id);

    let serial_name = ir.add_expr(IrExpr::Const(IrConst::String(
        annotations::class_serial_name(ir, class_id),
    )));
    let base_kclass = kclass_literal(ir, class_name);
    let sub_kclasses: Vec<ExprId> = subclasses
        .iter()
        .map(|&subclass| kclass_literal(ir, subclass))
        .collect();
    let sub_serializers: Vec<ExprId> = subclasses
        .iter()
        .map(|&subclass| subclass_serializer(ir, ctx, class_name, subclass))
        .collect();
    let kclass_array = ir.add_expr(IrExpr::Vararg {
        array_type: Ty::obj_args("kotlin/Array", &[class_ty("kotlin/reflect/KClass")]),
        spreads: vec![false; sub_kclasses.len()],
        elements: sub_kclasses,
    });
    let serializer_array = ir.add_expr(IrExpr::Vararg {
        array_type: Ty::obj_args("kotlin/Array", &[kserializer_of(class_ty("kotlin/Any"))]),
        spreads: vec![false; sub_serializers.len()],
        elements: sub_serializers,
    });
    let serial_info_unsupported = annotations::class_has_serial_info(ctx, ir, class_id);
    let class_annotations = serial_info_annotations(ir, class_name, serial_info_unsupported);
    ir.new_external(
        SEALED_CLASS_SERIALIZER_FQ,
        "(Ljava/lang/String;Lkotlin/reflect/KClass;[Lkotlin/reflect/KClass;\
         [Lkotlinx/serialization/KSerializer;[Ljava/lang/annotation/Annotation;)V",
        vec![
            serial_name,
            base_kclass,
            kclass_array,
            serializer_array,
            class_annotations,
        ],
    )
}

/// `serializer()` of a sealed interface or a generic sealed class: it returns a new
/// `SealedClassSerializer` on every call.
pub(super) fn add_sealed_serializer_accessor(
    ir: &mut IrFile,
    ctx: &PluginContext,
    class_id: ClassId,
) {
    let serializer = sealed_class_serializer(ir, ctx, class_id);
    let ret = ir.add_expr(IrExpr::Return(Some(serializer)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![ret],
        value: None,
    });
    let owner = serializer_accessor_owner(ir, class_id);
    complete_frontend_serializer_accessor(ir, owner, body);
}

/// The subclasses kotlinc registers: the sealed base's direct subclasses in their metadata order,
/// with every sealed one replaced by its own subclasses, recursively.
fn concrete_subclasses(ir: &IrFile, class_id: ClassId) -> Vec<TypeName> {
    let mut subclasses = Vec::new();
    for subclass in ir.classes[class_id as usize].sorted_sealed_subclasses() {
        match local_class(ir, subclass) {
            Some(sealed) if ir.classes[sealed as usize].is_sealed => {
                subclasses.extend(concrete_subclasses(ir, sealed));
            }
            _ => subclasses.push(subclass),
        }
    }
    subclasses
}

/// A subclass's serializer, as kotlinc derives an element's: a non-generic class's
/// `$serializer.INSTANCE` and an object's `ObjectSerializer`. A generic class's `$serializer` is
/// constructed directly, over one serializer per type parameter: the generic sealed base's own
/// `typeSerial<j>` when the parameter is the base's `j`-th type argument, else a
/// `PolymorphicSerializer` of the parameter's representative upper bound.
fn subclass_serializer(
    ir: &mut IrFile,
    ctx: &PluginContext,
    base: TypeName,
    subclass: TypeName,
) -> ExprId {
    if local_generic_serializable_class(ir, ctx, subclass) {
        let formals: Vec<(String, Ty)> =
            ir.class_signature_name(subclass)
                .map_or_else(Vec::new, |signature| {
                    signature
                        .type_params
                        .iter()
                        .map(|parameter| {
                            (
                                parameter.semantic_name.clone(),
                                representative_upper_bound(parameter),
                            )
                        })
                        .collect()
                });
        let base_arguments = base_type_arguments(ir, base, subclass);
        let arguments: Vec<ExprId> = formals
            .iter()
            .map(|(identity, bound)| {
                match base_arguments
                    .iter()
                    .position(|argument| argument.ty_param_name() == Some(identity.as_str()))
                {
                    // The base's `serializer(typeSerial0, …)` is a companion member: its
                    // serializers follow `this`.
                    Some(j) => ir.add_expr(IrExpr::GetValue(1 + j as u32)),
                    None => type_parameter_serializer(ir, subclass, *bound),
                }
            })
            .collect();
        let descriptor = format!(
            "({})V",
            "Lkotlinx/serialization/KSerializer;".repeat(arguments.len())
        );
        return ir.new_external_name(subclass.nested_child("$serializer"), &descriptor, arguments);
    }
    let ty = Ty::obj_name(subclass);
    element_serializer_expr(ir, ctx, &ty).unwrap_or_else(|| unsupported_element_serializer(ir, ty))
}

/// The type arguments `subclass` gives the generic sealed `base`, in terms of the subclass's own
/// type parameters. A subclass reached through intermediate sealed classes composes each step's
/// arguments along its declared inheritance path, as kotlinc's plugin walks it from the leaf up to
/// the base; none when no path of this file's classes reaches the base.
fn base_type_arguments(ir: &IrFile, base: TypeName, subclass: TypeName) -> Vec<Ty> {
    let Some(subclass_id) = local_class(ir, subclass) else {
        return Vec::new();
    };
    for supertype in ir.classes[subclass_id as usize].supertypes.iter() {
        let Some(parent) = supertype.kotlin_class_internal() else {
            continue;
        };
        if parent == base {
            return supertype.type_args().to_vec();
        }
        let Some(parent_id) = local_class(ir, parent) else {
            continue;
        };
        let through_parent = base_type_arguments(ir, base, parent);
        if through_parent.is_empty() {
            continue;
        }
        let bindings: HashMap<String, Ty> = type_parameter_identities(ir, parent_id)
            .into_iter()
            .zip(supertype.type_args().iter().copied())
            .collect();
        return through_parent
            .into_iter()
            .map(|argument| ty_subst_keep_unbound(argument, &bindings))
            .collect();
    }
    Vec::new()
}

/// kotlinc's representative upper bound of a type parameter: its first bound that is a class
/// rather than an interface, else its first bound, else `Any?`.
fn representative_upper_bound(parameter: &IrTypeParameter) -> Ty {
    parameter
        .bounds
        .iter()
        .find(|&&(bound, is_interface)| !is_interface && bound.non_null().ty_param_name().is_none())
        .or_else(|| parameter.bounds.first())
        .map_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")), |&(bound, _)| bound)
}

/// `PolymorphicSerializer(<bound>::class, <serial-info annotations>)` cast to `KSerializer`. A
/// bound that is itself a type parameter is not supported yet: it stays a placeholder, which
/// declines the file rather than giving the formal another serializer.
fn type_parameter_serializer(ir: &mut IrFile, subclass: TypeName, bound: Ty) -> ExprId {
    if bound.non_null().ty_param_name().is_some() {
        return unsupported_element_serializer(ir, bound);
    }
    let Some(classifier) = bound.kotlin_class_internal() else {
        return unsupported_element_serializer(ir, bound);
    };
    let annotations = serial_info_annotations(ir, subclass, false);
    let polymorphic = polymorphic_serializer(ir, classifier, annotations);
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: polymorphic,
        type_operand: class_ty(super::KSERIALIZER_FQ),
    })
}

fn local_class(ir: &IrFile, classifier: TypeName) -> Option<ClassId> {
    ir.classes
        .iter()
        .position(|class| class.fq_name_id() == classifier)
        .map(|index| index as ClassId)
}

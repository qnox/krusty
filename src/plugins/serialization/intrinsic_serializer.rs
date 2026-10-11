//! The serializer kotlinx's `serializer<T>()` intrinsic produces for a checked type, planned in the
//! frontend and realized here.
//!
//! kotlinc's `SerializationJvmIrIntrinsicSupport.generateSerializerForType` takes a fast path first:
//! when T's companion (or T itself, as a `@Serializable object`) declares the
//! `serializer(KSerializer<T1>, …): KSerializer<T<T1, …>>` accessor, it calls that accessor with an
//! intrinsic serializer for each type argument — whatever serializer the class's own
//! `@Serializable(with = …)` names. Only a type without the accessor takes the general lookup. A
//! nullable type wraps the result in `.nullable`.
//!
//! The accessor is SELECTED in the frontend, from the declarations the resolver publishes with their
//! stable identities, by the declaration's full semantic signature. The plan carries that identity
//! as a synthesized call, so lowering realizes the exact declaration; nothing here looks a member up
//! again.

use crate::ir::{ExprId, IrExpr, IrFile};
use crate::plugins::{
    FrontendExpressionContext, FrontendResolvedSingletonCall, PluginContext,
    PluginSynthesizedOperand,
};
use crate::types::{type_name, Ty, TypeName};

use super::constructed_standard_serializers::constructed_standard_serializer;
use super::{kserializer_of, wrap_nullable_serializer, KSERIALIZER_FQ, SERIALIZABLE_FQ};

/// The member name the intrinsic's fast path calls.
pub(super) const ACCESSOR_NAME: &str = "serializer";

const CONSTRUCTED: &str = "constructedSerializer";
const NULLABLE: &str = "nullableSerializer";
const GENERAL: &str = "typeSerializer";

/// Plan the intrinsic serializer for `ty`. `None` when no serializer can be planned: a star
/// projection (kotlinc emits a throw), or a classifier whose singleton declares more than one
/// matching accessor.
pub(super) fn plan(
    ctx: &FrontendExpressionContext<'_>,
    ty: Ty,
) -> Option<PluginSynthesizedOperand> {
    let checked = ty.non_null();
    let classifier = checked.kotlin_class_internal()?;
    let arguments = checked
        .type_args()
        .iter()
        .map(|argument| match argument {
            Ty::StarProjection(_) => None,
            Ty::OutProjection(inner) | Ty::InProjection(inner) => Some(**inner),
            _ => Some(*argument),
        })
        .collect::<Option<Vec<_>>>()?;
    let result = kserializer_of(checked);
    let parameter_types = arguments
        .iter()
        .map(|&argument| kserializer_of(argument))
        .collect::<Vec<_>>();
    let planned = match accessor(ctx, classifier, &parameter_types, result)? {
        Accessor::Selected(call) => PluginSynthesizedOperand::SingletonCall {
            call,
            arguments: arguments
                .iter()
                .map(|&argument| plan(ctx, argument))
                .collect::<Option<Vec<_>>>()?,
        },
        Accessor::Absent => match constructed_standard_serializer(classifier) {
            Some(serializer) if !arguments.is_empty() => operation(
                CONSTRUCTED,
                vec![serializer],
                Vec::new(),
                result,
                arguments
                    .iter()
                    .map(|&argument| plan(ctx, argument))
                    .collect::<Option<Vec<_>>>()?,
            ),
            _ => operation(GENERAL, Vec::new(), vec![checked], result, Vec::new()),
        },
    };
    Some(if ty.is_nullable() {
        operation(
            NULLABLE,
            Vec::new(),
            Vec::new(),
            kserializer_of(ty),
            vec![planned],
        )
    } else {
        planned
    })
}

fn operation(
    operation: &'static str,
    data: Vec<TypeName>,
    types: Vec<Ty>,
    result: Ty,
    operands: Vec<PluginSynthesizedOperand>,
) -> PluginSynthesizedOperand {
    PluginSynthesizedOperand::Operation {
        plugin: "serialization",
        operation,
        data,
        types,
        result,
        operands,
    }
}

/// What a classifier's singleton offers the intrinsic's fast path.
enum Accessor {
    Absent,
    Selected(FrontendResolvedSingletonCall),
}

/// Ask the ordinary resolver/checker for the singleton call. The plugin only recognizes whether the
/// already-selected declaration is the serialization accessor contract; it never sees or chooses
/// another candidate.
fn accessor(
    ctx: &FrontendExpressionContext<'_>,
    classifier: TypeName,
    parameters: &[Ty],
    result: Ty,
) -> Option<Accessor> {
    let call = ctx.resolve_singleton_member_call(classifier, ACCESSOR_NAME, parameters, result);
    let Some(call) = call else {
        return Some(Accessor::Absent);
    };
    // Only a `@Serializable object` serves as its own accessor's receiver; any other classifier's
    // accessor lives on its companion.
    if (call.receiver == classifier
        && !ctx.has_classifier_annotation(classifier, type_name(SERIALIZABLE_FQ)))
        || call.selected.suspend
        || !is_serializer_accessor(&call, classifier, parameters.len())
    {
        return Some(Accessor::Absent);
    }
    Some(Accessor::Selected(call))
}

/// Whether `callable`'s DECLARED signature is exactly
/// `serializer(KSerializer<P1>, …, KSerializer<Pn>): KSerializer<classifier<P1, …, Pn>>`, the
/// `Pi` distinct type parameters in declaration order. A same-name, same-arity declaration whose
/// parameter or result type arguments differ is a different function and is never selected.
pub(super) fn is_serializer_accessor(
    call: &FrontendResolvedSingletonCall,
    classifier: TypeName,
    type_parameters: usize,
) -> bool {
    let callable = &call.selected.member;
    if callable.context_count != 0
        || callable.call_sig.vararg_index.is_some()
        || callable.call_sig.required != callable.params.len()
        || !call
            .argument_parameters
            .iter()
            .enumerate()
            .all(|(index, &parameter)| usize::try_from(parameter) == Ok(index))
    {
        return false;
    }
    let (params, ret) = callable
        .generic_sig
        .as_ref()
        .map_or((callable.params.as_slice(), callable.ret), |signature| {
            (signature.params.as_slice(), signature.ret)
        });
    signature_is_serializer_accessor(&callable.name, params, ret, classifier, type_parameters)
}

fn signature_is_serializer_accessor(
    name: &str,
    params: &[Ty],
    ret: Ty,
    classifier: TypeName,
    type_parameters: usize,
) -> bool {
    let serializer = type_name(KSERIALIZER_FQ);
    if name != ACCESSOR_NAME || params.len() != type_parameters {
        return false;
    }
    let mut formals: Vec<Ty> = Vec::with_capacity(type_parameters);
    for &parameter in params {
        let Ty::Obj(owner, [argument]) = parameter else {
            return false;
        };
        if owner != serializer
            || argument.is_nullable()
            || argument.ty_param_name().is_none()
            || formals.contains(argument)
        {
            return false;
        }
        formals.push(*argument);
    }
    let Ty::Obj(owner, [served]) = ret else {
        return false;
    };
    owner == serializer
        && !served.is_nullable()
        && served.kotlin_class_internal() == Some(classifier)
        && served.type_args() == formals.as_slice()
}

/// Realize one nested intrinsic-serializer operation. `true` when the kind belongs to this module;
/// an operation whose serializer cannot be built stays a placeholder, which fails closed.
pub(super) fn specialize(
    ir: &mut IrFile,
    ctx: &PluginContext,
    index: usize,
    kind: &str,
    exprs: &[ExprId],
    data: &[TypeName],
    types: &[Ty],
) -> bool {
    match (kind, exprs, data, types) {
        (CONSTRUCTED, arguments, [serializer], []) => {
            let arity = arguments.len();
            // The intrinsic constructs the runtime serializer directly over its operands, without
            // narrowing them.
            ir.exprs[index] = IrExpr::New {
                internal: *serializer,
                args: arguments.to_vec(),
                ctor_params: Some(vec![Ty::obj(KSERIALIZER_FQ); arity]),
                ctor_desc: Some(format!(
                    "({})V",
                    "Lkotlinx/serialization/KSerializer;".repeat(arity)
                )),
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            };
            true
        }
        (NULLABLE, [inner], [], []) => {
            let wrapped = wrap_nullable_serializer(ir, *inner);
            ir.exprs[index] = ir.exprs[wrapped as usize].clone();
            true
        }
        (GENERAL, [], [], [ty]) => {
            if let Some(serializer) = super::element_serializer_expr(ir, ctx, ty) {
                ir.exprs[index] = ir.exprs[serializer as usize].clone();
            }
            true
        }
        (CONSTRUCTED | NULLABLE | GENERAL, ..) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serializer(argument: Ty) -> Ty {
        kserializer_of(argument)
    }

    #[test]
    fn the_accessor_is_selected_by_its_full_declared_signature() {
        let reading = type_name("dep/Reading");
        assert!(signature_is_serializer_accessor(
            ACCESSOR_NAME,
            &[],
            serializer(Ty::obj_name(reading)),
            reading,
            0
        ));
        let boxed = type_name("dep/Box");
        let t = Ty::ty_param("T0", Ty::nullable(Ty::obj("kotlin/Any")));
        assert!(signature_is_serializer_accessor(
            ACCESSOR_NAME,
            &[serializer(t)],
            serializer(Ty::obj_args_name(boxed, &[t])),
            boxed,
            1
        ));
    }

    /// The same name, arity and `KSerializer` outer types are not the accessor when a generic
    /// argument differs: a result serving another type, a concrete parameter argument, a result
    /// whose arguments are not the parameters, or a nullable served type.
    #[test]
    fn a_lookalike_overload_is_not_the_accessor() {
        let reading = type_name("dep/Reading");
        let boxed = type_name("dep/Box");
        let t = Ty::ty_param("T0", Ty::nullable(Ty::obj("kotlin/Any")));
        let u = Ty::ty_param("U0", Ty::nullable(Ty::obj("kotlin/Any")));
        for (params, ret, classifier, arity) in [
            (Vec::new(), serializer(Ty::String), reading, 0),
            (
                Vec::new(),
                serializer(Ty::nullable(Ty::obj_name(reading))),
                reading,
                0,
            ),
            (
                vec![serializer(Ty::String)],
                serializer(Ty::obj_args_name(boxed, &[Ty::String])),
                boxed,
                1,
            ),
            (
                vec![serializer(t)],
                serializer(Ty::obj_args_name(boxed, &[u])),
                boxed,
                1,
            ),
            (
                vec![serializer(t)],
                serializer(Ty::obj_args("kotlin/collections/List", &[t])),
                boxed,
                1,
            ),
        ] {
            assert!(
                !signature_is_serializer_accessor(ACCESSOR_NAME, &params, ret, classifier, arity,),
                "{params:?} -> {ret:?} must not be selected"
            );
        }
    }
}

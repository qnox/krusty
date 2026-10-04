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
    FrontendDeclaredCallable, FrontendExpressionContext, PluginContext, PluginSynthesizedOperand,
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
pub(super) fn plan(ctx: &FrontendExpressionContext, ty: Ty) -> Option<PluginSynthesizedOperand> {
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
    let planned = match accessor(ctx, classifier, arguments.len())? {
        Accessor::Selected(receiver, accessor) => PluginSynthesizedOperand::SingletonCall {
            receiver,
            target: accessor.target,
            params: arguments
                .iter()
                .map(|&argument| kserializer_of(argument))
                .collect(),
            ret: result,
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
enum Accessor<'a> {
    Absent,
    /// The singleton receiver and its accessor declaration.
    Selected(TypeName, &'a FrontendDeclaredCallable),
}

/// The accessor of `classifier`'s singleton. `None` when its declarations are ambiguous, which
/// leaves the type unplanned.
fn accessor(
    ctx: &FrontendExpressionContext,
    classifier: TypeName,
    arity: usize,
) -> Option<Accessor<'_>> {
    let (Some(members), Some(&type_parameters)) = (
        ctx.singleton_members.get(&classifier),
        ctx.classifier_type_parameters.get(&classifier),
    ) else {
        return Some(Accessor::Absent);
    };
    // Only a `@Serializable object` serves as its own accessor's receiver; any other classifier's
    // accessor lives on its companion.
    if type_parameters != arity
        || (members.receiver_is_classifier
            && !ctx.has_classifier_annotation(classifier, type_name(SERIALIZABLE_FQ)))
    {
        return Some(Accessor::Absent);
    }
    let mut matching = members
        .callables
        .iter()
        .filter(|callable| is_serializer_accessor(callable, classifier, type_parameters));
    let selected = matching.next();
    if matching.next().is_some() {
        return None;
    }
    Some(match selected {
        Some(callable) => Accessor::Selected(members.receiver, callable),
        None => Accessor::Absent,
    })
}

/// Whether `callable`'s DECLARED signature is exactly
/// `serializer(KSerializer<P1>, …, KSerializer<Pn>): KSerializer<classifier<P1, …, Pn>>`, the
/// `Pi` distinct type parameters in declaration order. A same-name, same-arity declaration whose
/// parameter or result type arguments differ is a different function and is never selected.
pub(super) fn is_serializer_accessor(
    callable: &FrontendDeclaredCallable,
    classifier: TypeName,
    type_parameters: usize,
) -> bool {
    let serializer = type_name(KSERIALIZER_FQ);
    if callable.name != ACCESSOR_NAME || callable.params.len() != type_parameters {
        return false;
    }
    let mut formals: Vec<Ty> = Vec::with_capacity(type_parameters);
    for parameter in &callable.params {
        let Ty::Obj(owner, [argument]) = *parameter else {
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
    let Ty::Obj(owner, [served]) = callable.ret else {
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
    use crate::plugins::FrontendCallableTarget;

    fn callable(params: Vec<Ty>, ret: Ty) -> FrontendDeclaredCallable {
        FrontendDeclaredCallable {
            name: ACCESSOR_NAME.to_owned(),
            params,
            ret,
            target: FrontendCallableTarget::module_for_test(0),
        }
    }

    fn serializer(argument: Ty) -> Ty {
        kserializer_of(argument)
    }

    #[test]
    fn the_accessor_is_selected_by_its_full_declared_signature() {
        let reading = type_name("dep/Reading");
        assert!(is_serializer_accessor(
            &callable(Vec::new(), serializer(Ty::obj_name(reading))),
            reading,
            0
        ));
        let boxed = type_name("dep/Box");
        let t = Ty::ty_param("T0", Ty::nullable(Ty::obj("kotlin/Any")));
        assert!(is_serializer_accessor(
            &callable(
                vec![serializer(t)],
                serializer(Ty::obj_args_name(boxed, &[t]))
            ),
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
        for (candidate, classifier, arity) in [
            (callable(Vec::new(), serializer(Ty::String)), reading, 0),
            (
                callable(Vec::new(), serializer(Ty::nullable(Ty::obj_name(reading)))),
                reading,
                0,
            ),
            (
                callable(
                    vec![serializer(Ty::String)],
                    serializer(Ty::obj_args_name(boxed, &[Ty::String])),
                ),
                boxed,
                1,
            ),
            (
                callable(
                    vec![serializer(t)],
                    serializer(Ty::obj_args_name(boxed, &[u])),
                ),
                boxed,
                1,
            ),
            (
                callable(
                    vec![serializer(t)],
                    serializer(Ty::obj_args("kotlin/collections/List", &[t])),
                ),
                boxed,
                1,
            ),
        ] {
            assert!(
                !is_serializer_accessor(&candidate, classifier, arity),
                "{candidate:?} must not be selected"
            );
        }
    }
}

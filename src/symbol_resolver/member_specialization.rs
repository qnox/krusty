//! Receiver-bound specialization of declared member signatures.

use super::{ty_subst, ty_subst_keep_unbound, GSigBinds};
use crate::libraries::{
    GenericSig, InlineBodyPlan, InlineCollectionAppend, InlineCollectionCapacity,
    InlineIterationTraversal, LibraryCallable, LibraryMember, PropertyInfo,
};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

#[derive(Clone, Copy)]
pub(crate) enum TypePosition {
    In,
    Out,
    Invariant,
}

#[derive(Clone, Copy)]
pub(crate) enum UnboundSpecialization {
    Preserve,
    UseUpperBound,
}

fn compose_position(position: TypePosition, variance: crate::types::TypeVariance) -> TypePosition {
    use crate::types::TypeVariance;
    match (position, variance) {
        (TypePosition::Invariant, _) | (_, TypeVariance::Invariant) => TypePosition::Invariant,
        (TypePosition::Out, TypeVariance::Out) | (TypePosition::In, TypeVariance::In) => {
            TypePosition::Out
        }
        (TypePosition::Out, TypeVariance::In) | (TypePosition::In, TypeVariance::Out) => {
            TypePosition::In
        }
    }
}

/// Substitute receiver-bound type parameters through a member signature while retaining use-site
/// projection semantics. Projection belongs to the classifier argument (`Box<out X>`); a value type
/// never becomes `out X`: reads expose `X`, writes admit `Nothing`, and nested function/class variance
/// composes with the surrounding position.
fn specialize_member_type_with_unbound(
    source: &dyn SymbolSource,
    ty: Ty,
    bindings: &GSigBinds,
    position: TypePosition,
    unbound: UnboundSpecialization,
) -> Ty {
    match ty {
        Ty::TyParam(name, bound) => {
            let Some(binding) = bindings.get(name).copied() else {
                return match unbound {
                    UnboundSpecialization::Preserve => ty,
                    UnboundSpecialization::UseUpperBound => bound.non_null(),
                };
            };
            let adjusted = |binding: Ty| {
                if bound.upper_bound_admits_null() {
                    binding
                } else {
                    binding.non_null()
                }
            };
            match (position, binding) {
                (TypePosition::Out, Ty::OutProjection(inner) | Ty::StarProjection(inner)) => {
                    adjusted(*inner)
                }
                (TypePosition::In, Ty::OutProjection(_) | Ty::StarProjection(_)) => Ty::Nothing,
                (TypePosition::Out, Ty::InProjection(_)) => *bound,
                (TypePosition::In, Ty::InProjection(inner)) => adjusted(*inner),
                (
                    TypePosition::Invariant,
                    projected
                    @ (Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_)),
                ) => projected,
                (_, binding) => adjusted(binding),
            }
        }
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| {
                    specialize_member_type_with_unbound(
                        source,
                        *parameter,
                        bindings,
                        compose_position(position, crate::types::TypeVariance::In),
                        unbound,
                    )
                })
                .collect(),
            specialize_member_type_with_unbound(source, signature.ret, bindings, position, unbound),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Nullable(inner) => Ty::nullable(specialize_member_type_with_unbound(
            source, *inner, bindings, position, unbound,
        )),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(specialize_member_type_with_unbound(
            source, *inner, bindings, position, unbound,
        )),
        Ty::InProjection(inner) => Ty::in_projection(specialize_member_type_with_unbound(
            source,
            *inner,
            bindings,
            compose_position(position, crate::types::TypeVariance::In),
            unbound,
        )),
        Ty::OutProjection(inner) => Ty::out_projection(specialize_member_type_with_unbound(
            source,
            *inner,
            bindings,
            compose_position(position, crate::types::TypeVariance::Out),
            unbound,
        )),
        Ty::StarProjection(inner) => Ty::star_projection(specialize_member_type_with_unbound(
            source,
            *inner,
            bindings,
            compose_position(position, crate::types::TypeVariance::Out),
            unbound,
        )),
        Ty::Obj(internal, arguments) if !arguments.is_empty() => {
            let variances = source
                .classifier(internal)
                .map(|classifier| classifier.type_param_variances.clone())
                .unwrap_or_default();
            Ty::obj_args_name(
                internal,
                &arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        specialize_member_type_with_unbound(
                            source,
                            *argument,
                            bindings,
                            compose_position(
                                position,
                                variances.get(index).copied().unwrap_or_default(),
                            ),
                            unbound,
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        }
        _ => ty,
    }
}

pub(super) fn specialize_member_type(
    source: &dyn SymbolSource,
    ty: Ty,
    bindings: &GSigBinds,
    position: TypePosition,
) -> Ty {
    specialize_member_type_with_unbound(
        source,
        ty,
        bindings,
        position,
        UnboundSpecialization::Preserve,
    )
}

/// THE decision point for what a projected type argument means to a consumer. A binding map may
/// legitimately carry a use-site projection (`List<*>` binds a formal to `out Any?` — the stand-in
/// for kotlinc's captured type); what that capture means depends only on the POSITION of the slot
/// being instantiated, never on the callee: a read sees the projection's readable bound, a write
/// admits `Nothing`, and a classifier-argument position keeps the projection, because `List<out X>`
/// is a legal type. Raw `ty_subst`/`ty_subst_keep_unbound` ARE that invariant rule and stay correct
/// wherever a receiver or classifier argument is formed; every slot that types a VALUE — parameter,
/// return, lambda input — instantiates through here instead. `signature` supplies `formal_bounds`
/// for bound-aware reads, since the inline `TyParam` bound is not always populated.
pub(crate) fn instantiate_slot(
    source: &dyn SymbolSource,
    signature: Option<&GenericSig>,
    ty: Ty,
    bindings: &GSigBinds,
    position: TypePosition,
    unbound: UnboundSpecialization,
) -> Ty {
    if !bindings
        .values()
        .any(|binding| binding.projection_inner().is_some())
    {
        return match unbound {
            UnboundSpecialization::Preserve => ty_subst_keep_unbound(ty, bindings),
            UnboundSpecialization::UseUpperBound => ty_subst(ty, bindings),
        };
    }
    let ty = signature.map_or(ty, |signature| {
        let bounds: std::collections::HashMap<String, Ty> = signature
            .formals
            .iter()
            .zip(&signature.formal_bounds)
            .filter_map(|(formal, bounds)| bounds.first().map(|bound| (formal.clone(), *bound)))
            .collect();
        if bounds.is_empty() {
            ty
        } else {
            crate::types::ty_with_param_bounds(ty, &bounds)
        }
    });
    specialize_member_type_with_unbound(source, ty, bindings, position, unbound)
}

pub(crate) fn specialize_signature_input_type(
    source: &dyn SymbolSource,
    ty: Ty,
    bindings: &GSigBinds,
) -> Ty {
    specialize_member_type_with_unbound(
        source,
        ty,
        bindings,
        TypePosition::In,
        UnboundSpecialization::Preserve,
    )
}

pub(crate) fn specialize_signature_output_type(
    source: &dyn SymbolSource,
    ty: Ty,
    bindings: &GSigBinds,
) -> Ty {
    specialize_member_type_with_unbound(
        source,
        ty,
        bindings,
        TypePosition::Out,
        UnboundSpecialization::Preserve,
    )
}

pub(super) fn specialize_final_signature_output_type(
    source: &dyn SymbolSource,
    ty: Ty,
    bindings: &GSigBinds,
) -> Ty {
    specialize_member_type_with_unbound(
        source,
        ty,
        bindings,
        TypePosition::Out,
        UnboundSpecialization::UseUpperBound,
    )
}

pub(crate) fn specialize_signature_receiver_type(
    source: &dyn SymbolSource,
    ty: Ty,
    bindings: &GSigBinds,
) -> Ty {
    specialize_member_type_with_unbound(
        source,
        ty,
        bindings,
        TypePosition::Invariant,
        UnboundSpecialization::Preserve,
    )
}

/// Apply an already-selected provider member declaration to one concrete dispatch receiver.
/// Unlike ordinary member resolution this performs no name lookup or overload selection: decoded
/// inline-body plans use it to specialize the exact stable declaration named by their bytecode.
fn specialize_fixed_member_receiver(
    source: &dyn SymbolSource,
    receiver: Ty,
    member: &LibraryMember,
) -> Option<LibraryMember> {
    let owner = member.owner?;
    let classifier = source.classifier(owner)?;
    let applied_owner = super::applied_hierarchy(source, receiver)
        .into_iter()
        .find_map(|(candidate, applied, _)| (candidate == owner).then_some(applied))?;
    let bindings = super::classifier_bindings(&classifier, applied_owner);
    let declaration_formals = classifier
        .type_params()
        .iter()
        .chain(
            member
                .generic_sig
                .as_ref()
                .into_iter()
                .flat_map(|signature| &signature.formals),
        )
        .cloned()
        .collect::<Vec<_>>();
    let mut specialized = member.clone();
    // `params`/`ret` are the erased callable realization for classpath members. Kotlin metadata's
    // generic signature is the semantic declaration and retains the owning classifier's type
    // parameters (`Map<K, V>.entries: Set<Map.Entry<K, V>>`, `Iterator<T>.next(): T`). Apply the
    // already-selected receiver to that declaration; using the erased fields here would turn the
    // exact stable chain back into `Set`/`Iterator`/`Any` after resolution.
    let (declared_params, declared_ret) = member
        .generic_sig
        .as_ref()
        .map_or((member.params.as_slice(), member.ret), |signature| {
            (signature.params.as_slice(), signature.ret)
        });
    specialized.params = declared_params
        .iter()
        .map(|parameter| specialize_signature_input_type(source, *parameter, &bindings))
        .collect();
    specialized.ret = specialize_signature_output_type(source, declared_ret, &bindings);
    member_slots_are_applied(&specialized.params, specialized.ret, &declaration_formals)
        .then_some(specialized)
}

fn member_slots_are_applied(params: &[Ty], ret: Ty, declaration_formals: &[String]) -> bool {
    !params
        .iter()
        .any(|parameter| crate::types::ty_mentions_param(*parameter, declaration_formals))
        && !crate::types::ty_mentions_param(ret, declaration_formals)
}

/// Specialize the exact member chain decoded for an iteration body. The declaration identities are
/// already fixed by the provider; this applies receiver type arguments only and never performs a
/// spelling lookup or overload scan.
pub(crate) fn specialize_inline_iteration_traversal(
    source: &dyn SymbolSource,
    receiver: Ty,
    traversal: &mut InlineIterationTraversal,
) -> Option<()> {
    match traversal {
        InlineIterationTraversal::Iterator {
            prepare,
            has_next,
            next,
        } => {
            let mut current = receiver;
            for member in prepare {
                *member = specialize_fixed_member_receiver(source, current, member)?;
                current = member.ret;
            }
            **has_next = specialize_fixed_member_receiver(source, current, has_next)?;
            **next = specialize_fixed_member_receiver(source, current, next)?;
        }
        InlineIterationTraversal::Array => {}
        InlineIterationTraversal::Counted { size, get } => {
            **size = specialize_fixed_member_receiver(source, receiver, size)?;
            **get = specialize_fixed_member_receiver(source, receiver, get)?;
        }
    }
    Some(())
}

fn specialize_fixed_extension(
    source: &dyn SymbolSource,
    receiver: Ty,
    arguments: &[Ty],
    callable: &LibraryCallable,
) -> Option<LibraryCallable> {
    let signature = callable.generic_sig.as_deref()?;
    let declared_receiver = signature.receiver?;
    if signature.params.len() != arguments.len() {
        return None;
    }
    let mut bindings = GSigBinds::new();
    super::unify_ty_from_symbols(source, declared_receiver, receiver, &mut bindings);
    for (&declared, &actual) in signature.params.iter().zip(arguments) {
        super::unify_ty_from_symbols(source, declared, actual, &mut bindings);
    }
    let mut specialized = callable.clone();
    specialized.params = std::iter::once(specialize_signature_receiver_type(
        source,
        declared_receiver,
        &bindings,
    ))
    .chain(
        signature
            .params
            .iter()
            .map(|parameter| specialize_signature_input_type(source, *parameter, &bindings)),
    )
    .collect();
    specialized.ret = specialize_signature_output_type(source, signature.ret, &bindings);
    specialized.source_receiver = specialized.params.first().copied();
    let [specialized_receiver, specialized_arguments @ ..] = specialized.params.as_slice() else {
        return None;
    };
    (member_slots_are_applied(&specialized.params, specialized.ret, &signature.formals)
        && super::semantic_arg_assignable(source, specialized_receiver, &receiver)
        && specialized_arguments
            .iter()
            .zip(arguments)
            .all(|(declared, actual)| super::semantic_arg_assignable(source, declared, actual)))
    .then_some(specialized)
}

/// Apply selected receiver/output types to every exact dependency in a decoded collection body.
/// The provider already fixed each declaration identity; this only specializes its metadata
/// signature and never performs callable lookup or name-based dispatch.
pub(crate) fn specialize_inline_collection_transform(
    source: &dyn SymbolSource,
    receiver: Ty,
    part: Ty,
    output: Ty,
    plan: &mut InlineBodyPlan,
) -> Option<()> {
    let InlineBodyPlan::CollectionTransform {
        traversal,
        factory,
        capacity,
        append,
        ..
    } = plan
    else {
        return None;
    };
    specialize_inline_iteration_traversal(source, receiver, traversal)?;
    let factory_owner = factory.owner?;
    if source.classifier(factory_owner)?.type_params().len() != 1 {
        return None;
    }
    let accumulator = Ty::obj_args_name(factory_owner, &[output]);
    if let Some(capacity) = capacity {
        match capacity {
            InlineCollectionCapacity::Member(member) => {
                **member = specialize_fixed_member_receiver(source, receiver, member)?;
            }
            InlineCollectionCapacity::Extension { callable, .. } => {
                **callable = specialize_fixed_extension(source, receiver, &[Ty::Int], callable)?;
            }
        }
    }
    match append {
        InlineCollectionAppend::Member(member) => {
            **member = specialize_fixed_member_receiver(source, accumulator, member)?;
        }
        InlineCollectionAppend::Extension(callable) => {
            **callable = specialize_fixed_extension(source, accumulator, &[part], callable)?;
        }
    }
    Some(())
}

pub(super) fn specialize_callable(
    source: &dyn SymbolSource,
    callable: &mut LibraryCallable,
    bindings: &GSigBinds,
) {
    callable.params = callable
        .params
        .iter()
        .map(|ty| specialize_member_type(source, *ty, bindings, TypePosition::In))
        .collect();
    callable.ret = specialize_member_type(source, callable.ret, bindings, TypePosition::Out);
    callable.source_receiver = callable
        .source_receiver
        .map(|ty| specialize_member_type(source, ty, bindings, TypePosition::Invariant));
    // A declared bare type parameter stays one: bound to a value class it is still a box read out
    // of an erased slot, and only the unsubstituted declaration says so.
    callable.declared_ret = callable.declared_ret.map(|ty| match ty {
        Ty::TyParam(..) => ty,
        _ => specialize_member_type(source, ty, bindings, TypePosition::Out),
    });
}

/// Apply already-solved declaration type arguments to one selected Kotlin property.
///
/// Property type parameters can be constrained by more than the extension receiver: context
/// parameters are declaration inputs too.  Selection owns that inference and publishes one fully
/// specialized semantic property; consumers must not try to recover it from an accessor spelling
/// or from a backend realization.
pub(crate) fn apply_property_bindings(
    source: &dyn SymbolSource,
    property: &mut PropertyInfo,
    bindings: &GSigBinds,
) {
    property.receiver = property
        .receiver
        .map(|ty| specialize_member_type(source, ty, bindings, TypePosition::Invariant));
    property.ty = specialize_member_type(source, property.ty, bindings, TypePosition::Out);
    specialize_callable(source, &mut property.getter, bindings);
    property.getter.ret = property.ty;
    if let Some(setter) = &mut property.setter {
        specialize_callable(source, setter, bindings);
    }
}

pub(super) fn specialize_call_sig(
    source: &dyn SymbolSource,
    call_sig: &mut crate::libraries::CallSig,
    bindings: &GSigBinds,
) {
    for parameters in &mut call_sig.lambda_param_types {
        for parameter in parameters {
            *parameter = specialize_member_type(source, *parameter, bindings, TypePosition::Out);
        }
    }
    for receiver in &mut call_sig.lambda_receivers {
        *receiver =
            receiver.map(|ty| specialize_member_type(source, ty, bindings, TypePosition::Out));
    }
}

#[cfg(test)]
mod projected_member_view_tests {
    use super::{member_slots_are_applied, specialize_member_type, GSigBinds, TypePosition};
    use crate::libraries::LibraryType;
    use crate::symbol_source::SymbolSource;
    use crate::types::{type_name, Ty, TypeName};

    struct Source;

    impl SymbolSource for Source {
        fn classifier(&self, _internal: TypeName) -> Option<std::sync::Arc<LibraryType>> {
            None
        }
    }

    fn bindings(projected: Ty) -> GSigBinds {
        GSigBinds::from([("T".to_string(), projected)])
    }

    #[test]
    fn projected_classifier_arguments_become_read_and_write_views() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let out = bindings(Ty::out_projection(Ty::String));
        assert_eq!(
            specialize_member_type(&Source, parameter, &out, TypePosition::Out),
            Ty::String
        );
        assert_eq!(
            specialize_member_type(&Source, parameter, &out, TypePosition::In),
            Ty::Nothing
        );

        let input = bindings(Ty::in_projection(Ty::String));
        assert_eq!(
            specialize_member_type(&Source, parameter, &input, TypePosition::Out),
            Ty::nullable(Ty::obj("kotlin/Any"))
        );
        assert_eq!(
            specialize_member_type(&Source, parameter, &input, TypePosition::In),
            Ty::String
        );
    }

    #[test]
    fn function_parameter_position_reverses_the_member_position() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let callback = Ty::fun(vec![parameter], Ty::Unit);
        let out = bindings(Ty::out_projection(Ty::String));
        assert_eq!(
            specialize_member_type(&Source, callback, &out, TypePosition::In),
            Ty::fun(vec![Ty::String], Ty::Unit)
        );
    }

    #[test]
    fn projected_binding_stays_projected_in_invariant_nested_position() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let nested = Ty::obj_args_name(type_name("sample/Invariant"), &[parameter]);
        let out = bindings(Ty::out_projection(Ty::String));
        assert_eq!(
            specialize_member_type(&Source, nested, &out, TypePosition::Out),
            Ty::obj_args("sample/Invariant", &[Ty::out_projection(Ty::String)])
        );
    }

    #[test]
    fn applied_member_slots_preserve_an_enclosing_declaration_identity() {
        let provider_formals = ["T".to_owned()];
        let provider_t = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let source_e = Ty::ty_param("\0tp:source:emit:E", Ty::nullable(Ty::obj("kotlin/Any")));

        assert!(member_slots_are_applied(&[], source_e, &provider_formals));
        assert!(!member_slots_are_applied(
            &[],
            provider_t,
            &provider_formals
        ));
    }
}

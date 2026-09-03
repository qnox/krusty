//! Call resolution — the binding layer that sits *above* a [`SymbolSource`].
//!
//! A [`SymbolSource`] is an argument-independent metadata oracle: given a name (and optional receiver)
//! it returns every overload with its raw signature and flags ([`crate::libraries::FunctionSet`]). It
//! does no overload selection and no type-variable binding.
//!
//! [`SymbolResolver`] is the arg-dependent layer on top: given the actual argument types at a call site
//! it selects the right overload and binds generic receiver/parameter/return types. It uses
//! [`crate::libraries::SemanticPlatform`] for source-level library facts; backend descriptors and runtime
//! ABI are not part of resolution.

use crate::libraries::{
    CallSig, Callables, FnKind, FunctionInfo, FunctionSet, GenericSig, LibraryCallable,
    LibraryMember, Origin, PropKind, PropertyInfo, PropertySet, SemanticPlatform, SourceMember,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{Ty, TypeName, Visibility};

mod call_argument;
mod callable_shapes;
mod classifier_scope;
mod generic_inference;
mod qualified_classifiers;
mod sam;
pub(crate) use call_argument::CallArgKind;
pub(crate) use callable_shapes::{
    classifier_callable_signature, classifier_callable_signatures, declared_function_type,
};
pub(crate) use generic_inference::*;
pub(crate) use sam::{semantic_sam_signature, SamSignature};

/// The classifier whose declarations form an expression type's member scope. `Nothing` has no
/// instances of its own, but Kotlin still exposes the ordinary `Any` members on a bottom-typed
/// expression. This projection is only for dispatch-member lookup and applicability; extension
/// receivers continue to use the expression's actual type.
pub(crate) fn member_scope_receiver(receiver: Ty) -> Ty {
    match receiver {
        Ty::Nothing => Ty::obj_name(crate::types::wk::any()),
        Ty::TyParam(_, bound) => member_scope_receiver(*bound),
        _ => receiver,
    }
}

/// Lookup for a classifier inherited through a supertype's nested-class scope. Providers expose the
/// declaration record only; core applies structural nesting and access rules.
pub(crate) fn inherited_classifier_shape(
    source: &dyn SymbolSource,
    internal: TypeName,
    inheritor: TypeName,
) -> Option<std::sync::Arc<crate::libraries::LibraryType>> {
    use crate::libraries::ClassifierAccess;

    let classifier = source.classifier(internal)?;
    if !classifier.is_nested {
        return None;
    }
    let accessible = match classifier.access {
        ClassifierAccess::Public | ClassifierAccess::Protected => true,
        ClassifierAccess::Internal => classifier.source_file.is_some(),
        ClassifierAccess::PackagePrivate => {
            let package = internal.package();
            inheritor.package_matches(&package)
        }
        ClassifierAccess::Private => false,
    };
    accessible.then_some(classifier)
}

/// Direct applied supertypes derived from one classifier record. Providers publish symbolic templates;
/// core owns substitution and every transitive traversal.
pub(crate) fn direct_supertypes(source: &dyn SymbolSource, ty: Ty) -> Vec<Ty> {
    let Some(internal) = ty.kotlin_class_internal() else {
        return Vec::new();
    };
    let Some(classifier) = source.classifier(internal) else {
        return Vec::new();
    };
    direct_supertypes_from_classifier(&classifier, ty)
}

/// Applied semantic hierarchy in breadth-first order. Providers expose only direct declarations;
/// core owns the transitive walk and generic substitution for every declaration origin.
pub(crate) fn applied_hierarchy(source: &dyn SymbolSource, root: Ty) -> Vec<(TypeName, Ty, usize)> {
    let Some(internal) = root.kotlin_class_internal() else {
        return Vec::new();
    };
    let mut pending = std::collections::VecDeque::from([(internal, root, 0)]);
    let mut seen = std::collections::HashSet::new();
    let mut hierarchy = Vec::new();
    while let Some((owner, applied, depth)) = pending.pop_front() {
        if !seen.insert(owner) {
            continue;
        }
        hierarchy.push((owner, applied, depth));
        pending.extend(
            direct_supertypes(source, applied)
                .into_iter()
                .filter_map(|parent| Some((parent.kotlin_class_internal()?, parent, depth + 1))),
        );
    }
    hierarchy
}

/// Apply a runtime subtype named without source arguments to the generic arguments already known on
/// one of its supertypes. A typealias can expand that bare spelling to a star-applied semantic type,
/// so callers decide from syntax whether contextual recovery is allowed; this operation derives the
/// subtype application from its declaration regardless of that default expansion. A smart cast from
/// `Opt<T>` to source spelling `Sm` denotes `Sm<T>` when `Sm<X> : Opt<X>`; keeping `Sm` raw would
/// erase reads of `Sm.value` to `Any`.
pub(crate) fn apply_subtype_arguments_from_supertype(
    source: &dyn SymbolSource,
    subtype: Ty,
    supertype: Ty,
) -> Ty {
    if supertype.type_args().is_empty() {
        return subtype;
    }
    let Some(subtype_name) = subtype.kotlin_class_internal() else {
        return subtype;
    };
    let Some(classifier) = source.classifier(subtype_name) else {
        return subtype;
    };
    if classifier.type_params.is_empty() {
        return subtype;
    }
    let symbolic_args = classifier
        .type_params
        .iter()
        .enumerate()
        .map(|(index, name)| {
            Ty::ty_param(
                name,
                classifier
                    .type_param_bounds
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
            )
        })
        .collect::<Vec<_>>();
    let symbolic_subtype = Ty::obj_args_name(subtype_name, &symbolic_args);
    let Some(applied_supertype) = receiver_hierarchy(source, symbolic_subtype)
        .into_iter()
        .map(|(candidate, _)| candidate)
        .find(|candidate| candidate.kotlin_class_internal() == supertype.kotlin_class_internal())
    else {
        return subtype;
    };
    let mut bindings = GSigBinds::new();
    unify_ty_from_symbols(source, applied_supertype, supertype, &mut bindings);
    let arguments = symbolic_args
        .iter()
        .map(|argument| ty_subst_keep_unbound(*argument, &bindings))
        .collect::<Vec<_>>();
    if arguments.iter().zip(&classifier.type_params).all(
        |(argument, formal)| !matches!(argument, Ty::TyParam(identity, _) if identity == formal),
    ) {
        Ty::obj_args_name(subtype_name, &arguments)
    } else {
        subtype
    }
}

fn direct_supertypes_from_classifier(
    classifier: &crate::libraries::LibraryType,
    ty: Ty,
) -> Vec<Ty> {
    let bindings = classifier
        .type_params
        .iter()
        .cloned()
        .zip(
            ty.type_args()
                .iter()
                .copied()
                .chain(std::iter::repeat_with(|| Ty::obj("kotlin/Any"))),
        )
        .collect::<std::collections::HashMap<_, _>>();
    if classifier.supertype_templates.is_empty() {
        classifier.supertypes.iter_ids().map(Ty::obj_name).collect()
    } else {
        let applied = classifier
            .supertype_templates
            .iter()
            // A local/anonymous classifier's supertype may mention type variables owned by its
            // enclosing declaration. Apply only this classifier's arguments; erasing every other
            // symbolic variable loses the lexical type (`object : Converter<Box<T>, T>` became
            // `Converter<Box<Any>, Any>` during the hierarchy walk).
            .map(|supertype| ty_subst_keep_unbound(*supertype, &bindings))
            .collect::<Vec<_>>();
        crate::trace_compiler!(
            "supertype",
            "direct supertypes ty={ty:?} formals={:?} templates={:?} bindings={bindings:?} applied={applied:?}",
            classifier.type_params,
            classifier.supertype_templates,
        );
        applied
    }
}

/// The nearest companion instance contributed by a classifier receiver tower. Kotlin constructor
/// headers are static contexts: `this` denotes the constructed classifier's companion, or the first
/// companion on its superclass chain when that classifier has none (`EnumType` reaches
/// `kotlin.Enum.Companion`). Providers supply the companion declaration; core owns the hierarchy
/// walk and returns its semantic classifier identity without deriving target storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClassifierCompanionInstance {
    pub companion: TypeName,
}

pub(crate) fn classifier_companion_instance(
    source: &dyn SymbolSource,
    receiver: Ty,
) -> Option<ClassifierCompanionInstance> {
    let mut queue = std::collections::VecDeque::from([receiver]);
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = queue.pop_front() {
        let Some(owner) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(owner) {
            continue;
        }
        let classifier = source.classifier(owner)?;
        if let Some((_, companion)) = &classifier.companion_object {
            return Some(ClassifierCompanionInstance {
                companion: *companion,
            });
        }
        queue.extend(direct_supertypes(source, current));
    }
    None
}

/// Value receiver denoted by a classifier in expression/call position. An object denotes itself; a
/// class with a companion denotes that nested object. Absence means the classifier has no value facet.
pub(crate) fn classifier_value_receiver(
    source: &dyn SymbolSource,
    classifier: TypeName,
) -> Option<Ty> {
    let shape = source.classifier(classifier)?;
    if shape.is_object() {
        Some(Ty::obj_name(classifier))
    } else {
        shape
            .companion_object
            .as_ref()
            .map(|(_, companion)| Ty::obj_name(*companion))
    }
}

fn receiver_hierarchy(source: &dyn SymbolSource, receiver: Ty) -> Vec<(Ty, u32)> {
    let mut queue = std::collections::VecDeque::from([(receiver, 0)]);
    let mut seen = std::collections::HashSet::new();
    let mut hierarchy = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        let Some(internal) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(internal) {
            continue;
        }
        hierarchy.push((current, depth));
        queue.extend(
            direct_supertypes(source, current)
                .into_iter()
                .map(|supertype| (supertype, depth + 1)),
        );
    }
    hierarchy
}

pub(crate) fn classifier_bindings(
    classifier: &crate::libraries::LibraryType,
    receiver: Ty,
) -> std::collections::HashMap<String, Ty> {
    let raw_bindings = classifier
        .type_params
        .iter()
        .cloned()
        .zip(receiver.type_args().iter().copied())
        .collect::<std::collections::HashMap<_, _>>();

    classifier
        .type_params
        .iter()
        .cloned()
        .zip(
            receiver
                .type_args()
                .iter()
                .copied()
                .enumerate()
                .map(|(index, argument)| {
                    // Kotlin metadata records STAR without a nested type. Its readable type comes from
                    // the corresponding declaration bound, specialized by the complete application.
                    // Reconstruct it here, where declaration and application meet. This also handles
                    // dependent/F-bounds: `Rec<R, out T : Rec<R, T>>` applied as `Rec<*, *>` gives the
                    // second star the readable bound `Rec<*, *>`, rather than the decoder placeholder
                    // `Any?`.
                    let metadata_star_bound = Ty::nullable(Ty::obj("kotlin/Any"));
                    let argument = if matches!(argument, Ty::StarProjection(inner) if *inner == metadata_star_bound)
                    {
                        let upper_bound = classifier
                            .type_param_bounds()
                            .get(index)
                            .and_then(|bounds| bounds.first())
                            .copied()
                            .map(|bound| ty_subst_keep_unbound(bound, &raw_bindings))
                            .unwrap_or(metadata_star_bound);
                        Ty::star_projection(upper_bound)
                    } else {
                        argument
                    };

                    // Declaration-site variance makes the MATCHING use-site projection redundant:
                    // `interface List<out E>` means `List<out X>` — and so `List<*>` — simply is
                    // `List<X>`, so members see the plain argument and `List<*>.indexOf` takes
                    // `Any?`. An invariant classifier keeps the projection, which is what makes
                    // `MutableList<*>.add` collapse to `Nothing` and stay prohibited.
                    match (
                        classifier.type_param_variances.get(index).copied(),
                        argument,
                    ) {
                        (
                            Some(crate::types::TypeVariance::Out),
                            Ty::OutProjection(inner) | Ty::StarProjection(inner),
                        ) => *inner,
                        (Some(crate::types::TypeVariance::In), Ty::InProjection(inner)) => *inner,
                        _ => argument,
                    }
                }),
        )
        .collect()
}

/// The declared upper bound carried by each classifier type-parameter occurrence.
///
/// JVM `Signature` parsing discovers a class's formal declarations separately from a member's type
/// uses. Until those two records are joined, a `TT;` use carries only the parser's temporary `Any`
/// placeholder. Substitution must consume the declaration bound instead: in particular, an unqualified
/// Java `T extends Object` is platform-nullable, while Kotlin `T & Any` intentionally carries a
/// non-null occurrence bound.
fn classifier_type_parameter_bounds(
    classifier: &crate::libraries::LibraryType,
) -> std::collections::HashMap<String, Ty> {
    classifier
        .type_params
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let bound = classifier
                .type_param_bounds()
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            (formal.clone(), bound)
        })
        .collect()
}

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

fn specialize_member_type(
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

fn specialize_final_signature_output_type(
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

fn specialize_callable(
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
    callable.declared_ret = callable
        .declared_ret
        .map(|ty| specialize_member_type(source, ty, bindings, TypePosition::Out));
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

fn specialize_call_sig(
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
    use super::{specialize_member_type, GSigBinds, TypePosition};
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
}

fn declared_callables(
    source: &dyn SymbolSource,
    classifier: &crate::libraries::LibraryType,
    receiver: Ty,
    name: &str,
) -> Callables {
    let Some(callables) = classifier.declared_callables.get(name) else {
        return Callables::None;
    };
    specialize_declared_callables(source, classifier, receiver, callables.clone())
}

fn specialize_declared_callables(
    source: &dyn SymbolSource,
    classifier: &crate::libraries::LibraryType,
    receiver: Ty,
    callables: Callables,
) -> Callables {
    let base_bindings = classifier_bindings(classifier, receiver);
    let (mut functions, mut properties) = callables.into_parts();
    for function in &mut functions.overloads {
        let mut bindings = base_bindings.clone();
        if let Some(signature) = &function.generic_sig {
            for formal in &signature.formals {
                // A method formal always owns its name. In `class Box<T> { fun <T> echo(T): T }`,
                // the method's `T` shadows the receiver-bound class `T`; retaining the class binding
                // here would specialize `Box<String>.echo(42)` to `String` before overload inference.
                bindings.remove(formal);
            }
        }
        match function.kind {
            FnKind::Member => function.receiver = Some(receiver),
            FnKind::Extension => {
                function.receiver = function.receiver.map(|extension_receiver| {
                    specialize_member_type(
                        source,
                        extension_receiver,
                        &bindings,
                        TypePosition::Invariant,
                    )
                });
            }
            FnKind::TopLevel => {}
        }
        specialize_callable(source, &mut function.callable, &bindings);
        function.ret.class = function
            .ret
            .class
            .map(|ty| ty_subst_keep_unbound(ty, &bindings));
        specialize_call_sig(source, &mut function.call_sig, &bindings);
        if let Some(signature) = &mut function.generic_sig {
            let suspend_ret = function
                .flags
                .suspend
                .then(|| {
                    let continuation = *signature.params.last()?;
                    match continuation {
                        Ty::Obj(name, args)
                            if crate::types::same(name, crate::types::wk::continuation()) =>
                        {
                            args.first().copied()
                        }
                        _ => None,
                    }
                })
                .flatten();
            if let Some(suspend_ret) = suspend_ret {
                signature.params.pop();
                signature.ret = suspend_ret;
            }
            let declared_ret = signature.ret;
            signature.receiver = signature
                .receiver
                .map(|ty| specialize_member_type(source, ty, &bindings, TypePosition::Invariant));
            signature.params = signature
                .params
                .iter()
                .map(|ty| specialize_member_type(source, *ty, &bindings, TypePosition::In))
                .collect();
            signature.ret =
                specialize_member_type(source, signature.ret, &bindings, TypePosition::Out);
            if signature.ret != declared_ret {
                function.callable.ret = signature.ret;
            }
            for bounds in &mut signature.formal_bounds {
                for bound in bounds {
                    *bound = ty_subst_keep_unbound(*bound, &bindings);
                }
            }
        }
    }
    for property in &mut properties.overloads {
        let raw_owner_property = !classifier.type_params.is_empty()
            && receiver.type_args().is_empty()
            && property.ty.mentions_ty_param();
        let declared_nullability = property.ty;
        let mut bindings = base_bindings.clone();
        for formal in &property.formals {
            if !classifier.type_params.contains(formal) {
                bindings.remove(formal);
            }
        }
        match property.kind {
            PropKind::Member => property.receiver = Some(receiver),
            PropKind::MemberExtension | PropKind::Extension => {
                property.receiver = property.receiver.map(|extension_receiver| {
                    specialize_member_type(
                        source,
                        extension_receiver,
                        &bindings,
                        TypePosition::Invariant,
                    )
                });
            }
            PropKind::TopLevel => {}
        }
        property.ty = specialize_member_type(source, property.ty, &bindings, TypePosition::Out);
        specialize_callable(source, &mut property.getter, &bindings);
        property.getter.ret = property.ty;
        if let Some(setter) = &mut property.setter {
            specialize_callable(source, setter, &bindings);
        }
        if raw_owner_property {
            let erased = property.getter.physical_ret;
            let erased = match declared_nullability {
                Ty::Nullable(_) => Ty::nullable(erased.non_null()),
                Ty::PlatformNullable(_) => Ty::platform_nullable(erased.non_null()),
                _ => erased,
            };
            property.ty = erased;
            property.getter.ret = erased;
            if let Some(parameter) = property
                .setter
                .as_mut()
                .and_then(|setter| setter.params.last_mut())
            {
                *parameter = erased;
            }
        }
    }
    Callables::from_parts(functions, properties)
}

pub(crate) fn declared_member_callables(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
) -> Callables {
    let Some(classifier) = receiver
        .kotlin_class_internal()
        .and_then(|internal| source.classifier(internal))
    else {
        return Callables::None;
    };
    declared_callables(source, &classifier, receiver, name)
}

pub(crate) fn members_in_hierarchy(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
) -> Callables {
    // A function type carries its callable shape directly in `FnSig`; it is not named by deriving a
    // `FunctionN` classifier from the parameter count. For ordinary member lookup its declared
    // classifier is the arity-independent `Function<R>`, whose hierarchy supplies `Any` members.
    // `invoke` remains a member of the `FnSig` itself and is handled by the caller from that signature.
    let receiver = match receiver.non_null() {
        Ty::Fun(signature) => Ty::obj_args("kotlin/Function", &[signature.ret]),
        Ty::Unit => Ty::obj("kotlin/Unit"),
        Ty::Nothing => Ty::obj("kotlin/Nothing"),
        _ => receiver,
    };
    let mut functions = FunctionSet::default();
    let mut properties = PropertySet::default();
    let mut queue = std::collections::VecDeque::from([(receiver, 0)]);
    let mut seen = std::collections::HashSet::new();
    while let Some((current, depth)) = queue.pop_front() {
        let Some(internal) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(internal) {
            continue;
        }
        let Some(classifier) = source.classifier(internal) else {
            continue;
        };
        let (mut current_functions, mut current_properties) =
            declared_callables(source, &classifier, current, name).into_parts();
        crate::trace_compiler!(
            "resolve",
            "member hierarchy name={name} root={receiver:?} rung={depth} current={current:?} functions={:?}",
            current_functions
                .overloads
                .iter()
                .map(|function| (function.callable.params.as_slice(), function.callable.ret))
                .collect::<Vec<_>>(),
        );
        for function in &mut current_functions.overloads {
            function.receiver_rank += depth;
        }
        for property in &mut current_properties.overloads {
            property.receiver_rank += depth;
        }
        functions.overloads.extend(current_functions.overloads);
        properties.overloads.extend(current_properties.overloads);
        queue.extend(
            direct_supertypes_from_classifier(&classifier, current)
                .into_iter()
                .map(|supertype| (supertype, depth + 1)),
        );
    }

    // Kotlin operator conventions are inherited by an override even when the overriding declaration
    // does not repeat `operator` (`Comparable<T>.compareTo` is the common case). This is a relation
    // between declarations in the class model, so compute it here while the one core hierarchy is
    // available. Providers still report only their exact declaration flags.
    let declarations = functions
        .overloads
        .iter()
        .map(|function| {
            (
                function.receiver_rank,
                function.flags.operator,
                function.semantic_params().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    for function in &mut functions.overloads {
        if !function.flags.operator
            && declarations.iter().any(|(rank, operator, params)| {
                *operator
                    && *rank > function.receiver_rank
                    && params.as_slice() == &*function.semantic_params()
            })
        {
            function.flags.operator = true;
        }
    }
    inherit_overridden_default_arguments(source, &mut functions);
    retain_covariant_inherited_overrides(source, &mut functions);
    Callables::from_parts(functions, properties)
}

/// Publish inherited default-argument availability on the overriding declaration that remains the
/// semantic call target. Kotlin forbids repeating defaults on an override: a call through the
/// derived receiver selects the override's covariant result and parameter contract, while omitted
/// slots obtain their expressions from an overridden declaration. The provider coordinate is kept
/// as realization data; it must never replace the selected callable during overload resolution.
fn inherit_overridden_default_arguments(source: &dyn SymbolSource, functions: &mut FunctionSet) {
    let declarations = functions.overloads.clone();
    for implementation in &mut functions.overloads {
        let implementation_result = implementation.ret.apply(implementation.callable.ret);
        let implementation_parameters = implementation.semantic_params();
        let inherited = declarations
            .iter()
            .filter(|candidate| {
                candidate.receiver_rank > implementation.receiver_rank
                    && candidate.context_count == implementation.context_count
                    && candidate.semantic_params() == implementation_parameters
                    && resolution_subtype(
                        source,
                        implementation_result,
                        candidate.ret.apply(candidate.callable.ret),
                    )
            })
            .collect::<Vec<_>>();
        if inherited.is_empty() {
            continue;
        }
        let parameter_count = implementation.call_sig.param_defaults.len();
        for parameter in 0..parameter_count {
            if implementation.call_sig.param_defaults[parameter] {
                continue;
            }
            if inherited.iter().any(|candidate| {
                candidate
                    .call_sig
                    .param_defaults
                    .get(parameter)
                    .copied()
                    .unwrap_or(false)
            }) {
                implementation.call_sig.param_defaults[parameter] = true;
                if implementation
                    .default_values
                    .get(parameter)
                    .is_some_and(Option::is_none)
                {
                    if let Some(value) = inherited
                        .iter()
                        .find_map(|candidate| candidate.default_values.get(parameter))
                        .cloned()
                        .flatten()
                    {
                        implementation.default_values[parameter] = Some(value);
                    }
                }
            }
        }
        implementation.call_sig.required = crate::libraries::required_arity(
            parameter_count,
            &implementation.call_sig.param_defaults,
        );
        if implementation.callable.default_realization.is_none() {
            implementation.callable.default_realization = inherited
                .iter()
                .filter(|candidate| candidate.callable.default_realization.is_some())
                .min_by_key(|candidate| candidate.receiver_rank)
                .and_then(|candidate| candidate.callable.default_realization.clone());
        }
    }
}

/// Normalize the complete inherited member family of an explicitly imported object member into the
/// receiver-less import scope. The object remains the dispatch receiver on the selected callable;
/// an ordinary member consequently behaves as a top-level candidate at the use site, while a member
/// extension keeps its extension receiver and competes with other extensions normally.
///
/// Providers expose only declarations and direct supertypes. Keeping the hierarchy walk here avoids
/// making module and classpath sources independently manufacture inherited duplicates.
pub(crate) fn imported_object_member_symbols(
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &str,
) -> Option<std::rc::Rc<crate::libraries::ResolvedSymbols>> {
    let classifier = source.classifier(owner)?;
    if !classifier.is_object() {
        return None;
    }

    let singleton = crate::libraries::SingletonDispatch { classifier: owner };

    let (mut functions, mut properties) =
        members_in_hierarchy(source, Ty::obj_name(owner), name).into_parts();
    functions
        .overloads
        .retain_mut(|function| match function.kind {
            FnKind::Member => {
                function.kind = FnKind::TopLevel;
                function.receiver = None;
                function.callable.singleton_dispatch = Some(Box::new(singleton.clone()));
                true
            }
            FnKind::Extension => {
                function.callable.singleton_dispatch = Some(Box::new(singleton.clone()));
                true
            }
            FnKind::TopLevel => false,
        });
    properties
        .overloads
        .retain_mut(|property| match property.kind {
            PropKind::Member => {
                property.kind = PropKind::TopLevel;
                property.receiver = None;
                property.getter.singleton_dispatch = Some(Box::new(singleton.clone()));
                if let Some(setter) = &mut property.setter {
                    setter.singleton_dispatch = Some(Box::new(singleton.clone()));
                }
                true
            }
            PropKind::MemberExtension => {
                property.kind = PropKind::Extension;
                property.getter.singleton_dispatch = Some(Box::new(singleton.clone()));
                if let Some(setter) = &mut property.setter {
                    setter.singleton_dispatch = Some(Box::new(singleton.clone()));
                }
                true
            }
            PropKind::Extension => {
                property.getter.singleton_dispatch = Some(Box::new(singleton.clone()));
                if let Some(setter) = &mut property.setter {
                    setter.singleton_dispatch = Some(Box::new(singleton.clone()));
                }
                true
            }
            PropKind::TopLevel => false,
        });

    // Member extension FUNCTIONS are deliberately absent from ordinary dispatch lookup, while
    // extension properties already live in `declared_callables` as `MemberExtension`. Walk the same
    // applied hierarchy and recover the function declarations from the classifier's semantic member
    // table. This includes inherited declarations such as `object C : I<String>` importing an
    // `I`-declared extension, and specializes any owner type parameters before applicability runs.
    let mut extension_queue = std::collections::VecDeque::from([(Ty::obj_name(owner), 0)]);
    let mut extension_seen = std::collections::HashSet::new();
    while let Some((current, depth)) = extension_queue.pop_front() {
        let Some(current_owner) = current.kotlin_class_internal() else {
            continue;
        };
        if !extension_seen.insert(current_owner) {
            continue;
        }
        let Some(classifier) = source.classifier(current_owner) else {
            continue;
        };
        crate::trace_compiler!(
            "resolve",
            "object import extension hierarchy name={name} root={owner:?} rung={depth} current={current:?} declared={:?}",
            classifier
                .members
                .iter()
                .filter(|member| member.name == name)
                .map(|member| (member.name.as_str(), member.is_member_extension()))
                .collect::<Vec<_>>(),
        );
        let declared_extensions = classifier
            .members
            .iter()
            .filter(|member| member.name == name && member.is_member_extension())
            .cloned()
            .map(|member| {
                let receiver = member
                    .generic_sig
                    .as_ref()
                    .and_then(|signature| signature.receiver)
                    .or_else(|| member.params.get(member.context_count).copied());
                let mut function = crate::libraries::FunctionInfo::classifier_member(
                    FnKind::Extension,
                    current_owner,
                    member,
                );
                function.receiver = receiver;
                function.receiver_rank = depth;
                function.callable.singleton_dispatch = Some(Box::new(singleton.clone()));
                function
            })
            .collect::<Vec<_>>();
        if !declared_extensions.is_empty() {
            let specialized = specialize_declared_callables(
                source,
                &classifier,
                current,
                Callables::Functions(FunctionSet {
                    overloads: declared_extensions,
                }),
            );
            functions
                .overloads
                .extend(specialized.into_parts().0.overloads);
        }
        extension_queue.extend(
            direct_supertypes_from_classifier(&classifier, current)
                .into_iter()
                .map(|supertype| (supertype, depth + 1)),
        );
    }

    // Some providers keep member extensions out of ordinary dispatch-member lookup: they are not
    // callable through `object.extensionReceiver`, and therefore do not belong in the receiver's
    // member scope. They are nevertheless importable declarations of the object. Read that exact
    // classifier callable namespace as the second half of the import surface and merge only its
    // extensions; ordinary members already came from the hierarchy walk above (including inherited
    // declarations). Module sources may expose an extension through both paths, so compare stable or
    // physical declaration identities before appending it.
    let declared = source.symbols(SymbolNamespace::Classifier(owner), name);
    let (declared_functions, declared_properties) = declared.callables.clone().into_parts();
    for function in declared_functions.overloads {
        if function.kind != FnKind::Extension {
            continue;
        }
        let already_present = functions.overloads.iter().any(|existing| {
            existing.stable_declaration.is_some()
                && existing.stable_declaration == function.stable_declaration
                || existing.callable.external_identity.is_some()
                    && existing.callable.external_identity == function.callable.external_identity
                || existing.callable.owner == function.callable.owner
                    && existing.callable.name == function.callable.name
                    && existing.callable.descriptor == function.callable.descriptor
        });
        if !already_present {
            functions.overloads.push(function);
        }
    }
    for property in declared_properties.overloads {
        if property.kind != PropKind::Extension {
            continue;
        }
        let already_present = properties.overloads.iter().any(|existing| {
            existing.stable_declaration.is_some()
                && existing.stable_declaration == property.stable_declaration
                || existing.getter.external_identity.is_some()
                    && existing.getter.external_identity == property.getter.external_identity
                || existing.getter.owner == property.getter.owner
                    && existing.getter.name == property.getter.name
                    && existing.getter.descriptor == property.getter.descriptor
        });
        if !already_present {
            properties.overloads.push(property);
        }
    }

    let callables = Callables::from_parts(functions, properties);
    (!matches!(callables, Callables::None)).then(|| {
        std::rc::Rc::new(crate::libraries::ResolvedSymbols {
            callables,
            ..Default::default()
        })
    })
}

/// Collapse one inherited function slot when a diamond contributes covariant return declarations at
/// the same receiver rung. Kotlin treats `iterator(): MutableIterator<T>` as the override of
/// `iterator(): Iterator<T>` even when the two declarations arrive through sibling supertypes. They
/// are not overloads: equal value-parameter lists identify the same slot, and the strict return
/// subtype is its most-specific declaration. Incomparable returns remain separate so ordinary
/// overload/ambiguity diagnostics can reject an invalid hierarchy.
fn retain_covariant_inherited_overrides(source: &dyn SymbolSource, functions: &mut FunctionSet) {
    let mut retained: Vec<FunctionInfo> = Vec::with_capacity(functions.overloads.len());
    for candidate in functions.overloads.drain(..) {
        let candidate_ret = candidate.ret.apply(candidate.callable.ret);
        let mut candidate_is_shadowed = false;
        let mut shadowed = Vec::new();
        for (index, existing) in retained.iter().enumerate() {
            if existing.receiver_rank != candidate.receiver_rank
                || existing.context_count != candidate.context_count
                || existing.semantic_params() != candidate.semantic_params()
            {
                continue;
            }
            let existing_ret = existing.ret.apply(existing.callable.ret);
            let candidate_is_subtype = resolution_subtype(source, candidate_ret, existing_ret);
            let existing_is_subtype = resolution_subtype(source, existing_ret, candidate_ret);
            if candidate_is_subtype && !existing_is_subtype {
                shadowed.push(index);
            } else if existing_is_subtype && !candidate_is_subtype {
                candidate_is_shadowed = true;
                break;
            }
        }
        if candidate_is_shadowed {
            continue;
        }
        for index in shadowed.into_iter().rev() {
            retained.remove(index);
        }
        retained.push(candidate);
    }
    functions.overloads = retained;
}

/// Result of inherited nested-classifier lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InheritedNestedClassifier {
    NotFound,
    Found(TypeName),
    Ambiguous,
}

impl InheritedNestedClassifier {
    pub(crate) fn found(self) -> Option<TypeName> {
        match self {
            Self::Found(internal) => Some(internal),
            Self::NotFound | Self::Ambiguous => None,
        }
    }
}

/// Return a source class and its lexical owners, nearest first.
pub(crate) fn lexical_enclosing_classifier_names(
    owner: TypeName,
    mut classifier_exists: impl FnMut(TypeName) -> bool,
) -> Vec<TypeName> {
    let mut owners = Vec::new();
    let mut candidate = Some(owner);
    while let Some(internal) = candidate {
        if classifier_exists(internal) {
            owners.push(internal);
        }
        candidate = internal.nested_owner();
    }
    owners
}

pub(crate) fn inherited_nested_classifier_name(
    name: &str,
    roots: Vec<TypeName>,
    mut direct_supertypes: impl FnMut(TypeName) -> Vec<TypeName>,
    mut classifier_exists: impl FnMut(TypeName) -> bool,
) -> InheritedNestedClassifier {
    if name.contains(['.', '/', '$']) {
        return InheritedNestedClassifier::NotFound;
    }
    let mut level = roots;
    let mut seen = std::collections::HashSet::new();
    while !level.is_empty() {
        let mut matches = std::collections::HashSet::new();
        let mut next = Vec::new();
        for owner in level {
            if !seen.insert(owner) {
                continue;
            }
            let candidate = owner
                .existing_nested_child(name)
                .unwrap_or_else(|| crate::types::type_name_nested_child(owner, name));
            if classifier_exists(candidate) {
                matches.insert(candidate);
            }
            next.extend(direct_supertypes(owner));
        }
        match matches.len() {
            0 => level = next,
            1 => {
                return InheritedNestedClassifier::Found(
                    matches
                        .into_iter()
                        .next()
                        .expect("one inherited classifier"),
                )
            }
            _ => return InheritedNestedClassifier::Ambiguous,
        }
    }
    InheritedNestedClassifier::NotFound
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LambdaCallShape {
    /// Exact identities of the selected overload's generic formals. A postponed lambda frame may
    /// collect and suppress constraints only for these variables; symbolic types owned by an
    /// enclosing declaration remain fixed expectations.
    pub generic_formals: Vec<String>,
    /// Selected declaration parameters in source-argument order. This preserves the ordinary
    /// argument constraints that must be published before a later lambda is contextually checked,
    /// even when the selected callable comes from a provider rather than the source module.
    pub argument_parameters: Vec<Ty>,
    pub param_types: Option<Vec<Vec<Ty>>>,
    /// The selected callable parameter in source-argument order. Lambda checking consumes the
    /// decomposed inputs above; callable-reference adaptation needs the complete function type,
    /// including its return type, receiver bit, and still-unbound generic variables.
    pub expected_types: Option<Vec<Option<Ty>>>,
    /// Complete callable expectations whose result is fixed strongly enough to contextualize a
    /// lambda literal. Callable references may use the symbolic [`Self::expected_types`] above to
    /// select by input shape while contributing a result constraint; a lambda body must not be
    /// coerced to a widenable receiver lower bound before overload inference finishes.
    pub fixed_expected_types: Option<Vec<Option<Ty>>>,
    pub receivers: Option<Vec<Option<Ty>>>,
    pub context_counts: Option<Vec<usize>>,
    pub materialized: Option<Vec<bool>>,
    /// The selected callable permits its non-materialized lambda arguments to be spliced.
    pub inline: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct CallableImport {
    owner: SymbolNamespace,
    declared_name: String,
}

impl CallableImport {
    pub(crate) fn new(owner: SymbolNamespace, declared_name: String) -> Self {
        Self {
            owner,
            declared_name,
        }
    }
}

/// Name-aware import scope for unqualified top-level and extension callables.
#[derive(Clone, Debug)]
pub(crate) struct FunctionImportScope {
    explicit: std::collections::HashMap<String, CallableImport>,
    levels: [Vec<TypeName>; 4],
}

/// Classifier candidates contributed by one star-import precedence level. A star owner may denote
/// either a package (`import sample.*`) or a classifier (`import sample.Outer.*`); the retained level
/// stores its stable name, and the source's semantic facet decides which namespace it contributes.
/// Callables have their own namespace traversal, but classifier resolution in both signature and
/// body checking shares this operation so a compiled nested class cannot disappear between passes.
pub(crate) fn classifier_candidates_at_scope_level<S: SymbolSource + ?Sized>(
    source: &S,
    name: &str,
    owners: &[TypeName],
) -> Vec<TypeName> {
    let mut candidates = Vec::new();
    for &owner in owners {
        let candidate = if source.classifier(owner).is_some() {
            let nested = owner
                .existing_nested_child(name)
                .unwrap_or_else(|| crate::types::type_name_nested_child(owner, name));
            source
                .classifier(nested)
                .filter(|classifier| classifier.is_nested && nested.nested_owner() == Some(owner))
                .map(|_| nested)
        } else {
            source
                .symbols(SymbolNamespace::Package(owner), name)
                .classifier_name
        };
        if let Some(candidate) = candidate.filter(|candidate| !candidates.contains(candidate)) {
            candidates.push(candidate);
        }
    }
    candidates
}

impl FunctionImportScope {
    pub(crate) fn new(
        explicit: std::collections::HashMap<String, CallableImport>,
        levels: [Vec<TypeName>; 4],
    ) -> Self {
        Self { explicit, levels }
    }

    pub(crate) fn explicit_owner(&self, name: &str) -> Option<SymbolNamespace> {
        self.explicit
            .get(name)
            .map(|import| import.owner)
            .or_else(|| {
                name.strip_suffix("$default")
                    .and_then(|base| self.explicit.get(base).map(|import| import.owner))
            })
    }

    pub(crate) fn explicit_target(&self, name: &str) -> Option<(SymbolNamespace, String)> {
        if let Some(import) = self.explicit.get(name) {
            return Some((import.owner, import.declared_name.clone()));
        }
        let base = name.strip_suffix("$default")?;
        let import = self.explicit.get(base)?;
        Some((import.owner, format!("{}$default", import.declared_name)))
    }

    pub(crate) fn levels(&self) -> &[Vec<TypeName>; 4] {
        &self.levels
    }
}

pub(crate) type GSigBinds = std::collections::HashMap<String, Ty>;

/// [`crate::assignable::TypeOracle`] over a federated [`SymbolSource`] (module ∪ classpath): the class
/// hierarchy walk the one assignability relation needs. Kotlin-name supertypes, no JVM canonicalization —
/// source-type space, as `ReceiverMro` uses.
pub(crate) struct SourceOracle<'a>(pub &'a dyn SymbolSource);

impl crate::assignable::TypeOracle for SourceOracle<'_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        direct_supertypes(self.0, ty)
    }
    fn same_class_name(&self, a: TypeName, b: TypeName) -> bool {
        a == b
    }
    fn type_param_variance(&self, internal: TypeName, index: usize) -> crate::types::TypeVariance {
        self.0
            .classifier(internal)
            .and_then(|classifier| classifier.type_param_variances.get(index).copied())
            .unwrap_or_default()
    }
    fn type_param_upper_bounds(&self, internal: TypeName, index: usize) -> Vec<Ty> {
        self.0
            .classifier(internal)
            .and_then(|classifier| classifier.type_param_bounds.get(index).cloned())
            .filter(|bounds| !bounds.is_empty())
            .unwrap_or_else(|| vec![Ty::nullable(Ty::obj("kotlin/Any"))])
    }
}

pub(crate) use crate::types::{ty_subst, ty_subst_all, ty_subst_keep_unbound};

/// Specialize the selected member's lambda-parameter slots from concrete non-lambda arguments.
///
/// This is shared by Java SAM and Kotlin function-type expectations; naming it after either
/// origin would encourage the checker to grow parallel specialization paths for semantically
/// identical lambda arguments.
pub(crate) fn specialized_member_params(
    member: &LibraryMember,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Vec<Ty> {
    specialized_params(
        &member.params,
        member.generic_sig.as_ref(),
        Some(&member.call_sig),
        args,
        type_args,
    )
}

/// Specialize a function candidate's semantic parameters while leaving postponed lambda-owned
/// type variables unbound until the lambda body is checked. This is the same generic inference
/// operation member selection uses; signature-graph evaluation calls it after ordinary overload
/// selection instead of copying the resolver's temporary `Error` lambda probe into an expectation.
pub(crate) fn specialized_function_params(
    function: &FunctionInfo,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Vec<Ty> {
    let parameters = function.applied_params();
    specialized_params(
        parameters.as_ref(),
        function.generic_sig.as_ref(),
        Some(&function.call_sig),
        args,
        type_args,
    )
}

fn specialized_params(
    params: &[Ty],
    generic_sig: Option<&GenericSig>,
    call_sig: Option<&CallSig>,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Vec<Ty> {
    let Some(gsig) = generic_sig.filter(|sig| sig.params.len() == params.len()) else {
        return params.to_vec();
    };
    // Explicit call type arguments bind the formals before argument inference. `CallArgKind`
    // deliberately owns the syntactic lambda/literal provenance, so this generic specialization
    // never has to keep parallel boolean slices aligned with the argument types.
    let mut binds = seeded_gsig_binds(gsig, type_args);
    for (index, (&param, arg)) in gsig.params.iter().zip(args).enumerate() {
        if call_sig.is_some_and(|call_sig| !call_sig.parameter_contributes_to_inference(index)) {
            continue;
        }
        if !arg.is_lambda_literal() && !arg.is_omitted_default() {
            unify_inferred_ty(param, arg.type_for(param), &mut binds);
        }
    }
    let mut specialized = params.to_vec();
    for (index, parameter) in specialized.iter_mut().enumerate() {
        if args
            .get(index)
            .is_some_and(|argument| argument.is_lambda_literal())
        {
            *parameter = ty_subst_keep_unbound(gsig.params[index], &binds);
        }
    }
    specialized
}

fn specialized_constructor_params(
    src: &dyn SymbolSource,
    member: &LibraryMember,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Vec<Ty> {
    let Some(signature) = member
        .generic_sig
        .as_ref()
        .filter(|signature| signature.params.len() == member.params.len())
    else {
        return member.params.clone();
    };
    let mut bindings = seeded_gsig_binds(signature, type_args);
    for (&parameter, argument) in signature.params.iter().zip(args) {
        let materialized_lambda = argument.is_lambda_literal()
            && !argument.ty().mentions_error()
            && !argument.ty().mentions_pending();
        if (!argument.is_lambda_literal() || materialized_lambda) && !argument.is_omitted_default()
        {
            // Constructor classifier parameters collect lower bounds from every occurrence. A
            // first-wins equality binding turns `PairBox("x", null)` into two `String`
            // parameters and then rejects the nullable argument; the ordinary inference merge
            // correctly completes the shared `T` as `String?` before applicability is tested.
            // A lambda contributes only after compact checking has replaced its `Error` probe with
            // a complete function type. This lets sibling lambdas constrain one another without
            // allowing an untyped lambda placeholder to decide constructor inference.
            unify_inferred_ty_with_source(
                src,
                parameter,
                argument.type_for(parameter),
                &mut bindings,
            );
        }
    }
    signature
        .params
        .iter()
        // A postponed lambda may be the only source for a classifier type argument. Preserve its
        // still-unbound formal in the contextual function shape; erasing it to the upper bound here
        // coerces the lambda result to `Any` before the checked call can contribute its constraint.
        .map(|parameter| ty_subst_keep_unbound(*parameter, &bindings))
        .collect()
}

/// Seed a declaration's formals from explicit call-site type arguments. All inference channels use
/// this operation before receiver/argument unification so written arguments remain authoritative and
/// member, extension, static, and lambda-shape probes cannot drift into different binding rules.
pub(crate) fn seeded_gsig_binds(gsig: &GenericSig, type_args: &[Ty]) -> GSigBinds {
    gsig.formals
        .iter()
        .cloned()
        .zip(type_args.iter().copied())
        .filter(|(_, argument)| *argument != Ty::Error)
        .collect()
}

/// Kotlin's internal `@Exact` is a declaration-site applicability constraint. After the callable's
/// type parameters are fixed (explicitly, or inferred from this call), an annotated parameter admits
/// only the same semantic type—not an ordinary subtype.
fn top_level_exact_parameters_admit(
    candidate: &FunctionInfo,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> bool {
    if !candidate.call_sig.exact_params.iter().any(|exact| *exact) {
        return true;
    }
    let signature = candidate.semantic_signature();
    let mut bindings = seeded_gsig_binds(&signature, type_args);
    if type_args.is_empty() {
        for (index, (&parameter, argument)) in signature
            .params
            .iter()
            .skip(candidate.context_count)
            .zip(args)
            .enumerate()
        {
            if !candidate
                .call_sig
                .parameter_contributes_to_inference(index + candidate.context_count)
            {
                continue;
            }
            if argument.is_omitted_default() {
                continue;
            }
            unify_inferred_ty(parameter, argument.ty(), &mut bindings);
        }
    }
    signature
        .params
        .iter()
        .skip(candidate.context_count)
        .zip(args)
        .enumerate()
        .all(|(index, (&parameter, argument))| {
            if argument.is_omitted_default() {
                return true;
            }
            candidate.call_sig.parameter_admits(
                index + candidate.context_count,
                ty_subst(parameter, &bindings),
                argument.ty(),
            )
        })
}

fn bind_member_return(
    source: &dyn SymbolSource,
    gsig: &GenericSig,
    receiver: Ty,
    args: &[Ty],
    type_args: &[Ty],
    provider_ret: Ty,
) -> Ty {
    let args = args
        .iter()
        .copied()
        .map(CallArgKind::Typed)
        .collect::<Vec<_>>();
    bind_member_return_from_call_args(
        source,
        gsig,
        receiver,
        &args,
        type_args,
        None,
        &[],
        provider_ret,
    )
}

fn bind_member_return_from_call_args(
    source: &dyn SymbolSource,
    gsig: &GenericSig,
    receiver: Ty,
    args: &[CallArgKind],
    type_args: &[Ty],
    vararg_index: Option<usize>,
    no_infer_params: &[bool],
    provider_ret: Ty,
) -> Ty {
    let mut binds = seeded_gsig_binds(gsig, type_args);
    if let Ty::Obj(owner, arguments) = receiver.non_null() {
        if let Some(classifier) = source.classifier(owner) {
            for (formal, argument) in classifier.type_params.iter().zip(arguments) {
                if !gsig.formals.iter().any(|method| method == formal) {
                    binds.entry(formal.clone()).or_insert(*argument);
                }
            }
        }
    }
    if let Some(declared_receiver) = gsig.receiver {
        unify_ty_from_symbols(source, declared_receiver, receiver, &mut binds);
        preserve_receiver_identity_bindings(declared_receiver, receiver, &mut binds);
    } else {
        seed_undeclared_return_bindings(gsig.ret, provider_ret, &gsig.formals, &mut binds);
    }
    let receiver_bindings = binds.clone();
    let inferred = infer_generic_call_bindings_from_symbols(
        source,
        gsig,
        gsig.params.iter().zip(args).enumerate().filter_map(
            |(parameter, (&declared, argument))| {
                (!no_infer_params.get(parameter).copied().unwrap_or(false)
                    && !argument.is_expected_type_callable()
                    && !argument.is_omitted_default())
                .then_some((
                    parameter,
                    argument.inference_type(source, declared),
                    argument.is_spread(),
                ))
            },
        ),
        vararg_index,
    );
    merge_call_argument_bindings(
        source,
        gsig,
        type_args,
        &receiver_bindings,
        &mut binds,
        inferred,
    );
    crate::trace_compiler!(
        "expected_call",
        "bind member return declared={:?} provider={provider_ret:?} bindings={binds:?}",
        gsig.ret,
    );
    let ret = instantiate_slot(
        source,
        Some(gsig),
        gsig.ret,
        &binds,
        TypePosition::Out,
        UnboundSpecialization::UseUpperBound,
    );
    // A direct METHOD-owned return variable specializes to the inferred argument even when its
    // erased provider return is a non-top upper bound (`<A : Annotation> … : A` physically returns
    // `Annotation`). Owner variables are deliberately excluded: their provider-specialized return
    // is the receiver binding that `seed_undeclared_return_bindings` recovered above.
    let direct_method_return = gsig
        .ret
        .non_null()
        .ty_param_name()
        .is_some_and(|name| gsig.formals.iter().any(|formal| formal == name));
    if direct_method_return {
        ret
    } else {
        merge_specialized_return(provider_ret, ret)
    }
}

/// Receiver substitution must preserve a caller's still-symbolic type parameter. General call
/// inference intentionally ignores `T = T`, but a selected member on `Owner<T>` returning that same
/// `T` must not erase it to its upper bound while the enclosing generic declaration is being checked.
fn preserve_receiver_identity_bindings(declared: Ty, actual: Ty, bindings: &mut GSigBinds) {
    match (declared.non_null(), actual.non_null()) {
        (Ty::TyParam(declared, _), actual @ Ty::TyParam(found, _)) if declared == found => {
            bindings.entry(declared.to_string()).or_insert(actual);
        }
        (Ty::Obj(declared, declared_args), Ty::Obj(found, actual_args)) if declared == found => {
            for (&declared, &actual) in declared_args.iter().zip(actual_args) {
                preserve_receiver_identity_bindings(declared, actual, bindings);
            }
        }
        (Ty::InProjection(declared), Ty::InProjection(actual))
        | (Ty::OutProjection(declared), Ty::OutProjection(actual)) => {
            preserve_receiver_identity_bindings(*declared, *actual, bindings);
        }
        _ => {}
    }
}

fn specialize_property(
    source: &dyn SymbolSource,
    mut property: PropertyInfo,
    receiver: Ty,
) -> PropertyInfo {
    let generic = property.getter.generic_sig.clone();
    let mut binds = GSigBinds::new();
    if let Some(declared_receiver) = property.receiver {
        unify_ty_from_symbols(source, declared_receiver, receiver, &mut binds);
    }
    property.receiver = property.receiver.map(|declared| {
        instantiate_slot(
            source,
            generic.as_deref(),
            declared,
            &binds,
            TypePosition::In,
            UnboundSpecialization::UseUpperBound,
        )
    });
    property.ty = instantiate_slot(
        source,
        generic.as_deref(),
        property.ty,
        &binds,
        TypePosition::Out,
        UnboundSpecialization::UseUpperBound,
    );
    property.getter.params = property
        .getter
        .params
        .iter()
        .map(|parameter| {
            instantiate_slot(
                source,
                generic.as_deref(),
                *parameter,
                &binds,
                TypePosition::In,
                UnboundSpecialization::UseUpperBound,
            )
        })
        .collect();
    property.getter.ret = property.ty;
    if let Some(setter) = property.setter.as_mut() {
        setter.params = setter
            .params
            .iter()
            .map(|ty| {
                instantiate_slot(
                    source,
                    generic.as_deref(),
                    *ty,
                    &binds,
                    TypePosition::In,
                    UnboundSpecialization::UseUpperBound,
                )
            })
            .collect();
    }
    property
}

/// Declaration-site specificity for equal receiver-MRO extension-property candidates. Receiver
/// inference establishes applicability first; this comparison then prefers the declaration whose
/// generic bounds form a strict subtype of the other's after alpha-renaming its formals.
fn generic_property_more_specific(
    source: &dyn SymbolSource,
    left: &PropertyInfo,
    right: &PropertyInfo,
) -> bool {
    let (Some(left), Some(right)) = (
        left.getter.generic_sig.as_deref(),
        right.getter.generic_sig.as_deref(),
    ) else {
        return false;
    };
    if left.formals.len() != right.formals.len()
        || left.formal_bounds.len() != right.formal_bounds.len()
    {
        return false;
    }
    let rename = left
        .formals
        .iter()
        .zip(&right.formals)
        .enumerate()
        .map(|(index, (left_formal, right_formal))| {
            let bound = right
                .formal_bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            (left_formal.clone(), Ty::ty_param(right_formal, bound))
        })
        .collect::<GSigBinds>();
    if left
        .receiver
        .map(|receiver| ty_subst_keep_unbound(receiver, &rename))
        != right.receiver
    {
        return false;
    }
    let left_bounds = left
        .formal_bounds
        .iter()
        .map(|bounds| {
            bounds
                .iter()
                .copied()
                .map(|bound| ty_subst_keep_unbound(bound, &rename))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let left_at_least_as_specific =
        left_bounds
            .iter()
            .zip(&right.formal_bounds)
            .all(|(left_bounds, right_bounds)| {
                right_bounds.iter().all(|right_bound| {
                    left_bounds
                        .iter()
                        .any(|left_bound| resolution_subtype(source, *left_bound, *right_bound))
                })
            });
    let right_at_least_as_specific =
        left_bounds
            .iter()
            .zip(&right.formal_bounds)
            .all(|(left_bounds, right_bounds)| {
                left_bounds.iter().all(|left_bound| {
                    right_bounds
                        .iter()
                        .any(|right_bound| resolution_subtype(source, *right_bound, *left_bound))
                })
            });
    left_at_least_as_specific && !right_at_least_as_specific
}

/// Extract the property half of a namespace record. The namespace arrives behind a shared memo handle, so
/// candidates are cloned into the selection's owned working set; both top-level and extension-property
/// selection consume this exact helper, preventing their `Properties`/`Both` handling from drifting when
/// [`crate::libraries::Callables`] gains another mixed shape.
fn property_overloads(callables: &crate::libraries::Callables) -> Vec<PropertyInfo> {
    match callables {
        crate::libraries::Callables::Properties(properties)
        | crate::libraries::Callables::Both { properties, .. } => properties.overloads.clone(),
        crate::libraries::Callables::None | crate::libraries::Callables::Functions(_) => Vec::new(),
    }
}

/// Source visibility for package-level properties, applied BEFORE ambiguity/receiver ranking. A JVM
/// `internal`/private declaration may still have a public bytecode accessor, so accessor flags alone must
/// never make it callable from another module. Module-origin facts remain governed by the module's
/// file-aware [`SymbolSource`] overlay (which admits same-file private and hides sibling private); applying
/// a second, file-blind filter here would incorrectly reject the former.
fn source_property_visible(platform: &dyn SemanticPlatform, property: &PropertyInfo) -> bool {
    property.visibility == Visibility::Public
        || matches!(property.getter.origin, Origin::Module { .. })
        || (property.visibility == Visibility::Internal
            && platform.internal_accessible(property.owner))
}

fn bind_ext_ret(
    source: &dyn SymbolSource,
    gsig: &GenericSig,
    receiver: Ty,
    args: &[Ty],
    targs: &[Ty],
) -> Ty {
    bind_ext_ret_tracking(source, gsig, receiver, args, targs).0
}

/// [`bind_ext_ret`] plus the bindings it made. A caller that reports the result as a TYPE — rather
/// than emitting a call with it — needs to know whether the arguments actually determined the
/// variables: an unbound one silently specializes to its bound, and `Any` is indistinguishable from
/// a legitimately-inferred `Any` once the binding is discarded.
fn bind_ext_ret_tracking(
    source: &dyn SymbolSource,
    gsig: &GenericSig,
    receiver: Ty,
    args: &[Ty],
    targs: &[Ty],
) -> (Ty, GSigBinds) {
    let mut binds = seeded_gsig_binds(gsig, targs);
    if let Some(recv_sig) = gsig.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
        // A selected extension on `Owner<T>` must preserve a caller-owned symbolic `T` in its
        // result just like a selected member does. General unification intentionally ignores the
        // identity pair so later concrete evidence can still solve it; at final return binding,
        // however, leaving it absent erases `T` to its `Any?` bound (`Map.Entry<K, V>.component1()`
        // inside a postponed `buildMap<K, V>` lambda then incorrectly returns `Any`).
        preserve_receiver_identity_bindings(recv_sig, receiver, &mut binds);
    }
    for (ps, a) in gsig.params.iter().zip(args.iter().copied()) {
        unify_ty(*ps, a, &mut binds);
    }
    complete_bottom_constraint_bindings(gsig, &mut binds, targs.len());
    // A projection is a generic-argument constraint, never an expression value. Consume it while
    // materializing the selected callable's output: `Iterable<out Range>.first()` returns `Range`,
    // not the invalid top-level type `out Range`.
    let ret = specialize_final_signature_output_type(source, gsig.ret, &binds);
    (ret, binds)
}

/// Whether the receiver and arguments PIN every type variable the signature declares to one
/// concrete type.
///
/// Presence in the binding map is not the question. A variable can be "bound" to ITSELF — a `T?`
/// receiver unified against a `T?` declaration self-binds — and it can be bound TWICE to types that
/// disagree (`fun <T> Src.pick(a: T, b: T)` called with a `String` and an `Int`), where the first
/// argument wins and the join is never taken. Both produce a confident-looking type that the full
/// checker will not agree with, and a caller reporting one as a property's inferred type makes the
/// compiler contradict itself. Emission is unaffected either way: an unbound variable erases to its
/// bound, which is exactly what the call site emits.
fn extension_bindings_are_determinate(
    semantic: &GenericSig,
    receiver: Ty,
    args: &[Ty],
    binds: &GSigBinds,
) -> bool {
    // Deliberately STRUCTURAL: `unify_ty` without a source performs no hierarchy walk and no SAM
    // conversion, so a constraint that needs one (an `Iterable<T>` parameter answered by a `List<
    // String>`) simply does not bind here and the call reports nothing. That is the safe direction —
    // it costs an inferred type, never an incorrect one.
    let names_a_variable = |ty: Ty| crate::types::ty_mentions_any_param(ty);
    if semantic.formals.iter().any(|formal| {
        binds
            .get(formal.as_str())
            .is_none_or(|&bound| names_a_variable(bound))
    }) {
        return false;
    }
    // Unify each position on its own: a formal reached from two positions must reach the same type
    // from both, which the accumulated map cannot show once the first binding has taken.
    let mut settled: GSigBinds = GSigBinds::new();
    let positions = semantic
        .receiver
        .map(|shape| (shape, receiver))
        .into_iter()
        .chain(semantic.params.iter().copied().zip(args.iter().copied()));
    for (shape, actual) in positions {
        let mut one = GSigBinds::new();
        unify_ty(shape, actual, &mut one);
        for (formal, bound) in one {
            if let Some(&previous) = settled.get(formal.as_str()) {
                if previous != bound {
                    return false;
                }
            } else {
                settled.insert(formal, bound);
            }
        }
    }
    true
}

fn specialized_extension_return(lib: &dyn SemanticPlatform, o: &FunctionInfo, inferred: Ty) -> Ty {
    let provider = o.ret.apply(o.callable.ret);
    let Some(signature) = o.generic_sig.as_ref() else {
        return o.ret.apply(inferred);
    };
    let inferred = signature.apply_return_policy(lib, inferred);
    let direct_method_return = signature
        .ret
        .non_null()
        .ty_param_name()
        .is_some_and(|name| signature.formals.iter().any(|formal| formal == name));
    if direct_method_return {
        inferred
    } else {
        merge_specialized_return(provider, inferred)
    }
}

fn bind_defaulted_ext_ret(
    source: &dyn SymbolSource,
    o: &FunctionInfo,
    receiver: Ty,
    args: &[Ty],
    targs: &[Ty],
    trailing_lambda: bool,
) -> Ty {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, targs);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    if trailing_lambda {
        let prefix = args.len().saturating_sub(1);
        for (index, (ps, a)) in semantic.params.iter().take(prefix).zip(args).enumerate() {
            if !o.call_sig.parameter_contributes_to_inference(index) {
                continue;
            }
            unify_ty(*ps, *a, &mut binds);
        }
        if let (Some(ls), Some(la)) = (semantic.params.last(), args.last()) {
            let index = semantic.params.len().saturating_sub(1);
            if o.call_sig.parameter_contributes_to_inference(index) {
                unify_ty(*ls, *la, &mut binds);
            }
        }
    } else {
        for (index, (ps, a)) in semantic.params.iter().zip(args).enumerate() {
            if !o.call_sig.parameter_contributes_to_inference(index) {
                continue;
            }
            unify_ty(*ps, *a, &mut binds);
        }
    }
    specialize_final_signature_output_type(source, semantic.ret, &binds)
}

fn bind_defaulted_ext_ret_slots(
    source: &dyn SymbolSource,
    o: &FunctionInfo,
    receiver: Ty,
    slots: &[Option<Ty>],
    targs: &[Ty],
) -> Ty {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, targs);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    for (index, (ps, slot)) in semantic.params.iter().zip(slots).enumerate() {
        if !o.call_sig.parameter_contributes_to_inference(index) {
            continue;
        }
        if let Some(arg) = slot {
            unify_ty(*ps, *arg, &mut binds);
        }
    }
    specialize_final_signature_output_type(source, semantic.ret, &binds)
}

/// If `sig` is a function type, the partially substituted semantic types of its lambda parameters.
/// Unbound formals remain symbolic: this helper shapes a postponed lambda before its body contributes
/// inference constraints, so erasing `T` to its bound here would turn real evidence from `it` into
/// `Any` before overload selection runs. Empty for anything else.
pub(crate) fn function_input_types(
    source: &dyn SymbolSource,
    sig: Ty,
    binds: &GSigBinds,
) -> Vec<Ty> {
    match sig.non_null() {
        Ty::Fun(fsig) => fsig
            .params
            .iter()
            // A lambda PARAMETER is an ordinary value slot: a formal bound to a projection by a
            // projected receiver shapes `it` as the approximation, never as `out X` itself.
            .map(|parameter| {
                instantiate_slot(
                    source,
                    None,
                    *parameter,
                    binds,
                    TypePosition::Out,
                    UnboundSpecialization::Preserve,
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether argument `a` can be passed where parameter `p` is expected, in erased Kotlin terms: an
/// exact match, any argument into an erased `Any` parameter, or the *same erased class* (a parameter
/// `Pair` accepts an argument `Pair<Int, String>` — generic parameters erase to the raw type).
pub(crate) fn arg_fits(p: &Ty, a: &Ty) -> bool {
    // A lambda value fits a function-typed parameter when arities agree; its body result is handled by
    // the selected call's generic binding, not by erased descriptor matching. An erased `Any` parameter —
    // whether spelled `kotlin/Any` or its JVM form `java/lang/Object` (a generic vararg element erases to
    // it) — accepts any reference argument.
    p == a
        || matches!(p, Ty::Obj(n, _)
            if (crate::types::same(*n, crate::types::wk::any())
                || crate::types::same(*n, crate::types::wk::java_object()))
                && !matches!(a, Ty::Null | Ty::Nullable(_)))
        || matches!((p.fun_arity(), a.fun_arity()), (Some(pn), Some(an)) if pn == an)
        || matches!((p, a), (Ty::Obj(pi, _), Ty::Obj(ai, _)) if pi == ai)
}

/// Whether a function-shaped argument can adapt to a functional-interface parameter.
///
/// `arg` is sometimes an unchecked lambda probe rather than a completed function type. Keeping that
/// syntax state in [`CallArgKind`] at callers, then reducing it to `Ty::Error` here, lets every call
/// origin use the same arity rule without teaching this semantic operation about AST forms.
pub(crate) fn sam_arg_matches(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    param: Ty,
    arg: Ty,
) -> bool {
    let Some(sam) = semantic_sam_signature(src, param) else {
        return false;
    };
    if arg == Ty::Error {
        return sam.params.len() <= 1;
    }
    let Some(arity) = arg.fun_arity() else {
        return false;
    };
    // A checked lambda has an authoritative arity and must match exactly. An unchecked literal uses
    // the `Error` probe above; its selected call path performs the first body check and may synthesize
    // implicit `it` for a one-parameter SAM. Keeping those states distinct avoids syntax-specific
    // exceptions for call paths that happened to check a lambda too early.
    if sam.params.len() != usize::from(arity) {
        return false;
    }
    let Some(arg_ret) = arg.fun_ret() else {
        return false;
    };
    sam_return_matches(lib, src, sam.ret, arg_ret)
}

pub(crate) fn sam_return_matches(
    _lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    expected: Ty,
    actual: Ty,
) -> bool {
    if expected == Ty::Unit || matches!(actual, Ty::Error | Ty::Nothing) {
        return true;
    }
    // A method-owned result formal is intentionally still open while overload applicability is
    // decided. Its lambda result constrains that formal after selection; at this stage only the
    // declared upper bound can reject the candidate.
    let expected = match expected {
        Ty::TyParam(_, bound) => *bound,
        expected => expected,
    };
    semantic_arg_assignable(src, &expected, &actual)
}

fn arg_fits_platform(lib: &dyn SemanticPlatform, param: &Ty, arg: &Ty) -> bool {
    arg_fits(param, arg)
        || param
            .fun_arity()
            .zip(lib.function_like_arity(*arg))
            .is_some_and(|(p, a)| usize::from(p) == a)
}

fn arg_fits_source(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    param: &Ty,
    arg: &Ty,
) -> bool {
    arg_fits_platform(lib, param, arg)
        || semantic_arg_assignable(src, param, arg)
        // An unresolved type parameter is an inference slot during candidate collection. Keep the
        // candidate so `null` can either infer `Nothing?` from a callable-owned formal or reach the
        // ordinary checker, which rejects a fixed enclosing bare `T` with the precise nullability
        // diagnostic. This is not assignability: [`semantic_arg_assignable`] remains strict.
        || (*arg == Ty::Null && matches!(param.non_null(), Ty::TyParam(..)))
}

/// Whether a parameter can host a function value at all — the applicability test for an implicitly
/// typed lambda literal argument. A bare `{ … }` is typed only against an expected type (its
/// recorded type until then is `Ty::Error`), so overload pruning must go by the parameter's SHAPE:
/// kotlinc counts such a lambda as applicable only to function-typed, `kotlin.Function*`, SAM,
/// type-parameter, or `Any` parameters — the same doctrine as javac, where an implicitly typed
/// lambda is not pertinent to applicability against non-functional parameters (JLS 15.12.2.2).
/// Probed against kotlinc 2.4.10: `(f: () -> Unit)` vs `(name: String)` resolves to the function
/// overload; vs `(x: Any)` the function overload wins on specificity. Without this test the
/// lambda's placeholder `Ty::Error` is wildcard-assignable to EVERY parameter, and a resolvable
/// overload pair reads as ambiguous.
fn untyped_lambda_pertinent(lib: &dyn SemanticPlatform, src: &dyn SymbolSource, param: Ty) -> bool {
    let shape = param.non_null();
    shape.fun_arity().is_some()
        || matches!(shape, Ty::TyParam(..))
        || shape.is_erased_top()
        // A bare lambda with no explicit parameters can be zero-arity or use implicit `it`.
        // Recognize erased/marker function supertypes through the provider's declared hierarchy;
        // a classifier merely named `Function...` carries no callable semantics.
        || (0..=1)
            .filter_map(|arity| lib.function_type(arity))
            .any(|function| semantic_arg_assignable(src, &shape, &function))
        || sam_arg_matches(lib, src, param, Ty::Error)
}

pub(crate) fn resolution_subtype(src: &dyn SymbolSource, sub: Ty, sup: Ty) -> bool {
    crate::assignable::is_subtype(
        &crate::assignable::TyCtx::new(),
        &SourceOracle(src),
        sub,
        sup,
    )
}

pub(crate) enum CandidateSelection<T> {
    None,
    Selected(T),
    Ambiguous,
}

fn unique_most_specific<T>(
    candidates: impl IntoIterator<Item = (Vec<Ty>, T)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelection<T> {
    unique_most_specific_with_conflicts(candidates, at_least_as_specific, |_, _| false)
}

/// Select the unique most-specific candidate.
fn unique_most_specific_with_conflicts<T>(
    candidates: impl IntoIterator<Item = (Vec<Ty>, T)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
    equivalent_conflicts: impl Fn(&T, &T) -> bool,
) -> CandidateSelection<T> {
    let mut applicable = Vec::new();
    for (params, candidate) in candidates {
        let equivalent =
            applicable.iter().find(|(existing, _): &&(Vec<Ty>, T)| {
                existing.len() == params.len()
                    && existing.iter().zip(&params).enumerate().all(
                        |(position, (&left, &right))| {
                            at_least_as_specific(position, left, right)
                                && at_least_as_specific(position, right, left)
                        },
                    )
            });
        if let Some((_, existing_candidate)) = equivalent {
            if !equivalent_conflicts(existing_candidate, &candidate) {
                continue;
            }
        }
        applicable.push((params, candidate));
    }
    if applicable.is_empty() {
        return CandidateSelection::None;
    }

    let mut selected = None;
    for (index, (params, _)) in applicable.iter().enumerate() {
        let dominated =
            applicable
                .iter()
                .enumerate()
                .any(|(other_index, (other, _))| {
                    index != other_index
                        && other.len() == params.len()
                        && other.iter().zip(params).enumerate().all(
                            |(position, (&left, &right))| {
                                at_least_as_specific(position, left, right)
                            },
                        )
                        && !params.iter().zip(other).enumerate().all(
                            |(position, (&left, &right))| {
                                at_least_as_specific(position, left, right)
                            },
                        )
                });
        if !dominated && selected.replace(index).is_some() {
            return CandidateSelection::Ambiguous;
        }
    }

    let Some(selected) = selected else {
        return CandidateSelection::Ambiguous;
    };
    CandidateSelection::Selected(applicable.swap_remove(selected).1)
}

fn fixed_parameter_shape(
    params: &[Ty],
    args: &[CallArgKind],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
) -> Option<Vec<Ty>> {
    (params.len() == args.len()
        && params
            .iter()
            .zip(args)
            .enumerate()
            .all(|(i, (param, arg))| fits(i, param, arg)))
    .then(|| params.to_vec())
}

fn omitted_parameter_shape(
    params: &[Ty],
    args: &[CallArgKind],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
) -> Option<Vec<Ty>> {
    (params.len() > args.len()
        && params[..args.len()]
            .iter()
            .zip(args)
            .enumerate()
            .all(|(i, (param, arg))| fits(i, param, arg)))
    .then(|| params.to_vec())
}

fn vararg_parameter_shape(
    params: &[Ty],
    args: &[CallArgKind],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
) -> Option<Vec<Ty>> {
    let vararg_index = params.len().checked_sub(1)?;
    vararg_parameter_shape_at(params, args, vararg_index, &[], fits)
}

/// Expand positional element-form arguments at an explicitly declared vararg slot. Parameters
/// after a non-final vararg cannot consume positional arguments in Kotlin; they must be named or
/// defaulted, so this type-only selector admits the shape only when every trailing parameter has
/// a default. The returned shape is parallel to the provided arguments for specificity ranking.
fn vararg_parameter_shape_at(
    params: &[Ty],
    args: &[CallArgKind],
    vararg_index: usize,
    param_defaults: &[bool],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
) -> Option<Vec<Ty>> {
    let array = *params.get(vararg_index)?;
    let element = array.array_elem()?;
    if args.len() == vararg_index + 1
        && args.get(vararg_index).map(|argument| argument.ty()) == Some(array)
    {
        return None;
    }
    // At the vararg slot a plain argument fits the ELEMENT type, while a SPREAD (`*xs`) fits the
    // ARRAY type — Kotlin allows both, mixed in any order (`f("a", *xs)`).
    let vararg_expected =
        |argument: &CallArgKind| if argument.is_spread() { array } else { element };
    if args.len() < vararg_index
        || params[..vararg_index]
            .iter()
            .zip(args)
            .enumerate()
            .any(|(position, (parameter, argument))| !fits(position, parameter, argument))
        || (vararg_index + 1..params.len())
            .any(|position| !param_defaults.get(position).copied().unwrap_or(false))
        || args[vararg_index..]
            .iter()
            .enumerate()
            .any(|(offset, argument)| {
                !fits(vararg_index + offset, &vararg_expected(argument), argument)
            })
    {
        return None;
    }
    let mut expanded = params[..vararg_index].to_vec();
    for argument in &args[vararg_index.min(args.len())..] {
        expanded.push(vararg_expected(argument));
    }
    Some(expanded)
}

fn integer_literal_call_applies(
    params: &[Ty],
    args: &[CallArgKind],
    mut fits: impl FnMut(usize, &Ty, &CallArgKind) -> bool,
) -> Option<bool> {
    if params.len() != args.len() {
        return None;
    }
    params
        .iter()
        .zip(args)
        .enumerate()
        .try_fold(false, |adapted, (i, (&param, arg))| {
            if param == arg.ty() {
                Some(adapted)
            } else if arg.adapts_integer_literal_to(param) {
                Some(true)
            } else if fits(i, &param, arg) {
                Some(adapted)
            } else {
                None
            }
        })
}

fn parameter_at_least_as_specific(
    src: &dyn SymbolSource,
    left: Ty,
    right: Ty,
    arg: CallArgKind,
) -> bool {
    left == right
        || (left == arg.ty() && arg.adapts_integer_literal_to(right))
        || semantic_arg_assignable(src, &right, &left)
}

/// Select the unique most-specific parameter shape from an already-applicable tied family. This is
/// independent of declaration kind: functions and constructors use the same Kotlin relation, while
/// retaining their own candidate records and call materialization.
pub(crate) fn most_specific_parameter_shape_index(
    src: &dyn SymbolSource,
    parameter_shapes: &[Vec<Ty>],
    args: &[CallArgKind],
) -> CandidateSelection<usize> {
    unique_most_specific_with_conflicts(
        parameter_shapes
            .iter()
            .enumerate()
            .map(|(index, params)| (params.clone(), index)),
        |position, left, right| {
            args.get(position).is_some_and(|argument| {
                parameter_at_least_as_specific(src, left, right, argument.clone())
            })
        },
        |_, _| true,
    )
}

fn integer_literal_overload<T>(
    candidates: impl Iterator<Item = (Vec<Ty>, T)>,
    args: &[CallArgKind],
    mut fits: impl FnMut(usize, &Ty, &CallArgKind) -> bool,
    at_least_as_specific: impl Fn(usize, Ty, Ty, CallArgKind) -> bool,
    equivalent_conflicts: impl Fn(&T, &T) -> bool,
) -> CandidateSelection<T> {
    if !args.iter().any(|arg| arg.is_integer_literal()) {
        return CandidateSelection::None;
    }
    let mut applicable = Vec::new();
    let mut has_adaptation = false;
    for (params, candidate) in candidates {
        let Some(adapted) = integer_literal_call_applies(&params, args, &mut fits) else {
            continue;
        };
        has_adaptation |= adapted;
        if let Some((_, existing_candidate)) = applicable
            .iter()
            .find(|(existing, _): &&(Vec<Ty>, T)| existing == &params)
        {
            if !equivalent_conflicts(existing_candidate, &candidate) {
                continue;
            }
        }
        applicable.push((params, candidate));
    }
    if !has_adaptation {
        return CandidateSelection::None;
    }
    unique_most_specific_with_conflicts(
        applicable,
        |position, left, right| {
            at_least_as_specific(
                position,
                left,
                right,
                args.get(position)
                    .unwrap_or(&CallArgKind::Typed(Ty::Error))
                    .clone(),
            )
        },
        equivalent_conflicts,
    )
}

fn best_callable_member_overload<'a>(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    candidates: impl Iterator<Item = &'a LibraryMember> + Clone,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Option<&'a LibraryMember> {
    let adapts = |p: &Ty, arg: &CallArgKind, _i: usize| arg.adapts_integer_literal_to(*p);
    let fits = |_position: usize, param: &Ty, arg: &CallArgKind| {
        if arg.is_omitted_default() {
            return true;
        }
        if arg.is_lambda_literal() {
            if param.fun_arity().is_some() {
                arg_fits_source(lib, src, param, &arg.ty())
            } else {
                sam_arg_matches(lib, src, *param, arg.ty())
            }
        } else {
            arg_fits_source(lib, src, param, &arg.ty())
        }
    };
    let logical = |member: &LibraryMember| {
        let params = specialized_member_params(member, args, type_args);
        apply_platform_call_parameter_nullability(
            params,
            &member.call_sig.platform_nullable_params,
            &args.iter().map(|arg| arg.ty()).collect::<Vec<_>>(),
            member.call_sig.vararg,
        )
    };
    let named = candidates.filter(|member| member.name == name);
    // Literal provenance lives beside the type, so exact probes see the ordinary runtime `Int`.
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    if let Some(exact) = named.clone().find(|member| logical(member) == arg_tys) {
        return Some(exact);
    }
    match integer_literal_overload(
        named.clone().map(|member| (logical(member), member)),
        args,
        |position, param, arg| fits(position, param, arg),
        |_position, left, right, arg| {
            parameter_at_least_as_specific(src, left, right, arg)
                || resolution_subtype(src, left, right)
        },
        |_, _| false,
    ) {
        CandidateSelection::Selected(member) => return Some(member),
        CandidateSelection::Ambiguous => return None,
        CandidateSelection::None => {}
    }
    match unique_most_specific(
        named.clone().filter_map(|member| {
            fixed_parameter_shape(&logical(member), args, |position, param, arg| {
                fits(position, param, arg)
            })
            .map(|shape| (shape, member))
        }),
        |_, left, right| resolution_subtype(src, left, right),
    ) {
        CandidateSelection::Selected(member) => return Some(member),
        CandidateSelection::Ambiguous => return None,
        CandidateSelection::None => {}
    }
    match unique_most_specific(
        named.clone().filter_map(|member| {
            let params = logical(member);
            (args.len()..params.len())
                .all(|position| member.call_sig.param_has_default(position))
                .then(|| {
                    omitted_parameter_shape(&params, args, |i, param, arg| {
                        fits(i, param, arg) || adapts(param, arg, i)
                    })
                    .map(|shape| (shape, member))
                })
                .flatten()
        }),
        |_, left, right| resolution_subtype(src, left, right),
    ) {
        CandidateSelection::Selected(member) => return Some(member),
        CandidateSelection::Ambiguous => return None,
        CandidateSelection::None => {}
    }
    match unique_most_specific(
        named.filter_map(|member| {
            let params = logical(member);
            let vararg_index = member.call_sig.vararg_index?;
            vararg_parameter_shape_at(
                &params,
                args,
                vararg_index,
                &member.call_sig.param_defaults,
                |i, param, arg| fits(i, param, arg) || adapts(param, arg, i),
            )
            .map(|shape| (shape, member))
        }),
        |_, left, right| resolution_subtype(src, left, right),
    ) {
        CandidateSelection::Selected(member) => Some(member),
        CandidateSelection::None | CandidateSelection::Ambiguous => None,
    }
}

pub(crate) fn ranked_extension_overloads_by_recv<'a>(
    src: &dyn SymbolSource,
    receiver: Ty,
    fs: &'a FunctionSet,
) -> Vec<(u32, Ty, &'a FunctionInfo)> {
    ranked_extension_candidates(src, receiver, fs.overloads.iter())
}

fn ranked_extension_candidates<'a>(
    src: &dyn SymbolSource,
    receiver: Ty,
    overloads: impl Iterator<Item = &'a FunctionInfo>,
) -> Vec<(u32, Ty, &'a FunctionInfo)> {
    let candidates = overloads
        .filter(|o| o.is_extension() && o.receiver_rank != u32::MAX)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }
    let mro = ReceiverMro::new(src, receiver);
    let mut out: Vec<(u32, Ty, &FunctionInfo)> = candidates
        .into_iter()
        .filter_map(|o| {
            let decl = o.semantic_receiver()?;
            let (rank, binding_receiver) = mro.match_receiver(src, decl)?;
            Some((rank, binding_receiver, o))
        })
        .collect();
    // `@kotlin.internal.HidesMembers` sits above the whole callable tower. Ordinary extensions are
    // ordered by lexical/import scope rung first and receiver distance only within that rung. This
    // matters to consumers that walk the list to shape postponed lambdas: a same-file `Any.map`
    // must shape its lambda before the more receiver-specific imported `Result<T>.map`, exactly as
    // final overload selection evaluates applicability one scope-tower rung at a time.
    out.sort_by_key(|(rank, _, o)| {
        let hides_members = o
            .annotations
            .contains(&crate::types::type_name("kotlin/internal/HidesMembers"));
        (
            !hides_members,
            if hides_members { 0 } else { o.scope_rank },
            *rank,
        )
    });
    out
}

fn callable_with_return(c: &LibraryCallable, ret: Ty, default_call: bool) -> LibraryCallable {
    LibraryCallable {
        ret,
        default_call,
        vararg_elem: None,
        vararg_index: None,
        ..c.clone()
    }
}

/// Materialize the default-argument bridge attached to the already-selected declaration. The bridge
/// is realization data only: semantic parameters, generic signature, visibility, and overload identity
/// remain those of `base`; no synthetic name is re-entered into resolution.
pub(crate) fn selected_default_callable(base: &FunctionInfo) -> Option<LibraryCallable> {
    let Some(realization) = base.callable.default_realization.as_deref() else {
        crate::trace_compiler!(
            "default_semantics",
            "selected callable has no default realization: {}.{}{}",
            base.callable.owner.render(),
            base.callable.name,
            base.callable.descriptor,
        );
        return None;
    };
    let mut callable = base.callable.clone();
    callable.owner = realization.owner;
    callable.name = realization.name.clone();
    callable.descriptor = realization.descriptor.clone();
    callable.physical_params = realization.real_params.clone();
    callable.physical_ret = realization.ret;
    callable.suspend = realization.suspend;
    callable.default_call = true;
    Some(callable)
}

/// Record the vararg slot/element on a `$default` callable so the lowerer packs loose trailing
/// elements into the slot's array — or emits an EMPTY array when the vararg itself is omitted
/// (kotlinc's `$default` passes the array straight through, so the slot takes NO mask bit and a
/// null placeholder would trip the callee's non-null vararg check). The arg-vs-param identity
/// guard keeps a caller passing the array itself (`f("n", arr)`) from being packed.
fn record_default_vararg_slot(
    callable: &mut LibraryCallable,
    vararg_index: Option<usize>,
    params: &[Ty],
    args: &[Ty],
) {
    let Some(index) = vararg_index else {
        return;
    };
    let Some(elem) = params.get(index).and_then(|param| param.array_read_elem()) else {
        return;
    };
    if args.get(index).copied() != params.get(index).copied() {
        callable.vararg_elem = Some(elem);
        callable.vararg_index = Some(index);
    }
}

/// The arg-dependent binding layer over a [`SymbolSource`]: it selects overloads and binds generics for
/// a specific call site. Holds the oracle by reference — cheap to construct per query.
pub struct SymbolResolver<'a> {
    /// Source-level library facts used during resolution.
    lib: &'a dyn SemanticPlatform,
    /// The aggregated resolution source: module declarations shadow library declarations of the same name.
    src: crate::symbol_source::CompositeSource<'a>,
    /// The current compilation module, when present.
    module: Option<&'a dyn SymbolSource>,
    /// The packages in scope for TOP-LEVEL function resolution (same-package, star/explicit imports,
    /// defaults). `None` disables the filter (a context with no import scope — signature inference).
    /// When `Some`, a top-level function resolves only if its facade's package is in scope, matching
    /// kotlinc: an unqualified top-level call binds ONLY to an imported/same-package/default function,
    /// not to any classpath function of that name.
    fn_scope: Option<FunctionScopeRef<'a>>,
    /// Lexically enclosing classes, nearest first.
    lexical_classes: Vec<TypeName>,
    /// Package containing the current source declaration, for Java package-private classifier access.
    access_package: Option<TypeName>,
    /// Current source file, for top-level `private` declarations in this compilation module.
    access_file: Option<u32>,
}

#[derive(Clone, Copy)]
enum FunctionScopeRef<'a> {
    Flat(&'a [TypeName]),
    Imports(&'a FunctionImportScope),
}

impl FunctionScopeRef<'_> {
    fn package_count(self) -> usize {
        match self {
            Self::Flat(packages) => packages.len(),
            Self::Imports(scope) => {
                scope.explicit.len() + scope.levels.iter().map(Vec::len).sum::<usize>()
            }
        }
    }
}

/// The receiver of a reference: a value, an implicit `this`, or a named type.
#[derive(Clone, Copy)]
pub enum SymRecv<'q> {
    Value(Ty),
    ImplicitValue(Ty),
    Type(&'q str),
    TypeName(TypeName),
    /// No receiver — a plain `name(args)` resolved against the import scope's top-level (and same-facade
    /// extension) functions. A DOTTED `name` (`kotlinx.coroutines.runBlocking`) is a fully-qualified
    /// reference: it resolves against its own package, not the import scope.
    TopLevel,
}

/// What a name DENOTES on its receiver — the declared thing the resolver found, NOT how it is used.
/// [`SymbolResolver::resolve_symbol`] resolves a name to one of these; the CALLER then applies whatever
/// its syntax needs (invoke it, read it, write its setter, take a reference), including handling a
/// mismatch itself (`Test()` where `Test` is a property — the caller emits an `invoke`). The resolver
/// does not care whether the site is a call, a read, a write, or a reference.
/// The facets a `recv.name` member supports — see [`Symbol::Member`]. Boxed into the enum so a member
/// symbol stays pointer-sized.
pub struct MemberFacets {
    pub call: Option<ResolvedMember>,
    pub read: Option<ResolvedMember>,
    pub write: Option<ResolvedPropertySetter>,
    pub method_ref: Option<LibraryMember>,
    pub property_ref: Option<ResolvedPropertyRef>,
    /// Receiver-less property declarations denoted by this name. Contextual overloads remain in the
    /// family because applicability depends on the caller's lexical/implicit-receiver scope, which
    /// the symbol resolver deliberately does not own.
    pub values: Vec<PropertyInfo>,
    /// Every overload named `name` applicable to the receiver — instance members, operators, AND in-scope
    /// extension functions with a matching receiver — most-derived/member-first. A caller inspecting the
    /// whole family (named-arg mapping, defaults, return agreement, member-vs-extension dispatch) filters
    /// this by [`FunctionInfo::kind`]/`receiver_rank`.
    pub overloads: Vec<FunctionInfo>,
    /// For a receiver-less [`SymRecv::TopLevel`] name: the single top-level callable selected against
    /// `args`/`type_args` (default/vararg-aware), ready for the emit seam. `None` for a value/type receiver.
    pub top_level_call: Option<LibraryCallable>,
    /// For a value receiver: the classpath EXTENSION callable `recv.name(args)` selected against
    /// `args`/`type_args` (default/vararg-aware; admits `@InlineOnly` splice candidates), ready for the emit
    /// seam. A same-module extension is `None` (it emits through the module path, not a library callable).
    pub extension_call: Option<LibraryCallable>,
    /// The RESULT of invoking the selected extension, for every declaration origin.
    ///
    /// [`Self::extension_call`] is an emit handle, and a same-module extension emits through the
    /// module path rather than as a library callable, so it is absent there — but the semantic
    /// result of the call is the same question for all origins and is answered here. Without it a
    /// consumer asking only "what does this name return on this receiver" had to know which provider
    /// declared the callable, which is a provenance test standing in for a semantic one.
    pub extension_result: Option<Ty>,
    pub extension_property: Option<PropertyInfo>,
}

impl MemberFacets {
    /// Materialize the callable-reference view of the already-discovered extension property.
    /// Consumers that need several facets of one name can keep this structure and must not repeat
    /// symbol lookup merely to ask for the property-reference form.
    pub(crate) fn extension_property_ref(&self) -> Option<ResolvedPropertyRef> {
        self.extension_property
            .clone()
            .and_then(select_extension_property_ref)
    }
}

pub enum Symbol {
    /// A member of a value receiver `recv.name`, with whichever facets the declaration supports. A name
    /// may support several at once — a Java zero-argument method (`list.size`, `str.length`) is both a
    /// property `read` and a `call`/method `reference` — so the resolver reports them all and the caller
    /// takes the one its syntax needs (`recv.name(args)` → `call`, `recv.name` → `read`, `recv.name = v`
    /// → `write`, `recv::name` → `method_ref`/`property_ref`).
    Member(Box<MemberFacets>),
    /// An object/companion instance member `Type.name(args)`.
    Instance(LibraryMember),
    /// A static/companion member `Type.name(args)`.
    Companion(LibraryMember),
    /// A constructor `Type(args)`: one semantic overload-selection result carrying the platform
    /// application selected for it. Direct and marker/default realization are not separate symbols.
    Constructor(SelectedConstructorCall),
}

impl Symbol {
    pub(crate) fn selected_member(self) -> Option<LibraryMember> {
        match self {
            Symbol::Member(f) => f.call.map(|resolved| resolved.member),
            Symbol::Instance(member) | Symbol::Companion(member) => Some(member),
            Symbol::Constructor(SelectedConstructorCall::Direct(member)) => Some(*member),
            Symbol::Constructor(SelectedConstructorCall::Platform(_)) => None,
        }
    }

    /// This name invoked as a method with the resolved arguments (`recv.name(args)`).
    pub fn call(self) -> Option<ResolvedMember> {
        match self {
            Symbol::Member(f) => f.call,
            _ => None,
        }
    }
    /// Semantic result of invoking the selected value-receiver callable. Member and extension are
    /// candidate kinds inside overload selection; a non-emitting consumer must not branch on their
    /// physical realization merely to read the result type.
    pub fn call_return(self) -> Option<Ty> {
        match self {
            Symbol::Member(f) => match f.call {
                Some(call) => Some(call.ret),
                // The emit handle first (it carries the call-site realization), then the result the
                // selection itself produced — the only answer a same-module extension has.
                None => f.extension_call.map(|call| call.ret).or(f.extension_result),
            },
            _ => None,
        }
    }
    /// The selected callable's own SYMBOLIC signature, its type variables still unbound.
    ///
    /// Reported through the same member-or-extension family as [`Self::call_return`]: which physical
    /// kind answered the call is a realization detail, and a consumer binding a type variable from an
    /// argument must not have to branch on it.
    pub fn call_generic_sig(self) -> Option<crate::libraries::GenericSig> {
        match self {
            Symbol::Member(f) => match f.call {
                Some(call) => call.member.generic_sig,
                None => f
                    .extension_call
                    .and_then(|call| call.generic_sig)
                    .map(|generic| *generic),
            },
            Symbol::Instance(member) | Symbol::Companion(member) => member.generic_sig,
            Symbol::Constructor(_) => None,
        }
    }
    /// This name read as a property (`recv.name`).
    pub fn property(self) -> Option<ResolvedMember> {
        match self {
            Symbol::Member(f) => f.read,
            _ => None,
        }
    }
    /// Semantic result of reading the selected receiver property, independent of whether its getter
    /// is an ordinary member or an extension realization.
    pub fn property_return(self) -> Option<Ty> {
        match self {
            Symbol::Member(f) => match f.read {
                Some(read) => Some(read.ret),
                None => f.extension_property.map(|property| property.ty),
            },
            _ => None,
        }
    }
    /// The setter of this property (`recv.name = v`).
    pub fn property_setter(self) -> Option<ResolvedPropertySetter> {
        match self {
            Symbol::Member(f) => f.write,
            _ => None,
        }
    }
    /// A bound method reference to this name (`recv::name`).
    pub fn method_ref(self) -> Option<LibraryMember> {
        match self {
            Symbol::Member(f) => f.method_ref,
            _ => None,
        }
    }
    /// A bound property reference to this name (`recv::name`).
    pub fn property_ref(self) -> Option<ResolvedPropertyRef> {
        match self {
            Symbol::Member(f) => f.property_ref,
            _ => None,
        }
    }
    pub fn value(self) -> Option<PropertyInfo> {
        match self {
            Symbol::Member(mut f) if f.values.len() == 1 => f.values.pop(),
            _ => None,
        }
    }
    pub fn values(self) -> Vec<PropertyInfo> {
        match self {
            Symbol::Member(f) => f.values,
            _ => Vec::new(),
        }
    }
    /// Every overload named this on the receiver — members, operators, and applicable in-scope extensions.
    pub fn overloads(self) -> Vec<FunctionInfo> {
        match self {
            Symbol::Member(f) => f.overloads,
            _ => Vec::new(),
        }
    }
    /// The selected receiver-less top-level callable ([`SymRecv::TopLevel`]).
    pub fn top_level_call(self) -> Option<LibraryCallable> {
        match self {
            Symbol::Member(f) => f.top_level_call,
            _ => None,
        }
    }
    /// The selected classpath extension callable for `recv.name(args)`.
    pub fn extension_call(self) -> Option<LibraryCallable> {
        match self {
            Symbol::Member(f) => f.extension_call,
            _ => None,
        }
    }
    /// The getter of a classpath extension property `recv.name`.
    pub fn extension_property_getter(self) -> Option<LibraryCallable> {
        match self {
            Symbol::Member(f) => f.extension_property.map(|property| property.getter),
            _ => None,
        }
    }
    pub fn extension_property(self) -> Option<PropertyInfo> {
        match self {
            Symbol::Member(f) => f.extension_property,
            _ => None,
        }
    }
    pub fn extension_property_ref(self) -> Option<ResolvedPropertyRef> {
        match self {
            Symbol::Member(f) => f.extension_property_ref(),
            _ => None,
        }
    }
    /// The object/companion instance member this resolved to (`Type.name(args)`).
    pub fn instance(self) -> Option<LibraryMember> {
        if let Symbol::Instance(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// The static/companion member this resolved to (`Type.name(args)`).
    pub fn companion(self) -> Option<LibraryMember> {
        if let Symbol::Companion(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// The constructor this resolved to (`Type(args)`).
    pub fn constructor(self) -> Option<SelectedConstructorCall> {
        if let Symbol::Constructor(m) = self {
            Some(m)
        } else {
            None
        }
    }
}

/// A selected property setter with the semantic access fact needed by the checker. Lowering receives
/// only `callable` after the checker has accepted `visibility`; it never reconstructs either fact.
#[derive(Clone, Debug)]
pub struct ResolvedPropertySetter {
    pub callable: LibraryCallable,
    pub visibility: crate::types::Visibility,
    pub source_member: Option<SourceMember>,
    pub stable_declaration: Option<crate::fir::DeclarationId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmbiguousExtensionProperty;

/// The emit-independent facts about a selected extension call. One computation feeds both the
/// callable the backend emits and the type the front end reports, so the two cannot disagree.
struct ExtensionCallShape {
    vparams: Vec<Ty>,
    args: Vec<Ty>,
    spread_slot: Option<(usize, Ty)>,
    /// The call supplies every value parameter; a defaulted call binds its return differently.
    exact: bool,
    ret: Ty,
    /// Every type variable the overload declares was bound by the receiver or an argument. When it
    /// is false the return is still the right thing to EMIT — an unbound variable erases to its
    /// bound, which is what the call site produces — but it is not a type worth reporting as a
    /// property's inferred type, because `Any`-from-failed-inference and a genuine `Any` are then
    /// indistinguishable.
    determined: bool,
}

/// Binding result for a name on the compiler's error receiver.
pub(crate) enum ErrorReceiverSelection<T> {
    Absent,
    Bind(Vec<T>),
    Silent,
}

impl<'a> SymbolResolver<'a> {
    /// Specialize one already-identified constructor declaration for contextual argument checking.
    /// The caller owns source-to-parameter argument mapping; this operation only applies the
    /// constructor's semantic generic signature and deliberately preserves formals owned by
    /// postponed lambdas until those lambdas have been checked.
    pub(crate) fn specialized_constructor_parameter_types(
        &self,
        constructor: &LibraryMember,
        arguments: &[CallArgKind],
        type_arguments: &[Ty],
    ) -> Vec<Ty> {
        specialized_constructor_params(&self.src, constructor, arguments, type_arguments)
    }

    /// Select a constructor declaration without coupling it to a platform invocation. Frontend
    /// signature inference needs the semantic result and parameter mapping only; default-marker or
    /// factory realization belongs to lowering after signatures are finalized.
    pub(crate) fn select_constructor_declaration_with_type_arguments(
        &self,
        internal: TypeName,
        args: &[CallArgKind],
        type_args: &[Ty],
    ) -> Option<LibraryMember> {
        let classifier = self.src.classifier(internal)?;
        let mut selected = select_constructor_declaration_from_type_with_type_arguments(
            self.lib,
            &self.src,
            &classifier,
            args,
            type_args,
        );
        if let Some(declaration) = &mut selected {
            declaration.owner.get_or_insert(internal);
            let mut bindings = classifier
                .type_parameters
                .type_params
                .iter()
                .cloned()
                .zip(type_args.iter().copied())
                .collect::<GSigBinds>();
            if let Some(signature) = declaration.generic_sig.as_ref() {
                // Overload selection has already specialized the constructor parameters. Recover
                // the corresponding classifier arguments from that selected semantic shape; this
                // preserves inference from integer literals, expected lambdas, named/default
                // mapping, and explicit type arguments without rerunning constructor selection.
                for (declared, applied) in signature.params.iter().zip(&declaration.params) {
                    unify_inferred_ty(*declared, *applied, &mut bindings);
                }
                // A lambda is postponed while its expected function shape is established, so the
                // selected parameter above can still contain the classifier formal (`(Int) -> T`).
                // Once the compact expression has produced the lambda's checked shape, collect its
                // result constraint as well. Keep already-selected/explicit bindings authoritative:
                // this second channel fills only formals that the selection shape left unresolved.
                let mut completed_argument_bindings = GSigBinds::new();
                for ((declared, applied), argument) in
                    signature.params.iter().zip(&declaration.params).zip(args)
                {
                    if argument.is_omitted_default() || argument.ty() == Ty::Error {
                        continue;
                    }
                    unify_inferred_ty(
                        *declared,
                        argument.type_for(*applied),
                        &mut completed_argument_bindings,
                    );
                }
                for formal in &classifier.type_parameters.type_params {
                    if !bindings.contains_key(formal) {
                        if let Some(argument) = completed_argument_bindings.get(formal).copied() {
                            bindings.insert(formal.clone(), argument);
                        }
                    }
                }
            }
            let arguments = classifier
                .type_parameters
                .type_params
                .iter()
                .enumerate()
                .map(|(index, formal)| {
                    bindings.get(formal).copied().unwrap_or_else(|| {
                        classifier
                            .type_parameters
                            .type_param_bounds
                            .get(index)
                            .and_then(|bounds| bounds.first())
                            .copied()
                            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                    })
                })
                .collect::<Vec<_>>();
            declaration.ret = Ty::obj_args_name(internal, &arguments);
        }
        crate::trace_compiler!(
            "signature",
            "semantic constructor selection classifier={internal} type_params={:?} explicit={type_args:?} declarations={:?} args={args:?} selected={:?}",
            classifier.type_parameters.type_params,
            classifier
                .constructors
                .iter()
                .map(|constructor| (
                    &constructor.params,
                    constructor.generic_sig.as_ref().map(|signature| (
                        &signature.formals,
                        &signature.params,
                    )),
                ))
                .collect::<Vec<_>>(),
            selected.as_ref().map(|constructor| (
                constructor.params.as_slice(),
                &constructor.call_sig,
            )),
        );
        selected
    }

    pub(crate) fn classifier_in_scope(&self, name: &str) -> CandidateSelection<TypeName> {
        classifier_scope::select(&self.src, self.fn_scope, name)
    }

    /// Select a nested classifier from an applied value/classifier receiver using the common
    /// inheritance and accessibility rules. Callers decide whether the selected classifier is a
    /// value (an `object`) or type syntax; this operation only binds its stable identity.
    pub(crate) fn nested_classifier(
        &self,
        receiver: Ty,
        name: &str,
    ) -> CandidateSelection<TypeName> {
        let Some(owner) = receiver.non_null().obj_internal() else {
            return CandidateSelection::None;
        };
        match inherited_nested_classifier_name(
            name,
            vec![owner],
            |candidate_owner| {
                direct_supertypes(&self.src, Ty::obj_name(candidate_owner))
                    .into_iter()
                    .filter_map(Ty::kotlin_class_internal)
                    .collect()
            },
            |candidate| self.classifier_accessible(candidate),
        ) {
            InheritedNestedClassifier::Found(classifier) => {
                CandidateSelection::Selected(classifier)
            }
            InheritedNestedClassifier::Ambiguous => CandidateSelection::Ambiguous,
            InheritedNestedClassifier::NotFound => CandidateSelection::None,
        }
    }

    pub fn new(lib: &'a dyn SemanticPlatform) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![lib as &dyn SymbolSource]),
            module: None,
            fn_scope: None,
            lexical_classes: Vec::new(),
            access_package: None,
            access_file: None,
        }
    }

    /// A resolver whose top-level function resolution is restricted to `fn_scope`'s packages.
    pub fn new_scoped(lib: &'a dyn SemanticPlatform, fn_scope: &'a [TypeName]) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![lib as &dyn SymbolSource]),
            module: None,
            fn_scope: Some(FunctionScopeRef::Flat(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
            access_file: None,
        }
    }

    /// The primary resolver: symbol resolution federates the current `module` over the classpath `lib`.
    pub fn new_scoped_with_module(
        lib: &'a dyn SemanticPlatform,
        module: &'a dyn SymbolSource,
        fn_scope: &'a [TypeName],
    ) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![module, lib as &dyn SymbolSource]),
            module: Some(module),
            fn_scope: Some(FunctionScopeRef::Flat(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
            access_file: None,
        }
    }

    pub(crate) fn new_import_scoped_with_module(
        lib: &'a dyn SemanticPlatform,
        module: &'a dyn SymbolSource,
        fn_scope: &'a FunctionImportScope,
    ) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![module, lib as &dyn SymbolSource]),
            module: Some(module),
            fn_scope: Some(FunctionScopeRef::Imports(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
            access_file: None,
        }
    }

    pub(crate) fn with_access_context(
        mut self,
        package: TypeName,
        file: u32,
        classes: Vec<TypeName>,
    ) -> Self {
        self.access_package = Some(package);
        self.access_file = Some(file);
        self.lexical_classes = classes;
        self
    }

    fn classifier_accessible(&self, internal: TypeName) -> bool {
        let Some(classifier) = self.src.classifier(internal) else {
            return false;
        };
        // A companion's physical nested classifier may be non-public even though source code exposes
        // it through the outer classifier's public companion value (`Double.Companion`). Admit exactly
        // that metadata edge; an arbitrary internal nested classifier remains inaccessible.
        if let Some(owner) = internal.nested_owner() {
            let exposed_companion = self
                .src
                .classifier(owner)
                .and_then(|outer| outer.companion_object.as_ref().map(|(_, name)| *name))
                .is_some_and(|companion| companion == internal);
            if exposed_companion && self.classifier_accessible(owner) {
                return true;
            }
        }
        let visibility = classifier.access.visibility();
        if classifier.access == crate::libraries::ClassifierAccess::Public
            || (classifier.access == crate::libraries::ClassifierAccess::Internal
                && (self
                    .module
                    .is_some_and(|module| module.classifier(internal).is_some())
                    || self.lib.internal_accessible(internal)))
        {
            return true;
        }
        if classifier.access == crate::libraries::ClassifierAccess::PackagePrivate
            && self.package_private_member_accessible(internal)
        {
            return true;
        }
        if classifier.access == crate::libraries::ClassifierAccess::Private
            && !internal.contains("$")
            && classifier.source_file == self.access_file
        {
            return true;
        }
        // A private/protected nested classifier is a member of its enclosing classifier. Module
        // lookup therefore needs the lexical owner stack: code in `Outer` (or one of its nested
        // classes) may name `Outer$Hidden` / `Outer$Stage` directly. Protected access from a
        // subclass is handled by the inherited-classifier walk below; this arm covers the declaring
        // class itself, which does not inherit from itself. The module source separately handles
        // top-level file-private declarations.
        if matches!(
            visibility,
            crate::types::Visibility::Private | crate::types::Visibility::Protected
        ) && self
            .module
            .is_some_and(|module| module.classifier(internal).is_some())
        {
            let rendered = internal.render();
            if self.lexical_classes.iter().copied().any(|owner| {
                let owner = owner.render();
                rendered == owner
                    || rendered
                        .strip_prefix(&owner)
                        .is_some_and(|suffix| suffix.starts_with('$'))
            }) {
                return true;
            }
        }
        let rendered = internal.render();
        let Some(simple) = rendered.rsplit_once('$').map(|(_, simple)| simple) else {
            return false;
        };
        self.lexical_classes.iter().copied().any(|owner| {
            inherited_nested_classifier_name(
                simple,
                direct_supertypes(&self.src, Ty::obj_name(owner))
                    .into_iter()
                    .filter_map(Ty::kotlin_class_internal)
                    .collect(),
                |candidate_owner| {
                    direct_supertypes(&self.src, Ty::obj_name(candidate_owner))
                        .into_iter()
                        .filter_map(Ty::kotlin_class_internal)
                        .collect()
                },
                |candidate| inherited_classifier_shape(&self.src, candidate, owner).is_some(),
            ) == InheritedNestedClassifier::Found(internal)
        })
    }

    pub(crate) fn inaccessible_classifier_access(
        &self,
        internal: TypeName,
    ) -> Option<crate::symbol_source::ClassifierAccess> {
        let access = self.src.classifier(internal)?.access;
        (!self.classifier_accessible(internal)).then_some(access)
    }

    fn package_private_member_accessible(&self, owner: TypeName) -> bool {
        self.access_package.is_some_and(|package| {
            let declared = owner.package();
            package.matches(&declared)
        })
    }

    /// Whether the type named `internal` — or anything in its (classpath) supertype chain — declares a
    /// member named `name` (Kotlin/source or physical JVM name). Drives the OVERRIDE test for a class
    /// whose supertype is not in the same file: an override is emitted without `ACC_FINAL` (kotlinc).
    pub fn declares_member(&self, internal: &str, name: &str) -> bool {
        let mut work = vec![crate::types::type_name(internal)];
        let mut seen = std::collections::HashSet::new();
        while let Some(cur) = work.pop() {
            if cur.matches("java/lang/Object") || cur.matches("kotlin/Any") || !seen.insert(cur) {
                continue;
            }
            let Some(t) = self.src.classifier(cur) else {
                continue;
            };
            if t.members
                .iter()
                .any(|m| m.name == name || m.physical_name.as_deref() == Some(name))
            {
                return true;
            }
            work.extend(t.supertypes.iter_ids());
        }
        false
    }

    /// The unqualified-name resolution loop for this resolver's import scope — `resolve_symbols` per
    /// candidate fqn `pkg/name` over the federated source. THE way to resolve an unqualified name: the
    /// caller extracts `classifier`, `callables.functions` (∪ classifier constructors, then `invoke`), or
    /// `callables.properties` from the records. Empty when there is no import scope.
    pub(crate) fn symbols_in_scope(
        &self,
        name: &str,
    ) -> Vec<std::rc::Rc<crate::libraries::ResolvedSymbols>> {
        self.fn_scope
            .map(|scope| symbols_in_function_scope(&self.src, name, scope))
            .unwrap_or_default()
    }

    /// Callable records by Kotlin scope-tower level, nearest first. A declaration with the same
    /// spelling but the wrong callable facet or an inapplicable receiver does not hide a declaration
    /// at the next level; selection, not lookup, decides which level is the first applicable one.
    fn symbol_levels_in_scope(
        &self,
        name: &str,
    ) -> Vec<Vec<std::rc::Rc<crate::libraries::ResolvedSymbols>>> {
        self.fn_scope
            .map(|scope| symbol_levels_in_function_scope(&self.src, name, scope))
            .unwrap_or_default()
    }

    /// Collect the declarations denoted by `receiver.name` exactly once. Member declarations and
    /// imported extension declarations use the same [`Callables`] shape; overload selection is a
    /// separate operation over this family.
    pub(crate) fn receiver_callables(&self, receiver: Ty, name: &str) -> Callables {
        // `Nothing` is the bottom value type, but its member scope is still Kotlin's ordinary
        // `Any` scope. This matters on unreachable/safe selector branches (`x == null; x?.equals(1)`):
        // the receiver remains `Nothing` for flow typing and extension applicability, while member
        // inventory comes from the semantic top classifier. No runtime receiver conversion is
        // implied; checked FIR retains the original bottom-typed expression.
        let member_scope_receiver = member_scope_receiver(receiver);
        let members = if !member_scope_receiver.is_nullable()
            && (member_scope_receiver.kotlin_class_internal().is_some()
                || matches!(member_scope_receiver, Ty::Fun(_)))
        {
            members_in_hierarchy(&self.src, member_scope_receiver, name)
        } else {
            Callables::default()
        };
        let mut functions = members
            .functions()
            .iter()
            .filter(|function| function.kind == FnKind::Member)
            .cloned()
            .collect::<Vec<_>>();
        let mut properties = members
            .properties()
            .iter()
            .filter(|property| property.kind == PropKind::Member)
            .cloned()
            .collect::<Vec<_>>();
        if self.fn_scope.is_some() {
            let levels = self.symbol_levels_in_scope(name);
            // `@kotlin.internal.HidesMembers` declarations are NOT part of the tower: they resolve
            // above all of it. The nearest-level rule below stops at the first level holding an
            // applicable extension, which would hide the default-imported `kotlin.collections`
            // level behind a same-file / imported / local `forEach` — so lift every annotated
            // declaration applicable to this receiver out of EVERY level first. Selection, not
            // lookup, then decides whether the promotion actually takes the call (see the priority
            // tiers in `select_overload_tracking_with_functions`); an annotated declaration that
            // does not fit falls through to the ordinary candidates collected below.
            for level in &levels {
                let scoped = callables_from_symbols(level);
                functions.extend(
                    ranked_extension_candidates(&self.src, receiver, scoped.functions().iter())
                        .into_iter()
                        .filter(|(_, _, function)| {
                            function
                                .annotations
                                .contains(&crate::types::type_name("kotlin/internal/HidesMembers"))
                        })
                        .map(|(rank, _, function)| {
                            let mut function = function.clone();
                            function.receiver_rank = rank;
                            function
                        }),
                );
            }
            let mut extension_property_level_found = false;
            for (scope_rank, level) in levels.into_iter().enumerate() {
                let scoped = callables_from_symbols(&level);
                crate::trace_compiler!(
                    "resolve",
                    "receiver scope level name={name} receiver={receiver:?} functions={:?}",
                    scoped
                        .functions()
                        .iter()
                        .map(|function| (function.kind, function.semantic_receiver()))
                        .collect::<Vec<_>>()
                );
                // Annotated declarations were already lifted above the tower; collecting them a second
                // time here would put two copies of one declaration in the same priority bucket and
                // read as an ambiguity. A level holding ONLY annotated declarations is therefore
                // empty for tower purposes and the walk continues past it.
                let extensions =
                    ranked_extension_candidates(&self.src, receiver, scoped.functions().iter())
                        .into_iter()
                        .filter(|(_, _, function)| {
                            !function
                                .annotations
                                .contains(&crate::types::type_name("kotlin/internal/HidesMembers"))
                        })
                        .collect::<Vec<_>>();
                crate::trace_compiler!(
                    "resolve",
                    "receiver scope applicable name={name} receiver={receiver:?} extensions={:?}",
                    extensions
                        .iter()
                        .map(|(rank, binding, function)| (
                            rank,
                            binding,
                            function.semantic_receiver()
                        ))
                        .collect::<Vec<_>>()
                );
                let extension_properties = if extension_property_level_found {
                    Vec::new()
                } else {
                    scoped
                        .properties()
                        .iter()
                        .filter(|property| property.kind == PropKind::Extension)
                        .cloned()
                        .collect::<Vec<_>>()
                };
                if extensions.is_empty() && extension_properties.is_empty() {
                    continue;
                }
                functions.extend(extensions.into_iter().map(|(rank, _, function)| {
                    let mut function = function.clone();
                    function.receiver_rank = rank;
                    function.scope_rank = u32::try_from(scope_rank).unwrap_or(u32::MAX);
                    function
                }));
                extension_property_level_found |= !extension_properties.is_empty();
                properties.extend(extension_properties);
            }
        }
        Callables::from_parts(
            FunctionSet {
                overloads: functions,
            },
            PropertySet {
                overloads: properties,
            },
        )
    }

    /// `Any` members precede visible extensions on an error receiver.
    pub(crate) fn error_receiver_functions(
        &self,
        name: &str,
    ) -> ErrorReceiverSelection<FunctionInfo> {
        let members: Vec<FunctionInfo> =
            members_in_hierarchy(&self.src, Ty::obj("kotlin/Any"), name)
                .functions()
                .iter()
                .filter(|function| function.kind == FnKind::Member)
                .cloned()
                .collect();
        if !members.is_empty() {
            return ErrorReceiverSelection::Bind(members);
        }
        self.error_receiver_extensions(name, |callables| {
            callables
                .functions()
                .iter()
                .filter(|function| function.is_extension())
                .cloned()
                .collect()
        })
    }

    /// Visible extension properties on an error receiver.
    pub(crate) fn error_receiver_properties(
        &self,
        name: &str,
    ) -> ErrorReceiverSelection<PropertyInfo> {
        self.error_receiver_extensions(name, |callables| {
            callables
                .properties()
                .iter()
                .filter(|property| property.kind == PropKind::Extension)
                .cloned()
                .collect()
        })
    }

    fn error_receiver_extensions<T>(
        &self,
        name: &str,
        facet: impl Fn(&Callables) -> Vec<T>,
    ) -> ErrorReceiverSelection<T> {
        let Some(scope) = self.fn_scope else {
            return ErrorReceiverSelection::Absent;
        };
        for level in tagged_symbol_levels_in_function_scope(&self.src, name, scope) {
            let candidates = facet(&callables_from_symbols(&level.symbols));
            if candidates.is_empty() {
                continue;
            }
            if level.kind.ambiguity_checks() && candidates.len() > 1 {
                return ErrorReceiverSelection::Silent;
            }
            return ErrorReceiverSelection::Bind(candidates);
        }
        ErrorReceiverSelection::Absent
    }

    pub(crate) fn select_receiver_indexed_get_function_with_params(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> CandidateSelection<(FunctionInfo, Vec<Ty>, Ty)> {
        let selected = match select_receiver_overload_from_functions_tracking(
            self.lib,
            receiver,
            name,
            args,
            type_args,
            ExtCtx {
                fn_scope: self.fn_scope,
                source: &self.src,
            },
            callables.functions(),
            IndexedConvention::Get,
        ) {
            CandidateSelection::Selected(selected) => selected,
            CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
            CandidateSelection::None => return CandidateSelection::None,
        };
        let binding_receiver = selected
            .semantic_receiver()
            .and_then(|declared| {
                ReceiverMro::new(&self.src, receiver).binding_receiver(&self.src, declared)
            })
            .unwrap_or(receiver);
        if selected.call_sig.vararg_index.is_none() {
            let params =
                logical_call_params(&self.src, &selected, binding_receiver, args, type_args);
            let resolved = if selected.is_extension() {
                let arg_tys = args.iter().map(CallArgKind::ty).collect::<Vec<_>>();
                selected.generic_sig.as_ref().map_or(
                    selected.ret.apply(selected.callable.ret),
                    |signature| {
                        specialized_extension_return(
                            self.lib,
                            &selected,
                            bind_ext_ret(
                                &self.src,
                                signature,
                                binding_receiver,
                                &arg_tys,
                                type_args,
                            ),
                        )
                    },
                )
            } else {
                resolved_member_from_info(
                    self.lib,
                    &self.src,
                    receiver,
                    args,
                    type_args,
                    selected.clone(),
                )
                .ret
            };
            return CandidateSelection::Selected((selected, params, resolved));
        }
        let Some((params, ret)) = indexed_call_shape(
            self.lib,
            &self.src,
            &selected,
            binding_receiver,
            args,
            type_args,
            false,
        ) else {
            return CandidateSelection::None;
        };
        CandidateSelection::Selected((selected, params, ret))
    }

    /// Select the `set` convention used by indexed assignment. The assignment RHS binds the final
    /// value parameter even when the preceding index parameter is a vararg; ordinary positional
    /// calls deliberately do not permit that mapping.
    pub(crate) fn select_receiver_indexed_set_function(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> CandidateSelection<FunctionInfo> {
        select_receiver_overload_from_functions_tracking(
            self.lib,
            receiver,
            name,
            args,
            type_args,
            ExtCtx {
                fn_scope: self.fn_scope,
                source: &self.src,
            },
            callables.functions(),
            IndexedConvention::Set,
        )
    }

    pub(crate) fn select_receiver_indexed_set_function_with_params(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> CandidateSelection<(FunctionInfo, Vec<Ty>, Ty)> {
        let selected = match self
            .select_receiver_indexed_set_function(receiver, name, args, type_args, callables)
        {
            CandidateSelection::Selected(selected) => selected,
            CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
            CandidateSelection::None => return CandidateSelection::None,
        };
        let binding_receiver = selected
            .semantic_receiver()
            .and_then(|declared| {
                ReceiverMro::new(&self.src, receiver).binding_receiver(&self.src, declared)
            })
            .unwrap_or(receiver);
        if selected.call_sig.vararg_index.is_none() {
            let params =
                logical_call_params(&self.src, &selected, binding_receiver, args, type_args);
            let arg_tys = args.iter().map(CallArgKind::ty).collect::<Vec<_>>();
            let inferred_ret = selected
                .generic_sig
                .as_ref()
                .map_or(selected.callable.ret, |sig| {
                    if selected.is_extension() {
                        bind_ext_ret(&self.src, sig, binding_receiver, &arg_tys, type_args)
                    } else {
                        bind_member_return(
                            &self.src,
                            sig,
                            binding_receiver,
                            &arg_tys,
                            type_args,
                            selected.callable.ret,
                        )
                    }
                });
            let ret = selected.ret.apply(inferred_ret);
            return CandidateSelection::Selected((selected, params, ret));
        }
        let Some((params, ret)) = indexed_call_shape(
            self.lib,
            &self.src,
            &selected,
            binding_receiver,
            args,
            type_args,
            true,
        ) else {
            return CandidateSelection::None;
        };
        CandidateSelection::Selected((selected, params, ret))
    }

    /// Select one receiver callable and return the value parameters specialized by the receiver and
    /// the already-known arguments. Syntax owners use this before checking postponed lambdas; overload
    /// selection remains here, while the checker remains responsible for typing expression bodies.
    pub(crate) fn select_receiver_function_with_params(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> Option<(FunctionInfo, Vec<Ty>)> {
        match self.select_receiver_function_with_params_tracking(
            receiver, name, args, type_args, callables,
        ) {
            CandidateSelection::Selected((selected, params, _)) => Some((selected, params)),
            CandidateSelection::None | CandidateSelection::Ambiguous => None,
        }
    }

    /// Parameter shapes of a receiver callable family after receiver and already-typed argument
    /// inference, without selecting an overload. A postponed lambda may be needed to distinguish
    /// otherwise applicable overloads; the caller may use only facts common to every returned
    /// shape to type that lambda, then invoke ordinary overload selection again with its final type.
    pub(crate) fn receiver_function_parameter_shapes(
        &self,
        receiver: Ty,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> Vec<Vec<Ty>> {
        callables
            .functions()
            .iter()
            .filter_map(|candidate| {
                let binding_receiver = candidate
                    .semantic_receiver()
                    .and_then(|declared| {
                        ReceiverMro::new(&self.src, receiver).binding_receiver(&self.src, declared)
                    })
                    .unwrap_or(receiver);
                let parameters =
                    logical_call_params(&self.src, candidate, binding_receiver, args, type_args);
                (parameters.len() == args.len()).then_some(parameters)
            })
            .collect()
    }

    /// Tracking form used by synthesized language conventions. It preserves ambiguity and the exact
    /// specialized return beside the selected declaration so the checker can commit one target and
    /// lowering never has to repeat receiver or overload resolution.
    pub(crate) fn select_receiver_function_with_params_tracking(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> CandidateSelection<(FunctionInfo, Vec<Ty>, Ty)> {
        match self.select_receiver_function_with_applied_receiver_tracking(
            receiver, name, args, type_args, callables,
        ) {
            CandidateSelection::Selected((selected, params, ret, _)) => {
                CandidateSelection::Selected((selected, params, ret))
            }
            CandidateSelection::None => CandidateSelection::None,
            CandidateSelection::Ambiguous => CandidateSelection::Ambiguous,
        }
    }

    /// Delegate conventions additionally need the receiver application inferred by their ordinary
    /// value arguments. For example, `D("K")` may initially have the raw type `D<>`, while
    /// `getValue(thisRef: R, ...)` fixes the owning `D<R>` to `D<String>`. Keep that application
    /// beside the selected declaration so the delegate initializer can be checked authoritatively
    /// against it; FIR must never reconstruct the inference.
    pub(crate) fn select_receiver_function_with_applied_receiver_tracking(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> CandidateSelection<(FunctionInfo, Vec<Ty>, Ty, Ty)> {
        let selected = match select_receiver_overload_from_functions_tracking(
            self.lib,
            receiver,
            name,
            args,
            type_args,
            ExtCtx {
                fn_scope: self.fn_scope,
                source: &self.src,
            },
            callables.functions(),
            IndexedConvention::Ordinary,
        ) {
            CandidateSelection::Selected(selected) => selected,
            CandidateSelection::None => return CandidateSelection::None,
            CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        };
        let binding_receiver = selected
            .semantic_receiver()
            .and_then(|declared| {
                ReceiverMro::new(&self.src, receiver).binding_receiver(&self.src, declared)
            })
            .unwrap_or(receiver);
        let semantic = selected.semantic_signature();
        let mut bindings = seeded_gsig_binds(&semantic, type_args);
        if let Some(declared_receiver) = semantic.receiver {
            unify_ty(declared_receiver, binding_receiver, &mut bindings);
        }
        let receiver_bindings = bindings.clone();
        let value_params = &semantic.params[selected.context_count.min(semantic.params.len())..];
        let mut argument_bindings = GSigBinds::new();
        for (&parameter, argument) in value_params.iter().zip(args) {
            if !argument.is_lambda_literal()
                && !argument.is_expected_type_callable()
                && !argument.is_omitted_default()
            {
                unify_inferred_ty(
                    parameter,
                    argument.type_for(parameter),
                    &mut argument_bindings,
                );
            }
        }
        let owner_argument_bindings = argument_bindings.clone();
        merge_call_argument_bindings(
            &self.src,
            &semantic,
            type_args,
            &receiver_bindings,
            &mut bindings,
            argument_bindings,
        );
        // A raw owning classifier has no receiver arguments to seed its declaration parameters.
        // They are nevertheless ordinary inference variables when a convention parameter mentions
        // them (`D<in R>.getValue(thisRef: R, ...)`). Method generic signatures intentionally list
        // only method-owned formals, so retain the argument constraints for the owner's distinct
        // stable formals here.
        if let Ty::Obj(owner, arguments) = receiver.non_null() {
            if arguments.is_empty() {
                if let Some(classifier) = self.src.classifier(owner) {
                    for formal in &classifier.type_params {
                        if let Some(inferred) = owner_argument_bindings.get(formal).copied() {
                            bindings.entry(formal.clone()).or_insert(inferred);
                        }
                    }
                }
            }
        }
        crate::trace_compiler!(
            "fir",
            "receiver call application receiver={receiver:?} name={name} semantic={semantic:?} bindings={bindings:?}",
        );
        let params = value_params
            .iter()
            .map(|parameter| ty_subst_keep_unbound(*parameter, &bindings))
            .collect::<Vec<_>>();
        let applied_receiver = match receiver.non_null() {
            Ty::Obj(owner, arguments) => self
                .src
                .classifier(owner)
                .and_then(|classifier| {
                    let applied = if arguments.len() == classifier.type_params.len() {
                        arguments
                            .iter()
                            .map(|argument| ty_subst_keep_unbound(*argument, &bindings))
                            .collect::<Vec<_>>()
                    } else if arguments.is_empty() {
                        classifier
                            .type_params
                            .iter()
                            .map(|formal| bindings.get(formal).copied())
                            .collect::<Option<Vec<_>>>()?
                    } else {
                        return None;
                    };
                    Some(Ty::obj_args_name(owner, &applied))
                })
                .unwrap_or(receiver),
            _ => receiver,
        };
        let ret = if selected.is_extension() {
            selected.generic_sig.as_ref().map_or(
                selected.ret.apply(selected.callable.ret),
                |signature| {
                    specialized_extension_return(
                        self.lib,
                        &selected,
                        bind_ext_ret(
                            &self.src,
                            signature,
                            binding_receiver,
                            &args.iter().map(CallArgKind::ty).collect::<Vec<_>>(),
                            type_args,
                        ),
                    )
                },
            )
        } else {
            let provider = resolved_member_from_info(
                self.lib,
                &self.src,
                receiver,
                args,
                type_args,
                selected.clone(),
            )
            .ret;
            merge_specialized_return(provider, ty_subst_keep_unbound(semantic.ret, &bindings))
        };
        CandidateSelection::Selected((selected, params, ret, applied_receiver))
    }

    /// Apply a convention constraint expressed on a selected supertype back to a raw concrete
    /// receiver. The classifier's own stable type parameters are substituted through its declared
    /// supertype templates, so `Single<T> : Entity<T?>` constrained as `Entity<List<Any>?>` becomes
    /// `Single<List<Any>>` rather than an erased or guessed application.
    pub(crate) fn apply_raw_receiver_constraint(&self, receiver: Ty, constraint: Ty) -> Option<Ty> {
        let Ty::Obj(owner, arguments) = receiver.non_null() else {
            return None;
        };
        if !arguments.is_empty() {
            return None;
        }
        let classifier = self.src.classifier(owner)?;
        if classifier.type_params.is_empty() {
            return None;
        }
        let symbolic_arguments = classifier
            .type_params
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                let bound = classifier
                    .type_param_bounds
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                Ty::ty_param(formal, bound)
            })
            .collect::<Vec<_>>();
        let symbolic_receiver = Ty::obj_args_name(owner, &symbolic_arguments);
        let constrained_owner = constraint.obj_internal()?;
        let declared_shape = receiver_hierarchy(&self.src, symbolic_receiver)
            .into_iter()
            .map(|(shape, _)| shape)
            .find(|shape| shape.obj_internal() == Some(constrained_owner))?;
        let mut bindings = GSigBinds::new();
        unify_inferred_ty_with_source(&self.src, declared_shape, constraint, &mut bindings);
        let applied = classifier
            .type_params
            .iter()
            .map(|formal| bindings.get(formal).copied())
            .collect::<Option<Vec<_>>>()?;
        Some(Ty::obj_args_name(owner, &applied))
    }

    pub(crate) fn materialize_member_function(
        &self,
        receiver: Ty,
        args: &[CallArgKind],
        type_args: &[Ty],
        selected: FunctionInfo,
    ) -> ResolvedMember {
        resolved_member_from_info(self.lib, &self.src, receiver, args, type_args, selected)
    }

    /// Convert an already-specialized overload selection into the checker/lowering handoff. The
    /// contextual selector has completed receiver, argument, and expected-result inference; running
    /// the legacy argument-only binder here would discard those bindings (notably for zero-argument
    /// generic members) and replace the selected return with its erased bound.
    pub(crate) fn commit_selected_member_function(
        &self,
        receiver: Ty,
        selected: FunctionInfo,
    ) -> ResolvedMember {
        let ret = selected.callable.ret;
        self.commit_selected_member_function_result(receiver, selected, ret)
    }

    /// Materialize an overload together with the exact result produced by the selecting inference
    /// session. Adapters that use a tracking selector must carry this result forward instead of
    /// invoking an older argument-only binder and losing projection constraints.
    pub(crate) fn commit_selected_member_function_result(
        &self,
        receiver: Ty,
        selected: FunctionInfo,
        ret: Ty,
    ) -> ResolvedMember {
        let member = selected.member_with_return(ret);
        ResolvedMember {
            receiver,
            ret,
            member,
            physical_params: selected.callable.physical_params.clone(),
            context_args: Vec::new(),
            projected_return_hazard: selected.projected_return_hazard,
            suspend: selected.flags.suspend,
            origin: selected.callable.origin.clone(),
        }
    }

    /// Select the nearest in-scope extension property, rejecting equal-rank candidates.
    pub fn select_extension_property(
        &self,
        receiver: Ty,
        name: &str,
    ) -> Result<Option<PropertyInfo>, AmbiguousExtensionProperty> {
        for symbols in self.symbol_levels_in_scope(name) {
            let callables = callables_from_symbols(&symbols);
            match self.select_extension_property_from_callables(receiver, name, &callables)? {
                Some(property) => return Ok(Some(property)),
                None => continue,
            }
        }
        Ok(None)
    }

    fn select_extension_property_from_callables(
        &self,
        receiver: Ty,
        name: &str,
        callables: &Callables,
    ) -> Result<Option<PropertyInfo>, AmbiguousExtensionProperty> {
        if callables.properties().is_empty() {
            return Ok(None);
        }
        let receiver_mro = ReceiverMro::new(&self.src, receiver);
        let mut candidates = callables
            .properties()
            .iter()
            .filter(|property| property.kind == PropKind::Extension)
            .filter(|property| source_property_visible(self.lib, property))
            .cloned()
            .filter(|property| {
                generic_bounds_admit(
                    &self.src,
                    property.getter.generic_sig.as_deref(),
                    receiver,
                    &[],
                    &[],
                )
            })
            .filter_map(|property| {
                let declared = property.receiver?;
                let rank = receiver_mro.rank(&self.src, declared);
                crate::trace_compiler!(
                    "resolve",
                    "extension property candidate name={name} receiver={receiver:?} declared={declared:?} setter={} source={:?} rank={rank:?}",
                    property.setter.is_some(),
                    property.source_key,
                );
                rank.map(|rank| (rank, property))
            })
            .collect::<Vec<_>>();
        let Some(nearest) = candidates.iter().map(|(rank, _)| *rank).min() else {
            return Ok(None);
        };
        candidates.retain(|(rank, _)| *rank == nearest);
        if candidates.len() > 1 {
            let dominated = (0..candidates.len())
                .filter(|&candidate| {
                    (0..candidates.len()).any(|other| {
                        candidate != other
                            && generic_property_more_specific(
                                &self.src,
                                &candidates[other].1,
                                &candidates[candidate].1,
                            )
                    })
                })
                .collect::<std::collections::HashSet<_>>();
            candidates = candidates
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| (!dominated.contains(&index)).then_some(candidate))
                .collect();
        }
        match candidates.as_mut_slice() {
            [(_, property)] => Ok(Some(specialize_property(
                &self.src,
                property.clone(),
                receiver,
            ))),
            _ => Err(AmbiguousExtensionProperty),
        }
    }

    /// Classify one interned type identity. The type-side counterpart of [`resolve_symbol`].
    pub fn classifier(
        &self,
        internal: TypeName,
    ) -> Option<std::sync::Arc<crate::libraries::LibraryType>> {
        self.src.classifier(internal)
    }

    pub fn classifier_associated_property(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<crate::libraries::PropertyInfo> {
        self.lib.classifier_associated_property(internal, name)
    }

    /// Provider-normalized classifier property visible from this resolver's lexical access site.
    /// The declaration may be realized however the platform chooses; this operation deals only in
    /// Kotlin property shape and source visibility.
    pub(crate) fn accessible_classifier_associated_property(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<crate::libraries::PropertyInfo> {
        let property = self.lib.classifier_associated_property(internal, name)?;
        let accessible = match property.visibility {
            Visibility::Public => true,
            Visibility::Internal => {
                self.module
                    .is_some_and(|module| module.classifier(property.owner).is_some())
                    || self.lib.internal_accessible(property.owner)
            }
            Visibility::PackagePrivate => self.package_private_member_accessible(property.owner),
            Visibility::Private => self.lexical_classes.iter().copied().any(|enclosing| {
                enclosing == property.owner
                    || std::iter::successors(enclosing.nested_owner(), |owner| owner.nested_owner())
                        .any(|owner| owner == property.owner)
            }),
            Visibility::Protected => self.lexical_classes.iter().copied().any(|enclosing| {
                crate::assignable::is_subtype(
                    &crate::assignable::TyCtx::new(),
                    &SourceOracle(&self.src),
                    Ty::obj_name(enclosing),
                    Ty::obj_name(property.owner),
                )
            }),
        };
        accessible.then_some(property)
    }

    /// The declared type of the member property `name` on `recv` — the property itself, with no accessor
    /// in the answer. A property is a declaration, not a method: whether the target realizes reading it
    /// through a method at all is not a resolution question, so a read must not be made to depend on
    /// finding one. Returns the selected declaration owner and its interface shape beside the logical
    /// property type so lowering does not rediscover either from a source-specific table. Nearest
    /// declaration wins, as for any member.
    ///
    /// Provider-normalized declarations take precedence over Java synthetic bean properties.
    /// Applicability still belongs to the use site because protected access depends on both the
    /// lexical class and receiver type.
    pub fn select_member_property(&self, recv: Ty, name: &str) -> Option<SelectedMemberProperty> {
        self.select_member_property_applicable_where(recv, name, |property| {
            (property.context_count == 0).then_some((true, 0))
        })
    }

    pub(crate) fn select_member_property_where(
        &self,
        recv: Ty,
        name: &str,
    ) -> Option<SelectedMemberProperty> {
        self.select_member_property_applicable_where(recv, name, |property| {
            (property.context_count == 0).then_some((true, 0))
        })
    }

    pub(crate) fn select_member_property_applicable_where(
        &self,
        recv: Ty,
        name: &str,
        property_applicable: impl Fn(&crate::libraries::PropertyInfo) -> Option<(bool, usize)>,
    ) -> Option<SelectedMemberProperty> {
        if recv.is_nullable() || recv.kotlin_class_internal().is_none() {
            return None;
        }
        // Walk normalized property declarations one classifier rung at a time. Providers have already
        // converted any target storage form into PropertyInfo plus opaque accessor identities.
        let mut queue = std::collections::VecDeque::from([recv]);
        let mut seen = std::collections::HashSet::new();
        let mut nearer: Vec<(std::sync::Arc<crate::libraries::LibraryType>, Ty)> = Vec::new();
        let mut synthetic_fallback: Option<(SelectedMemberProperty, bool)> = None;
        let mut inaccessible_declaration: Option<SelectedMemberProperty> = None;
        while let Some(current) = queue.pop_front() {
            let Some(internal) = current.kotlin_class_internal() else {
                continue;
            };
            if !seen.insert(internal) {
                continue;
            }
            let Some(shape) = self.src.classifier(internal) else {
                continue;
            };
            let (_, mut local_properties) =
                declared_callables(&self.src, &shape, current, name).into_parts();
            local_properties.overloads.extend(
                self.lib
                    .inherited_accessor_properties(&self.src, current, name)
                    .overloads,
            );
            crate::trace_compiler!(
                "resolve",
                "member property rung receiver={recv:?} current={current:?} owner={internal} name={name} properties={:?}",
                local_properties
                    .overloads
                    .iter()
                    .map(|property| (property.owner, property.kind, property.context_count))
                    .collect::<Vec<_>>(),
            );
            let local_property = local_properties
                .overloads
                .into_iter()
                .filter(|property| property.kind == PropKind::Member && property.receiver_rank == 0)
                .filter_map(|property| {
                    property_applicable(&property).map(|priority| (priority, property))
                })
                .max_by_key(|(priority, _)| *priority);
            if let Some(((accessible, _), mut property)) = local_property {
                crate::trace_compiler!(
                    "resolve",
                    "member property candidate receiver={current:?} owner={} name={name} getter={} classifier_formals={:?} declared={:?}",
                    property.owner,
                    property.getter.name,
                    shape.type_params,
                    property.ty,
                );
                // `declared_callables` has already applied `current` to this declaration. Applying
                // the classifier bindings again is not idempotent: after `Content<T>.value` becomes
                // a caller-owned type parameter, a second substitution sees that scoped parameter
                // as unbound and erases it to `Any`.
                let declared_ty = property.ty;
                let ty = nearer
                    .iter()
                    .find_map(|(shape, applied)| {
                        declared_callables(
                            &self.src,
                            shape.as_ref(),
                            *applied,
                            &property.getter.name,
                        )
                        .into_parts()
                        .0
                        .overloads
                        .into_iter()
                        .find(|function| function.semantic_params().is_empty())
                        .map(|function| function.callable.ret)
                    })
                    .unwrap_or(declared_ty);
                property.ty = ty;
                property.getter.ret = ty;
                crate::trace_compiler!(
                    "resolve",
                    "member property selected receiver={current:?} owner={} name={name} ty={ty:?} visibility={:?}",
                    property.owner,
                    property.visibility,
                );
                let interface = self
                    .src
                    .classifier(property.owner)
                    .is_some_and(|owner| owner.is_interface());
                let accessor_derived = property.accessor_derived;
                let selected = SelectedMemberProperty {
                    owner: property.owner,
                    ty,
                    interface,
                    visibility: property.visibility,
                    property: Some(property),
                };
                if accessor_derived {
                    synthetic_fallback.get_or_insert((selected, accessible));
                } else if accessible {
                    return Some(selected);
                } else if synthetic_fallback
                    .as_ref()
                    .is_some_and(|(_, accessible)| *accessible)
                {
                    return synthetic_fallback.map(|(property, _)| property);
                } else {
                    // An inaccessible Java declaration is not inherited at this use site and
                    // therefore cannot hide an accessible semantic property from a supertype
                    // (`HashMap.size`'s package-private storage vs `Map.size`). Keep it only as the
                    // diagnostic target if no accessible declaration is found above.
                    inaccessible_declaration.get_or_insert(selected);
                }
            }
            if shape.hidden_member_properties.contains(name) {
                return synthetic_fallback
                    .filter(|(_, accessible)| *accessible)
                    .map(|(property, _)| property)
                    .or(inaccessible_declaration);
            }
            nearer.push((shape, current));
            queue.extend(direct_supertypes(&self.src, current));
        }
        synthetic_fallback
            .filter(|(_, accessible)| *accessible)
            .map(|(property, _)| property)
            .or(inaccessible_declaration)
    }

    /// Resolve a name on a receiver to the thing it DENOTES — a member, a property, a companion/instance
    /// member, or a constructor — WITHOUT being told how the site uses it. The resolver does not care
    /// whether the caller is going to call it, read it, write it, or take a reference; it just says what
    /// the name is. The caller applies its own syntax to the returned [`Symbol`] (invoke the callable,
    /// read the property, use its setter, take a reference) and handles any mismatch itself (a `Type()`
    /// whose type has no constructor, an `invoke` on a property, …). `args` select a callable overload /
    /// constructor; they do not change WHAT the name is.
    pub fn resolve_symbol(
        &self,
        recv: SymRecv,
        name: &str,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<Symbol> {
        let args: Vec<CallArgKind> = args.iter().map(|&ty| CallArgKind::Typed(ty)).collect();
        self.select_symbol(recv, name, &args, type_args)
    }

    /// Select a callable referenced through a classifier (`Type::name`). Expected function
    /// parameters select the overload exactly as call arguments would; without an expected shape the
    /// name must denote one unique classifier callable. The returned declaration is the same model
    /// member used by `Type.name(...)` calls.
    pub(crate) fn classifier_callable_reference(
        &self,
        internal: TypeName,
        name: &str,
        expected_params: Option<&[Ty]>,
    ) -> Option<LibraryMember> {
        if let Some(parameters) = expected_params {
            let arguments = parameters
                .iter()
                .copied()
                .map(CallArgKind::Typed)
                .collect::<Vec<_>>();
            return select_companion_member(self.lib, &self.src, internal, name, &arguments, &[]);
        }

        let classifier = self.src.classifier(internal)?;
        let mut candidates = classifier
            .classifier_callables(internal)
            .into_iter()
            .filter(|member| member.name == name);
        let selected = candidates.next()?;
        candidates.next().is_none().then_some(selected)
    }

    /// Language-defined callables contributed by the classifier facet of a value-bearing classifier.
    /// The result uses the ordinary [`FunctionInfo`] candidate shape so callers can union it with the
    /// companion/object value's member family before overload selection.
    pub(crate) fn implicit_classifier_callable_candidates(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Vec<FunctionInfo> {
        let Some(classifier) = self.src.classifier(internal) else {
            return Vec::new();
        };
        classifier
            .classifier_callables(internal)
            .into_iter()
            .filter(|member| member.name == name)
            .filter_map(|member| {
                let implicit = member.implicit_classifier_callable?;
                let owner = member.owner.unwrap_or(internal);
                let mut candidate = FunctionInfo::classifier_member(FnKind::Member, owner, member);
                candidate.implicit_classifier_callable = Some(implicit);
                Some(candidate)
            })
            .collect()
    }

    /// The one callable family denoted by `Classifier.name(...)`. A value-bearing classifier
    /// contributes its ordinary members plus classifier-defined callables; every other classifier
    /// contributes the declarations in its classifier namespace. Both origins use `FunctionInfo`, so
    /// argument-dependent overload selection is a separate, shared operation.
    pub(crate) fn classifier_call_candidates(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<(Ty, Vec<FunctionInfo>)> {
        let receiver = classifier_value_receiver(&self.src, internal);
        let candidates = match receiver {
            Some(receiver) => {
                let mut candidates = self
                    .resolve_symbol(SymRecv::Value(receiver), name, &[], &[])
                    .map(Symbol::overloads)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|candidate| candidate.kind == FnKind::Member)
                    .collect::<Vec<_>>();
                candidates.extend(self.implicit_classifier_callable_candidates(internal, name));
                candidates
            }
            None => {
                let classifier = self.src.classifier(internal)?;
                let mut candidates = classifier
                    .classifier_callables(internal)
                    .into_iter()
                    .filter(|member| member.name == name)
                    .map(|member| FunctionInfo::classifier_member(FnKind::Member, internal, member))
                    .collect::<Vec<_>>();
                if candidates.is_empty() && self.lib.inherits_classifier_callables(internal) {
                    let mut seen = std::collections::HashSet::from([internal]);
                    let mut frontier = direct_supertypes(&self.src, Ty::obj_name(internal));
                    let mut rank = 1;
                    while !frontier.is_empty() {
                        let mut next = Vec::new();
                        for current in frontier {
                            let Some(owner) = current.kotlin_class_internal() else {
                                continue;
                            };
                            if !seen.insert(owner) {
                                continue;
                            }
                            if let Some(classifier) = self.src.classifier(owner) {
                                candidates.extend(
                                    classifier
                                        .classifier_callables(owner)
                                        .into_iter()
                                        .filter(|member| member.name == name)
                                        .map(|member| {
                                            let mut candidate = FunctionInfo::classifier_member(
                                                FnKind::Member,
                                                owner,
                                                member,
                                            );
                                            candidate.receiver_rank = rank;
                                            candidate
                                        }),
                                );
                                next.extend(direct_supertypes(&self.src, current));
                            }
                        }
                        if !candidates.is_empty() {
                            break;
                        }
                        frontier = next;
                        rank += 1;
                    }
                }
                candidates
            }
        };
        Some((
            receiver.unwrap_or_else(|| Ty::obj_name(internal)),
            candidates,
        ))
    }

    /// Select the callable denoted by `Classifier.name(...)` from the provider-normalized
    /// classifier namespace. This is the authoritative selection path for both compact signature
    /// solving and checked bodies; callers must not reconstruct classifier calls through the older
    /// companion-only symbol shortcut because that loses inherited associated callables.
    pub(crate) fn select_classifier_callable(
        &self,
        internal: TypeName,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
    ) -> Option<ResolvedMember> {
        let (receiver, candidates) = self.classifier_call_candidates(internal, name)?;
        self.select_classifier_callable_from_candidates(receiver, name, args, type_args, candidates)
    }

    /// Selection half of [`Self::select_classifier_callable`] for syntax owners that already
    /// materialized the candidate family while typing postponed arguments.
    pub(crate) fn select_classifier_callable_from_candidates(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        candidates: Vec<FunctionInfo>,
    ) -> Option<ResolvedMember> {
        self.select_receiver_callable_from_candidates(receiver, name, args, type_args, candidates)
    }

    /// Select one receiver callable from an already-materialized semantic overload rung. This is
    /// shared by classifier-associated calls and Pass-2 body-local member/super dispatch: both have
    /// already established the exact scope-tower rung, so selection must not reopen provider lookup.
    pub(crate) fn select_receiver_callable_from_candidates(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        candidates: Vec<FunctionInfo>,
    ) -> Option<ResolvedMember> {
        let callables = Callables::from_parts(
            FunctionSet {
                overloads: candidates,
            },
            PropertySet::default(),
        );
        let CandidateSelection::Selected((selected, _, result)) = self
            .select_receiver_function_with_params_tracking(
                receiver, name, args, type_args, &callables,
            )
        else {
            return None;
        };
        Some(self.commit_selected_member_function_result(receiver, selected, result))
    }

    pub(crate) fn top_level_candidates(&self, name: &str) -> Vec<FunctionInfo> {
        let functions = self
            .symbol_levels_in_scope(name)
            .into_iter()
            .map(function_set_from_symbols)
            .find(|functions| !functions.overloads.is_empty())
            .unwrap_or_default();
        crate::trace_compiler!(
            "resolve",
            "top-level candidate inventory {name}: {:?}",
            functions
                .overloads
                .iter()
                .map(|function| (
                    function.kind,
                    function.callable.owner.render(),
                    function.callable.name.as_str(),
                    function.semantic_params(),
                    function.semantic_receiver(),
                    function
                        .generic_sig
                        .as_ref()
                        .map(|signature| signature.formal_bounds.as_slice()),
                ))
                .collect::<Vec<_>>()
        );
        functions
            .into_top_level()
            // `$default` is a physical realization attached to its source declaration, never a
            // source-level overload candidate. Letting it into this family can select a synthetic
            // by declaration order and later append `$default` a second time.
            .filter(|function| !function.callable.default_call)
            .collect()
    }

    /// Select from a caller-provided projection of one top-level callable family.
    ///
    /// Contextual calls with explicitly named context parameters first project every declaration
    /// to its source-visible parameter order. The projection changes only the call shape; candidate
    /// identity and semantic types remain on [`FunctionInfo`]. Keeping the actual selection here
    /// makes that temporary projection use the same applicability, specificity, generic inference,
    /// and return specialization as an ordinary top-level call.
    pub(crate) fn select_top_level_function_candidates(
        &self,
        name: &str,
        candidates: Vec<FunctionInfo>,
        args: &[CallArgKind],
        type_args: &[Ty],
    ) -> Option<(FunctionInfo, LibraryCallable)> {
        self.select_top_level_function_candidates_with_visibility(
            name, candidates, args, type_args, None, false,
        )
    }

    /// Select a top-level callable with the contextual result constraint used by the ordinary
    /// checker. Signature solving calls this for typed initializers whose result participates in
    /// generic inference; it is the same candidate family and inference algorithm, not a graph-side
    /// approximation.
    pub(crate) fn select_top_level_function_candidates_with_expected(
        &self,
        name: &str,
        candidates: Vec<FunctionInfo>,
        args: &[CallArgKind],
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<(FunctionInfo, LibraryCallable)> {
        self.select_top_level_function_candidates_with_visibility(
            name, candidates, args, type_args, expected, false,
        )
    }

    /// Select from a caller-provided family when Kotlin's `INVISIBLE_REFERENCE` or
    /// `INVISIBLE_MEMBER` suppression makes otherwise hidden declarations applicable. Visibility
    /// is the only relaxed predicate; argument mapping, applicability, generic inference and
    /// specificity remain the ordinary top-level algorithm.
    pub(crate) fn select_top_level_function_candidates_ignoring_visibility(
        &self,
        name: &str,
        candidates: Vec<FunctionInfo>,
        args: &[CallArgKind],
        type_args: &[Ty],
    ) -> Option<(FunctionInfo, LibraryCallable)> {
        self.select_top_level_function_candidates_with_visibility(
            name, candidates, args, type_args, None, true,
        )
    }

    pub(crate) fn select_top_level_function_candidates_with_expected_ignoring_visibility(
        &self,
        name: &str,
        candidates: Vec<FunctionInfo>,
        args: &[CallArgKind],
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<(FunctionInfo, LibraryCallable)> {
        self.select_top_level_function_candidates_with_visibility(
            name, candidates, args, type_args, expected, true,
        )
    }

    fn select_top_level_function_candidates_with_visibility(
        &self,
        name: &str,
        candidates: Vec<FunctionInfo>,
        args: &[CallArgKind],
        type_args: &[Ty],
        expected: Option<Ty>,
        include_invisible: bool,
    ) -> Option<(FunctionInfo, LibraryCallable)> {
        let functions = FunctionSet {
            overloads: candidates,
        };
        let (selected, callable) = self.pick_top_level_with_visibility(
            name,
            &functions,
            args,
            type_args,
            expected,
            include_invisible,
        )?;
        if let Some(selected) = selected {
            return Some((selected, callable));
        }
        // A default bridge is a provider-owned physical callable rather than one of the projected
        // source candidates above. Preserve coordinate matching only for that legacy shape. An
        // ordinary selection carries its exact candidate identity out of the overload engine.
        let selected = functions.top_level().find(|candidate| {
            candidate.callable.owner == callable.owner
                && candidate.callable.name == callable.name
                && candidate.callable.descriptor == callable.descriptor
        })?;
        Some((selected.clone(), callable))
    }

    /// Normalize a Kotlin function type or SAM classifier to the callable shape used to check a
    /// postponed lambda. Candidate selection still retains the nominal SAM target separately; this
    /// operation exposes only its semantic input/result contract.
    pub(crate) fn functional_expectation(&self, target: Ty) -> Option<Ty> {
        if matches!(target.non_null(), Ty::Fun(_)) {
            return Some(target.non_null());
        }
        let sam = semantic_sam_signature(&self.src, target)?;
        Some(Ty::fun_with_shape(
            sam.params,
            sam.ret,
            sam.context_count,
            sam.has_receiver,
            sam.suspend,
        ))
    }

    pub(crate) fn select_symbol(
        &self,
        recv: SymRecv,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
    ) -> Option<Symbol> {
        match recv {
            SymRecv::Value(ty) | SymRecv::ImplicitValue(ty) => {
                // Resolve every facet the name supports on this receiver; a name can support several (a
                // Java zero-arg method is a property read AND a callable). Each facet is exactly the
                // former per-use resolution, so the caller's chosen facet behaves as before.
                let callables = self.receiver_callables(ty, name);
                let mut call_ambiguous = false;
                let selected_call = select_overload_tracking_with_functions(
                    self.lib,
                    ty,
                    name,
                    args,
                    type_args,
                    SelectionMode::Receiver,
                    ExtCtx {
                        fn_scope: self.fn_scope,
                        source: &self.src,
                    },
                    Some(callables.functions()),
                    &mut call_ambiguous,
                    IndexedConvention::Ordinary,
                );
                let call = selected_call
                    .as_ref()
                    .filter(|selected| selected.kind == FnKind::Member)
                    .cloned()
                    .map(|selected| {
                        self.materialize_member_function(ty, args, type_args, selected)
                    });
                let member_dispatch = receiver_allows_member_dispatch(ty);
                let member_property = member_dispatch
                    .then(|| member_property_from_callables(&callables))
                    .flatten();
                let read = member_property
                    .as_ref()
                    .and_then(|property| member_property_read_from_declaration(ty, property));
                // Read and write facets of an inherited intersection need not come from the same
                // declaration. `C : A, B` may inherit `A.val x` and `B.var x` at the same receiver
                // rung; Kotlin's fake override is readable through either getter and writable
                // through B's setter. A nearer declared `val` still blocks a farther inherited
                // setter, so restrict the setter search to the minimum property receiver rank.
                let write = member_property_write_from_callables(&callables);
                let method_ref = member_dispatch
                    .then(|| select_instance_reference_from_functions(ty, callables.functions()))
                    .flatten();
                let property_ref = member_property
                    .as_ref()
                    .filter(|_| ty.kotlin_class_internal() != Some(crate::types::wk::any()))
                    .and_then(|property| {
                        build_property_reference_from_declaration(ty, name, property)
                    });
                // The classpath EXTENSION callable for `recv.name(args)`: one extension selection (admits
                // `@InlineOnly` splice candidates — a plain and an inline call resolve identically, only the
                // emitter differs). A same-module extension emits through the module path, not a library
                // callable, so it is dropped here.
                let selected_extension = selected_call
                    .as_ref()
                    .filter(|selected| selected.is_extension())
                    .filter(|selected| !matches!(selected.callable.origin, Origin::Module { .. }));

                let extension_call = selected_extension.as_ref().and_then(|overload| {
                    let semantic_params = overload.semantic_params();
                    let arg_tys = args
                        .iter()
                        .enumerate()
                        .map(|(index, argument)| {
                            semantic_params.get(index).copied().map_or_else(
                                || argument.ty(),
                                |parameter| argument.type_for(parameter),
                            )
                        })
                        .collect::<Vec<_>>();
                    self.build_extension_callable(name, ty, &arg_tys, type_args, overload)
                });
                // The emit handle is origin-filtered; the RESULT is not. Selection has already chosen
                // this overload, and which provider declared it does not change what it returns —
                // but the handle answers first when it exists, so only ask when it cannot.
                let extension_result = extension_call
                    .is_none()
                    .then(|| {
                        selected_call
                            .as_ref()
                            .filter(|selected| selected.is_extension())
                            .and_then(|overload| {
                                self.extension_call_result(ty, args, type_args, overload)
                            })
                    })
                    .flatten();
                crate::trace_compiler!(
                    "resolve",
                    "extension symbol name={name} receiver={ty:?} selected={} realized={}",
                    selected_extension.is_some(),
                    extension_call.is_some(),
                );
                let extension_property = self
                    .select_extension_property_from_callables(ty, name, &callables)
                    .ok()
                    .flatten();
                // EVERY overload named `name` applicable to the receiver: instance members and operators
                // (the receiver-aware member query, federated over module + libraries) UNION the in-scope
                // extension functions whose declared receiver is in the receiver's supertype closure. This
                // is the whole candidate family `select_overload` picks from — a caller inspecting the set
                // (named-argument mapping, default-argument selection, return agreement, member-vs-extension
                // dispatch) reads it here and filters by `kind`/`receiver_rank` as it needs.
                let mut overloads = callables
                    .functions()
                    .iter()
                    .filter(|overload| member_dispatch && overload.kind == FnKind::Member)
                    .cloned()
                    .collect::<Vec<_>>();
                if callables.functions().iter().any(FunctionInfo::is_extension) {
                    let recv_mro = ReceiverMro::new(&self.src, ty);
                    overloads.extend(callables.functions().iter().cloned().filter_map(
                        |mut overload| {
                            if !overload.is_extension() {
                                return None;
                            }
                            overload.receiver_rank =
                                recv_mro.rank(&self.src, overload.semantic_receiver()?)?;
                            Some(overload)
                        },
                    ));
                }
                if call.is_none()
                    && read.is_none()
                    && write.is_none()
                    && method_ref.is_none()
                    && property_ref.is_none()
                    && overloads.is_empty()
                    && extension_call.is_none()
                    && extension_result.is_none()
                    && extension_property.is_none()
                {
                    return None;
                }
                Some(Symbol::Member(Box::new(MemberFacets {
                    call,
                    read,
                    write,
                    method_ref,
                    property_ref,
                    values: Vec::new(),
                    overloads,
                    top_level_call: None,
                    extension_call,
                    extension_result,
                    extension_property,
                })))
            }
            SymRecv::TopLevel => {
                // A receiver-less name: its top-level (and same-facade extension) overloads over this
                // resolver's scope. A fully-qualified `pkg.name(args)` resolves by constructing a resolver
                // scoped to `pkg` (the package is scope, not part of the name) and calling this. The caller
                // reads `overloads` to inspect the family, or `top_level_call` for the arg/type-arg selected
                // callable (default/vararg-aware) ready to emit.
                let levels = self.symbol_levels_in_scope(name);
                let fs = levels
                    .iter()
                    .map(|symbols| function_set_from_symbols(symbols.clone()))
                    .find(|functions| !functions.overloads.is_empty())
                    .unwrap_or_default();
                crate::trace_compiler!(
                    "signature_inference",
                    "top-level candidates={name} {:?}",
                    fs.overloads
                        .iter()
                        .map(|candidate| (
                            candidate.kind,
                            candidate.semantic_params(),
                            candidate.call_sig.vararg_index,
                            candidate.call_sig.required,
                            candidate.generic_sig.as_ref().map(|signature| (
                                &signature.formals,
                                &signature.params,
                                signature.ret,
                            )),
                        ))
                        .collect::<Vec<_>>(),
                );
                let top_level_call = self.pick_top_level(name, &fs, args, type_args, None);
                let overloads = fs.overloads;
                let values = levels
                    .into_iter()
                    .map(|symbols| {
                        symbols
                            .into_iter()
                            .flat_map(|symbols| property_overloads(&symbols.callables))
                            .filter(|property| property.kind == PropKind::TopLevel)
                            .filter(|property| source_property_visible(self.lib, property))
                            .collect::<Vec<_>>()
                    })
                    .find(|properties| !properties.is_empty())
                    .unwrap_or_default();
                if overloads.is_empty() && top_level_call.is_none() && values.is_empty() {
                    return None;
                }
                Some(Symbol::Member(Box::new(MemberFacets {
                    call: None,
                    read: None,
                    write: None,
                    method_ref: None,
                    property_ref: None,
                    values,
                    overloads,
                    top_level_call,
                    extension_call: None,
                    extension_result: None,
                    extension_property: None,
                })))
            }
            SymRecv::Type(internal) => self.select_symbol(
                SymRecv::TypeName(crate::types::type_name(internal)),
                name,
                args,
                type_args,
            ),
            SymRecv::TypeName(internal) => {
                if name.is_empty() {
                    // `Type(args)` — select the semantic constructor overload first, then attach the
                    // physical default invocation required by that application. This is one callable
                    // selection result: consumers never retry a rejected direct constructor as a
                    // separate name-resolution fallback.
                    select_constructor_call(self.lib, &self.src, internal, args)
                        .map(Symbol::Constructor)
                } else {
                    // `Type.name(args)` — an object/companion instance member, else a static/companion
                    // member. The resolver discovers which.
                    select_instance_member_name(self.lib, &self.src, internal, name, args)
                        .map(Symbol::Instance)
                        .or_else(|| {
                            select_companion_member(
                                self.lib, &self.src, internal, name, args, type_args,
                            )
                            .map(Symbol::Companion)
                        })
                }
            }
        }
    }

    /// Resolve `super.f(…)` through a provider-complete applied superclass/interface projection.
    /// Active body-local classifiers use the checker's transient member rung instead; ordinary
    /// module and dependency classifiers arrive here after providers have normalized overrides.
    pub(crate) fn select_super_instance(
        &self,
        recv: Ty,
        name: &str,
        args: &[CallArgKind],
    ) -> Option<LibraryMember> {
        let selected = select_instance_info(self.lib, &self.src, recv, name, args)?;
        if selected.flags.is_abstract {
            return None;
        }
        let ret = selected.ret.apply(selected.callable.ret);
        Some(selected.member_with_return(ret))
    }

    /// Overload-resolve a top-level call against an already-built [`FunctionSet`] (from the resolver's
    /// scope). The [`SymRecv::TopLevel`] arm of [`Self::resolve_symbol`] uses this to fill `top_level_call`.
    fn pick_top_level(
        &self,
        name: &str,
        fs: &FunctionSet,
        args: &[CallArgKind],
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<LibraryCallable> {
        self.pick_top_level_with_visibility(name, fs, args, type_args, expected, false)
            .map(|(_, callable)| callable)
    }

    fn top_level_callable_accessible(&self, candidate: &FunctionInfo) -> bool {
        candidate.visibility == Visibility::Public
            || (candidate.visibility == Visibility::Internal
                && self.lib.internal_accessible(candidate.callable.owner))
    }

    fn pick_top_level_with_visibility(
        &self,
        name: &str,
        fs: &FunctionSet,
        args: &[CallArgKind],
        type_args: &[Ty],
        expected: Option<Ty>,
        include_invisible: bool,
    ) -> Option<(Option<FunctionInfo>, LibraryCallable)> {
        // Exact/default probes see ordinary runtime types; `args` separately drives adaptation.
        let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
        let mut parsed: Vec<(&FunctionInfo, Vec<Ty>, Ty, GSigBinds)> = fs
            .top_level()
            .filter(|candidate| include_invisible || self.top_level_callable_accessible(candidate))
            .filter(|o| top_level_exact_parameters_admit(o, args, type_args))
            .filter_map(|o| {
                let semantic = o.semantic_signature();
                let mut bindings = seeded_gsig_binds(&semantic, type_args);
                let value_count = semantic.params.len().saturating_sub(o.context_count);
                let inferred = infer_generic_call_bindings_from_symbols(
                    &self.src,
                    &semantic,
                    args.iter()
                        .enumerate()
                        .filter_map(|(argument_index, argument)| {
                            if argument.is_omitted_default() {
                                return None;
                            }
                            let value_parameter = match o.call_sig.vararg_index {
                                Some(vararg)
                                    if argument_index >= vararg.saturating_sub(o.context_count) =>
                                {
                                    vararg.saturating_sub(o.context_count)
                                }
                                _ if argument_index < value_count => argument_index,
                                _ => return None,
                            };
                            let parameter = o.context_count + value_parameter;
                            let declared = *semantic.params.get(parameter)?;
                            let whole_array = argument.is_spread();
                            let expected =
                                if o.call_sig.vararg_index == Some(parameter) && !whole_array {
                                    declared.array_read_elem().unwrap_or(declared)
                                } else {
                                    declared
                                };
                            Some((
                                parameter,
                                argument.inference_type(&self.src, expected),
                                whole_array,
                            ))
                        }),
                    o.call_sig.vararg_index,
                );
                for (formal, actual) in inferred {
                    bindings.entry(formal).or_insert(actual);
                }
                if let Some(expected) = expected {
                    if let Some(inferred_return) = infer_generic_return_bindings_from_symbols(
                        &self.src,
                        &semantic,
                        expected,
                        |actual, bound| resolution_subtype(&self.src, actual, bound),
                    ) {
                        for (formal, actual) in inferred_return {
                            bindings.entry(formal).or_insert(actual);
                        }
                    }
                }
                if !generic_bindings_satisfy_bounds(&semantic, &bindings, |actual, bound| {
                    resolution_subtype(&self.src, actual, bound)
                }) {
                    crate::trace_compiler!(
                        "resolve",
                        "reject top-level candidate {}: bindings={:?}, bounds={:?}",
                        name,
                        bindings,
                        semantic.formal_bounds
                    );
                    return None;
                }
                let params = apply_platform_call_parameter_nullability(
                    semantic
                        .params
                        .iter()
                        .map(|parameter| ty_subst_keep_unbound(*parameter, &bindings))
                        .collect(),
                    &o.call_sig.platform_nullable_params,
                    &arg_tys,
                    o.call_sig.vararg,
                );
                // Leading CONTEXT parameters are supplied implicitly by the caller, never
                // positionally — matching and arity checks see only the value parameters.
                let value_params = params[o.context_count.min(params.len())..].to_vec();
                Some((o, value_params, o.callable.ret, bindings))
            })
            .collect();
        let fits = |p: &Ty, a: &CallArgKind| {
            if a.is_omitted_default() {
                return true;
            }
            if a.is_lambda_literal() && a.ty() == Ty::Error {
                return untyped_lambda_pertinent(self.lib, &self.src, *p);
            }
            let function = a.function_type().unwrap_or_else(|| a.ty());
            let sam = (a.is_lambda_literal() || a.function_type().is_some())
                && sam_arg_matches(self.lib, &self.src, *p, function);
            sam || self.arg_fits_or_subtype(p, &a.type_for(*p))
        };
        let adapts = |p: &Ty, a: &CallArgKind, _i: usize| a.adapts_integer_literal_to(*p);

        // Kotlin removes low-priority declarations only when an ordinary declaration is actually
        // applicable. Do that once, before specificity, so every exact/literal/default/vararg branch
        // below sees one coherent candidate tier.
        let applicable = |candidate: &(&FunctionInfo, Vec<Ty>, Ty, GSigBinds)| {
            let (_, params, ..) = candidate;
            fixed_parameter_shape(params, args, |_, parameter, argument| {
                fits(parameter, argument)
            })
            .is_some()
                || omitted_parameter_shape(params, args, |position, parameter, argument| {
                    fits(parameter, argument) || adapts(parameter, argument, position)
                })
                .is_some()
                || vararg_parameter_shape(params, args, |position, parameter, argument| {
                    fits(parameter, argument) || adapts(parameter, argument, position)
                })
                .is_some()
        };
        if parsed.iter().any(|candidate| {
            !candidate.0.annotations.contains(&crate::types::type_name(
                "kotlin/internal/LowPriorityInOverloadResolution",
            )) && applicable(candidate)
        }) {
            parsed.retain(|candidate| {
                !candidate.0.annotations.contains(&crate::types::type_name(
                    "kotlin/internal/LowPriorityInOverloadResolution",
                ))
            });
        }

        let pick = if let Some(exact) = parsed.iter().find(|(_, params, ..)| {
            params.len() == args.len()
                && params
                    .iter()
                    .zip(args)
                    .all(|(parameter, argument)| *parameter == argument.type_for(*parameter))
        }) {
            Some(exact)
        } else {
            let literal_pick = match integer_literal_overload(
                parsed
                    .iter()
                    .map(|entry @ (_, params, ..)| (params.clone(), entry)),
                args,
                |_, param, arg| fits(param, arg),
                |_position, left, right, arg| {
                    parameter_at_least_as_specific(&self.src, left, right, arg)
                        || resolution_subtype(&self.src, left, right)
                },
                |_, _| false,
            ) {
                CandidateSelection::Selected(entry) => Some(entry),
                CandidateSelection::Ambiguous => return None,
                CandidateSelection::None => None,
            };
            match literal_pick {
                Some(entry) => Some(entry),
                None => match unique_most_specific(
                    parsed.iter().filter_map(|entry @ (_, params, ..)| {
                        fixed_parameter_shape(params, args, |_, param, arg| fits(param, arg))
                            .map(|shape| (shape, entry))
                    }),
                    |_, left, right| resolution_subtype(&self.src, left, right),
                ) {
                    CandidateSelection::Selected(entry) => Some(entry),
                    CandidateSelection::Ambiguous => return None,
                    CandidateSelection::None => match unique_most_specific(
                        parsed.iter().filter_map(|entry @ (_, params, ..)| {
                            vararg_parameter_shape(params, args, |i, param, arg| {
                                fits(param, arg) || adapts(param, arg, i)
                            })
                            .map(|shape| (shape, entry))
                        }),
                        |_, left, right| resolution_subtype(&self.src, left, right),
                    ) {
                        CandidateSelection::Selected(entry) => Some(entry),
                        CandidateSelection::Ambiguous => return None,
                        CandidateSelection::None => None,
                    },
                },
            }
        };

        if pick.is_none() {
            if let Some(c) =
                self.select_top_level_default_callable(name, &arg_tys, type_args, expected)
            {
                crate::trace_compiler!(
                    "resolve",
                    "top-level {name} args={arg_tys:?} -> {}.{}{} default inline={:?}",
                    c.owner.render(),
                    c.name,
                    c.descriptor,
                    c.inline
                );
                return Some((None, c));
            }
        }

        let (o, params, ret, bindings) = pick?;
        let c = &o.callable;
        let mut vararg_elem = None;
        let ret_ty = o
            .generic_sig
            .as_ref()
            .map(|gsig| {
                // A vararg call binds `T` from the ELEMENTS, not from the array param. Detect it by the
                // trailing array parameter receiving element-wise args — NOT merely by arity: a SINGLE
                // element (`listOf(pair)`) has `params.len() == args.len()`, yet still spreads into the
                // vararg, so a plain `zip` would unify `Array<T>` against the non-array `Pair` and leave
                // `T` unbound (→ `List<Any>`). A spread (`listOf(*arr)`) passes the array itself — same
                // arity AND the last arg IS the array param — so it is not a vararg here.
                let vararg = params.last().is_some_and(|p| p.array_elem().is_some())
                    && (params.len() != arg_tys.len() || arg_tys.last() != params.last());
                if vararg && !gsig.params.is_empty() {
                    let fixed = gsig.params.len() - 1;
                    if let Some(inner) = gsig.params[fixed].array_read_elem() {
                        vararg_elem = Some(ty_subst(inner, bindings));
                    }
                }
                ty_subst(gsig.ret, bindings)
            })
            .unwrap_or(*ret);
        // The signature owns post-substitution behavior, so every provider and callable origin
        // reaches the same realization path after ordinary inference. A suspend callable's physical
        // `Object` result is not its source return; its generic metadata remains authoritative too.
        let ret_ty = o.ret.apply(
            o.generic_sig
                .as_ref()
                .map_or(ret_ty, |sig| sig.apply_return_policy(self.lib, ret_ty)),
        );

        crate::trace_compiler!(
            "resolve",
            "top-level {name} args={arg_tys:?} -> {}.{}{} inline={:?}",
            c.owner.render(),
            c.name,
            c.descriptor,
            c.inline
        );
        let callable = LibraryCallable {
            // A function with leading CONTEXT parameters emits the FULL parameter list (context
            // included) so the lowerer can prepend the implicit context sources; the matched
            // `params` were value-only for arity purposes. Without context, the matched list is
            // the full list (platform-nullability applied).
            params: if o.context_count > 0 {
                c.params.clone()
            } else {
                params.clone()
            },
            ret: ret_ty,
            physical_ret: *ret,
            default_call: false,
            vararg_elem,
            vararg_index: vararg_elem.and(o.call_sig.vararg_index),
            ..c.clone()
        };
        Some((Some((*o).clone()), callable))
    }

    /// Shape a selected extension overload into a [`LibraryCallable`] for the call site. An EXACT call binds
    /// the generic return directly. A call that OMITS trailing defaults picks the emit form by a Kotlin ABI
    /// fact — an `inline` function has no `$default` synthetic (kotlinc materializes defaults by inlining),
    /// so it becomes a MUST-INLINE splice; a non-`inline` one binds the `name$default` synthetic (the
    /// backend appends placeholders + a bit-mask).
    /// Everything about a call to a selected extension overload that is independent of HOW the call
    /// is emitted: the binding receiver, the context-stripped value parameters, the argument list
    /// normalized for a spread, and the resulting type.
    ///
    /// The result must be computed exactly once. Recomputing it beside the emit builder produced two
    /// definitions of "what does this extension return" that drifted immediately: a `vararg`
    /// extension zipped its arguments against the ARRAY parameter, so `Src().firstOf("a", "bb")`
    /// never bound `T` and typed the property `Any` — the class then carried `Ljava/lang/Object;`
    /// where kotlinc writes `Ljava/lang/String;`, which a downstream module cannot use, and no box
    /// test could see it because the program still ran.
    fn extension_call_shape(
        &self,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
        o: &FunctionInfo,
    ) -> ExtensionCallShape {
        let binding_receiver = self.extension_binding_receiver(receiver, o);
        let vparams = logical_value_params(&self.src, o, binding_receiver, type_args);
        // A `vararg` overload SPREAD over the trailing arguments is NOT a defaulted call — the caller
        // builds the packed array and the physical argument list still ends in it. Comparing raw arity
        // reads `"ab..!!".trimEnd('!', '.')` (2 arguments, 1 array parameter) as an omitted-default call
        // and then hunts for a `$default` synthetic that does not exist, so the whole call fell through
        // unresolved. Normalize to the PHYSICAL shape — fixed prefix plus the array — so the arity test
        // below sees what is actually emitted. `f(charArray)` passes the array THROUGH and is untouched.
        let spread = o
            .call_sig
            .vararg_index
            .filter(|&slot| args.len() > slot)
            .and_then(|slot| {
                let array = *vparams.get(slot)?;
                let element = array.array_read_elem()?;
                // Positional arguments beginning at a non-final vararg all belong to that
                // vararg; later parameters can only be supplied by name. Preserve an array
                // argument for the already-normalized spread/pass-through shape — sole, or an
                // exact-arity list whose vararg position already holds the array (the slot-mapped
                // named form, `segd("O", "K", flag = true)`, arrives here pre-packed).
                if args.get(slot) == Some(&array)
                    && (args.len() == slot + 1 || args.len() == vparams.len())
                {
                    return None;
                }
                let mut physical = args[..slot].to_vec();
                physical.push(array);
                Some((physical, slot, element))
            });
        let spread_slot = spread.as_ref().map(|(_, slot, elem)| (*slot, *elem));
        let args: &[Ty] = spread.as_ref().map_or(args, |(a, _, _)| a.as_slice());
        if vparams.len() == args.len() {
            let semantic = o.semantic_signature();
            let (ret_ty, binds) =
                bind_ext_ret_tracking(&self.src, &semantic, binding_receiver, args, type_args);
            let ret_ty2 = specialized_extension_return(self.lib, o, ret_ty);
            let determined =
                extension_bindings_are_determinate(&semantic, binding_receiver, args, &binds);
            return ExtensionCallShape {
                vparams,
                args: args.to_vec(),
                spread_slot,
                exact: true,
                ret: ret_ty2,
                determined,
            };
        }
        // Defaulted call — omitted trailing/middle params. Bind the return with default-aware alignment.
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        let ret_ty = specialized_extension_return(
            self.lib,
            o,
            bind_defaulted_ext_ret(
                &self.src,
                o,
                binding_receiver,
                args,
                type_args,
                trailing_lambda,
            ),
        );
        ExtensionCallShape {
            vparams,
            args: args.to_vec(),
            spread_slot,
            exact: false,
            ret: ret_ty,
            // A defaulted or vararg call aligns its arguments differently for the emit form; the
            // default-aware binder answers what to emit, not whether inference succeeded, so a
            // generic overload reached this way reports nothing.
            determined: o.semantic_signature().formals.is_empty(),
        }
    }

    pub(crate) fn build_extension_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
        o: &FunctionInfo,
    ) -> Option<LibraryCallable> {
        let shape = self.extension_call_shape(receiver, args, type_args, o);
        let ExtensionCallShape {
            vparams,
            spread_slot,
            exact,
            ret: ret_ty,
            ..
        } = &shape;
        let spread_slot = *spread_slot;
        let args: &[Ty] = &shape.args;
        if *exact {
            let c = &o.callable;
            let ret_ty2 = *ret_ty;
            crate::trace_compiler!(
                "resolve",
                "bind_extension_callable {}.{} gsig={} type_args={type_args:?} ret={ret_ty2:?}",
                c.owner.render(),
                c.name,
                o.generic_sig.is_some()
            );
            // `vararg_elem` is what tells the LOWERER to build the packed array. It must come from the
            // resolved overload's own `vararg` flag, never from the shape of the parameter list: plenty of
            // non-vararg extensions END in an array parameter (`Array<out T>?.contentEquals(other:
            // Array<out T>?)`), and packing one of those wraps the caller's array in a fresh 1-element
            // array — a silent miscompile the box corpus caught as `collectionLiterals/array.kt`.
            let mut c = callable_with_return(c, ret_ty2, false);
            if let Some((slot, element)) = spread_slot {
                c.vararg_elem = Some(element);
                c.vararg_index = Some(slot);
            }
            return Some(c);
        }
        let ret_ty = *ret_ty;
        // Prefer a real `name$default` synthetic when it exists — even for an `inline` function. Many
        // `inline` stdlib/coroutine functions (`Mutex.withLock`) also emit a `$default` callable (the
        // `$$forInline` variant is what kotlinc splices); calling `$default` threads the `Continuation`
        // through the ordinary suspend machinery instead of splicing a suspend body. Splice (MUST-INLINE)
        // only when there is NO `$default` synthetic — a genuine `@InlineOnly` callee with no call target.
        if let Some(c) = selected_default_callable(o) {
            crate::trace_compiler!(
                "resolve",
                "extension defaulted ($default) {name} recv={receiver:?} args={args:?} -> {}.{}{} ret={ret_ty:?}",
                c.owner.render(),
                c.name,
                c.descriptor
            );
            let mut c = callable_with_return(&c, ret_ty, true);
            // An element-form vararg call reaching the `$default` (`split('.')`): tell the
            // lowerer which element type to PACK before the mask machinery — without it the
            // loose element lowers straight into the array slot (a VerifyError). A spread was
            // already normalized to the physical array shape, so it records unconditionally;
            // otherwise fall back to the provider's vararg flag (skipped for `suspend`, whose
            // `$default` threads a Continuation this shape does not model).
            if let Some((slot, element)) = spread_slot {
                c.vararg_elem = Some(element);
                c.vararg_index = Some(slot);
            } else if !o.flags.suspend {
                record_default_vararg_slot(&mut c, o.call_sig.vararg_index, vparams, args);
            }
            return Some(c);
        }
        if o.flags.inline.can_inline() {
            let mut callable = callable_with_return(&o.callable, ret_ty, true);
            callable.inline = crate::libraries::InlineKind::MustInline;
            crate::trace_compiler!(
                "resolve",
                "extension defaulted (inline) {name} recv={receiver:?} args={args:?} -> {}.{}{} ret={ret_ty:?}",
                callable.owner.render(),
                callable.name,
                callable.descriptor
            );
            return Some(callable);
        }
        None
    }

    /// The type a call to this selected extension overload produces.
    ///
    /// This is the return half of [`Self::build_extension_callable`] without the emit half: the same
    /// binding receiver, the same generic binding from the arguments, the same specialization. It is
    /// well defined for every origin, whereas the callable that CARRIES it exists only where the
    /// call emits through a library declaration.
    fn extension_call_result(
        &self,
        receiver: Ty,
        args: &[CallArgKind],
        type_args: &[Ty],
        overload: &FunctionInfo,
    ) -> Option<Ty> {
        // The SAME shape the emit handle is built from — the argument normalization a `vararg` or a
        // defaulted call needs is part of what the call returns, not part of how it is emitted.
        let semantic_params = overload.semantic_params();
        let arg_tys = args
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                semantic_params
                    .get(index)
                    .copied()
                    .map_or_else(|| argument.ty(), |parameter| argument.type_for(parameter))
            })
            .collect::<Vec<_>>();
        let shape = self.extension_call_shape(receiver, &arg_tys, type_args, overload);
        shape.determined.then_some(shape.ret)
    }

    fn extension_binding_receiver(&self, receiver: Ty, overload: &FunctionInfo) -> Ty {
        overload
            .semantic_receiver()
            .and_then(|declared| {
                ReceiverMro::new(&self.src, receiver).binding_receiver(&self.src, declared)
            })
            .unwrap_or(receiver)
    }

    pub(crate) fn build_extension_callable_for_slots(
        &self,
        name: &str,
        receiver: Ty,
        type_args: &[Ty],
        o: &FunctionInfo,
        slots: &[Option<Ty>],
    ) -> Option<LibraryCallable> {
        let binding_receiver = self.extension_binding_receiver(receiver, o);
        if !self.extension_slots_admit_bounds(receiver, type_args, o, slots) {
            return None;
        }
        let slot_arguments = slots
            .iter()
            .enumerate()
            .map(|(index, slot)| {
                slot.map(CallArgKind::Typed).unwrap_or_else(|| {
                    CallArgKind::Typed(o.semantic_params().get(index).copied().unwrap_or(Ty::Error))
                })
            })
            .collect::<Vec<_>>();
        let vparams =
            logical_call_params(&self.src, o, binding_receiver, &slot_arguments, type_args);
        if vparams.len() != slots.len() {
            return None;
        }
        for (index, (param, slot)) in vparams.iter().zip(slots).enumerate() {
            if let Some(arg) = slot {
                // The slot map stores a vararg's arguments in ELEMENT form (`segd("O", "K",
                // flag = true)` keeps `"O"` at the vararg slot), so that slot admits the element
                // type as well as the array itself.
                let vararg_element_fits = o.call_sig.vararg_index == Some(index)
                    && param
                        .array_read_elem()
                        .is_some_and(|element| self.arg_fits_or_subtype(&element, arg));
                if !vararg_element_fits && !self.arg_fits_or_subtype(param, arg) {
                    return None;
                }
            }
        }
        let directly_realizable = slots
            .iter()
            .enumerate()
            .all(|(index, slot)| slot.is_some() || o.call_sig.vararg_index == Some(index));
        if directly_realizable {
            let mut args = slots
                .iter()
                .enumerate()
                .map(|(index, slot)| slot.unwrap_or(vparams[index]))
                .collect::<Vec<_>>();
            // Present the vararg slot in its ARRAY form: the packed array is what the emitted
            // call passes, and `build_extension_callable` reads an exact `vparams` match as the
            // already-normalized shape. An ELEMENT-form or OMITTED vararg means the call site must
            // PACK (one or zero elements), so stamp the vararg slot/element on the callable for the
            // slot-aware checked-FIR construction.
            let mut packed_vararg = None;
            if let Some(vararg) = o.call_sig.vararg_index {
                if let (Some(param), Some(arg)) = (vparams.get(vararg), args.get_mut(vararg)) {
                    if slots.get(vararg).is_some_and(Option::is_none) {
                        packed_vararg = param.array_read_elem().map(|element| (vararg, element));
                    } else if *arg != *param && param.array_elem().is_some() {
                        *arg = *param;
                        packed_vararg = param.array_read_elem().map(|element| (vararg, element));
                    }
                }
            }
            let mut callable =
                self.build_extension_callable(name, receiver, &args, type_args, o)?;
            if let Some((vararg, element)) = packed_vararg {
                callable.vararg_elem = Some(element);
                callable.vararg_index = Some(vararg);
            }
            return Some(callable);
        }

        let ret_ty = specialized_extension_return(
            self.lib,
            o,
            bind_defaulted_ext_ret_slots(&self.src, o, binding_receiver, slots, type_args),
        );
        if let Some(c) = selected_default_callable(o) {
            crate::trace_compiler!(
                "resolve",
                "extension defaulted slots ($default) {name} recv={receiver:?} slots={slots:?} -> {}.{}{} ret={ret_ty:?}",
                c.owner.render(),
                c.name,
                c.descriptor
            );
            let mut callable = callable_with_return(&c, ret_ty, true);
            if let Some(index) = o.call_sig.vararg_index {
                callable.vararg_elem = vparams
                    .get(index)
                    .and_then(|parameter| parameter.array_read_elem());
                callable.vararg_index = callable.vararg_elem.map(|_| index);
            }
            return Some(callable);
        }
        if o.flags.inline.can_inline() {
            let mut callable = callable_with_return(&o.callable, ret_ty, true);
            callable.inline = crate::libraries::InlineKind::MustInline;
            return Some(callable);
        }
        None
    }

    pub(crate) fn extension_slots_admit_bounds(
        &self,
        receiver: Ty,
        type_args: &[Ty],
        overload: &FunctionInfo,
        slots: &[Option<Ty>],
    ) -> bool {
        generic_bounds_admit_slots(
            &self.src,
            overload.generic_sig.as_ref(),
            &overload.call_sig,
            self.extension_binding_receiver(receiver, overload),
            slots,
            type_args,
        )
    }

    fn arg_fits_or_subtype(&self, param: &Ty, arg: &Ty) -> bool {
        arg_fits_source(self.lib, &self.src, param, arg)
    }

    /// Map supplied source arguments to the base declaration's parameter slots without consulting
    /// the `$default` bridge's erased parameter types. Type applicability is checked after generic
    /// inference against the semantic base signature.
    fn default_argument_mapping(
        &self,
        info: &FunctionInfo,
        parameter_count: usize,
        args: &[Ty],
        slots: Option<&[usize]>,
    ) -> Option<Vec<(usize, usize)>> {
        if info.context_count != 0 || args.len() > parameter_count {
            return None;
        }
        let sig = &info.call_sig;
        if let Some(slots) = slots {
            if slots.len() != args.len()
                || slots.iter().any(|parameter| *parameter >= parameter_count)
            {
                return None;
            }
            if !sig.param_defaults.is_empty()
                && (0..parameter_count).any(|parameter| {
                    !slots.contains(&parameter)
                        && !sig.param_has_default(parameter)
                        && sig.vararg_index != Some(parameter)
                })
            {
                return None;
            }
            return Some(
                slots
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(argument, parameter)| (parameter, argument))
                    .collect(),
            );
        }

        let trailing_lambda = args
            .last()
            .is_some_and(|argument| matches!(argument, Ty::Fun(_)));
        if trailing_lambda && args.len() < parameter_count {
            let last_parameter = parameter_count.checked_sub(1)?;
            let prefix = args.len().checked_sub(1)?;
            if sig.has_known_required_param(prefix..last_parameter) {
                return None;
            }
            let mut mapping = (0..prefix).map(|index| (index, index)).collect::<Vec<_>>();
            mapping.push((last_parameter, args.len() - 1));
            return Some(mapping);
        }
        if sig.has_known_required_param(args.len()..parameter_count) {
            return None;
        }
        Some((0..args.len()).map(|index| (index, index)).collect())
    }

    fn select_top_level_default_callable(
        &self,
        name: &str,
        args: &[Ty],
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<LibraryCallable> {
        self.select_top_level_default_callable_with_slots(name, args, None, type_args, expected)
    }

    fn select_top_level_default_callable_with_slots(
        &self,
        name: &str,
        args: &[Ty],
        slots: Option<&[usize]>,
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<LibraryCallable> {
        // Direct scope resolution, not `resolve_symbol(TopLevel)`: runs inside `pick_top_level` (see
        // `select_top_level_default_callable`) — routing back through `resolve_symbol` would recurse.
        let try_default = |o: &FunctionInfo| -> Option<LibraryCallable> {
            let c = &o.callable;
            if !o.public() && !o.flags.inline.must_inline() {
                return None;
            }
            // A `$default` synthetic usually carries NO generic `Signature` (it isn't API), so binding the
            // return type parameter off it fails and the erased `Object` return leaks (`runBlocking { … }`
            // → `Any`, losing the block's result type). Fall back to the BASE function's gsig — its leading
            // real parameters (and their type-parameter positions) align with the `$default`'s, so unifying
            // the provided args against it recovers `T` (`runBlocking<T>(block: () -> T): T` → `T = Ch`).
            // Resolve the base ONCE for every metadata facet the synthetic may lack. Arity alone is
            // not identity: two overloads can have the same number of parameters but different JVM
            // spellings and unrelated type variables. Borrowing either one's generic signature can
            // bind a return or reified marker to the wrong formal while still producing valid bytecode.
            //
            // krusty models `$default` with only the real parameters, so exact JVM spelling plus the
            // logical parameter vector identifies its base without reconstructing a descriptor from
            // lossy `Ty` values (`Byte`/`Short` both appear as `Int`).
            let base_spelling = c.name.strip_suffix("$default");
            let base = base_spelling.and_then(|spelling| {
                function_set_from_symbols(self.symbols_in_scope(name))
                    .into_top_level()
                    .find(|candidate| {
                        candidate.callable.name == spelling
                            && candidate.callable.params.as_slice() == c.params.as_slice()
                    })
            })?;
            let semantic = base.semantic_signature();
            let mapping = self.default_argument_mapping(o, semantic.params.len(), args, slots)?;
            crate::trace_compiler!(
                "default_semantics",
                "name={name} bridge={} base={} type_args={type_args:?} bridge_generic={:?} base_generic={:?}",
                c.name,
                base.callable.name,
                o.generic_sig,
                base.generic_sig,
            );
            // The `$default` method is a physical bridge, not a source declaration. Its erased
            // descriptor/signature must never override the semantic base selected above.
            let mut bindings = seeded_gsig_binds(&semantic, type_args);
            for (parameter, argument) in &mapping {
                let shape = *semantic.params.get(*parameter)?;
                if type_args.is_empty() {
                    unify_inferred_ty(shape, args[*argument], &mut bindings);
                } else {
                    unify_ty(shape, args[*argument], &mut bindings);
                }
            }
            if let Some(expected) = expected {
                unify_ty(semantic.ret, expected, &mut bindings);
            }
            if !generic_bindings_satisfy_bounds(&semantic, &bindings, |actual, bound| {
                resolution_subtype(&self.src, actual, bound)
            }) {
                return None;
            }
            let semantic_params = semantic
                .params
                .iter()
                .map(|parameter| {
                    instantiate_slot(
                        &self.src,
                        Some(&semantic),
                        *parameter,
                        &bindings,
                        TypePosition::In,
                        UnboundSpecialization::UseUpperBound,
                    )
                })
                .collect::<Vec<_>>();
            if mapping.iter().any(|(parameter, argument)| {
                !self.arg_fits_or_subtype(&semantic_params[*parameter], &args[*argument])
            }) {
                return None;
            }
            let ret_ty = instantiate_slot(
                &self.src,
                Some(&semantic),
                semantic.ret,
                &bindings,
                TypePosition::Out,
                UnboundSpecialization::UseUpperBound,
            );
            crate::trace_compiler!(
                "resolve",
                "top_level_default {name} base_gsig={} mapping={mapping:?} -> ret={ret_ty:?}",
                base.generic_sig.is_some()
            );
            let ret_ty = base.ret.apply(ret_ty);
            let mut callable =
                callable_with_return(&selected_default_callable(&base)?, ret_ty, true);
            // The bridge's descriptor exposes erased parameters (`Any`, `Any`, …), but argument
            // checking belongs to the selected SOURCE declaration. Retain the base generic shapes
            // specialized by this call's constraints; otherwise an omitted-default call such as
            // `assertEquals(String?, String)` selects correctly and is then spuriously rechecked as
            // `Any <- String?`.
            callable.params = apply_platform_call_parameter_nullability(
                semantic_params,
                &base.call_sig.platform_nullable_params,
                args,
                base.call_sig.vararg,
            );
            // Same reasoning as `base_gsig`, for the JVM `Signature`: the `$default` synthetic carries
            // none, and a `<reified T>` splice reads its formal type-parameter NAMES from there to bind
            // the call's type arguments. The base's formals ARE the synthetic's — the mask/marker
            // parameters the synthetic appends introduce no type variables — so an omitted-default call
            // to a reified inline can specialize its body instead of falling back to a direct
            // (throwing) invoke.
            //
            // The base is identified by SPELLING, not by arity alone: the formal type-parameter names
            // are what the splice substitutes by, so borrowing a same-arity SIBLING overload's
            // signature would specialize the body against the wrong names. `sourceName-<hash>$default`
            // belongs to `sourceName-<hash>`, and only that one.
            if callable.signature.is_none() {
                callable.signature = base.callable.signature;
            }
            record_default_vararg_slot(
                &mut callable,
                o.call_sig.vararg_index,
                &base.callable.params,
                args,
            );
            Some(callable)
        };
        let select_realized = |candidates: Vec<&FunctionInfo>| {
            let mut applicable = candidates
                .into_iter()
                .filter_map(|candidate| {
                    let callable = try_default(candidate)?;
                    let mapping = self.default_argument_mapping(
                        candidate,
                        callable.params.len(),
                        args,
                        slots,
                    )?;
                    let supplied = mapping
                        .iter()
                        .map(|(parameter, _)| callable.params[*parameter])
                        .collect();
                    Some((supplied, callable))
                })
                .collect::<Vec<_>>();
            // Equivalent supplied parameter shapes differ only in how many defaults the bridge must
            // fill. Kotlin prefers the declaration omitting fewer parameters; sorting makes that
            // declaration the equivalent candidate retained by the common specificity selector.
            applicable.sort_by_key(|(_, callable)| callable.params.len());
            unique_most_specific(applicable, |_, left, right| {
                resolution_subtype(&self.src, left, right)
            })
        };
        let fsd = function_set_from_symbols(self.symbols_in_scope(&format!("{name}$default")));
        crate::trace_compiler!(
            "default_semantics",
            "top-level default candidates name={name} args={args:?}: {:?}",
            fsd.top_level()
                .map(|candidate| (
                    candidate.callable.owner,
                    candidate.callable.name.as_str(),
                    candidate.callable.params.as_slice(),
                    candidate.call_sig.param_defaults.as_slice(),
                    candidate.call_sig.required,
                    candidate.call_sig.vararg_index,
                ))
                .collect::<Vec<_>>()
        );
        match select_realized(fsd.top_level().collect()) {
            CandidateSelection::Selected(callable) => return Some(callable),
            CandidateSelection::Ambiguous => return None,
            CandidateSelection::None => {}
        }
        // A `@JvmName`/value-class-mangled base (`sourceName` → `sourceName-<hash>`) mangles its
        // `$default` synthetic too. The import scope only knows the SOURCE spelling (`{name}$default` maps through
        // the explicit import), so resolve each mangled synthetic directly in its base candidate's
        // facade package. Probed LAST: an unmangled name never reaches this (the common case pays no
        // extra scope query).
        let mut seen_spellings = std::collections::HashSet::new();
        let mut mangled_defaults = Vec::new();
        for base in function_set_from_symbols(self.symbols_in_scope(name)).into_top_level() {
            let spelling = base.callable.name.clone();
            if spelling == name || !seen_spellings.insert(spelling.clone()) {
                continue;
            }
            let Some(pkg) = base.callable.owner.parent() else {
                continue;
            };
            let default_name = format!("{spelling}$default");
            let record = self
                .src
                .symbols(SymbolNamespace::Package(pkg), &default_name);
            let fs = function_set_from_symbols(std::iter::once(record));
            mangled_defaults.extend(fs.into_top_level());
        }
        match select_realized(mangled_defaults.iter().collect()) {
            CandidateSelection::Selected(callable) => Some(callable),
            CandidateSelection::None | CandidateSelection::Ambiguous => None,
        }
    }
}

// --- Navigation helpers (member/constructor resolution expressed purely against the trait) --------
// The inherited-member walk over a library type's hierarchy — arg-dependent binding, so it lives in
// this layer (not the oracle). `resolve` and `ir_lower` share one implementation, backend-agnostic.

pub(crate) fn apply_platform_call_parameter_nullability(
    mut params: Vec<Ty>,
    nullable: &[bool],
    args: &[Ty],
    vararg: bool,
) -> Vec<Ty> {
    if vararg {
        let Some((array, fixed)) = params.split_last() else {
            return params;
        };
        let array = *array;
        let fixed_len = fixed.len();
        for ((parameter, accepts_null), argument) in
            params[..fixed_len].iter_mut().zip(nullable).zip(args)
        {
            if *accepts_null
                && (argument.is_nullable() || *argument == Ty::Null)
                && parameter.is_reference()
            {
                *parameter = Ty::nullable(*parameter);
            }
        }
        if nullable.get(fixed_len).copied().unwrap_or(false) {
            if let Some(element) = array.array_read_elem() {
                if element.is_reference()
                    && args
                        .get(fixed_len..)
                        .unwrap_or_default()
                        .iter()
                        .any(|argument| argument.is_nullable() || *argument == Ty::Null)
                {
                    params[fixed_len] = Ty::array(Ty::nullable(element));
                }
            }
        }
        return params;
    }
    for ((parameter, accepts_null), argument) in params.iter_mut().zip(nullable).zip(args) {
        if *accepts_null
            && (argument.is_nullable() || *argument == Ty::Null)
            && parameter.is_reference()
        {
            *parameter = Ty::nullable(*parameter);
        }
    }
    params
}

/// A construction routed through the provider's opaque alternate realization. The semantic
/// declaration has already won overload selection; this record only carries the argument layout the
/// backend needs to invoke that declaration when a direct call is unavailable.
#[derive(Clone, Debug)]
pub struct SyntheticCtorCall {
    /// The selected Kotlin declaration. Its semantic parameters and call shape remain authoritative;
    /// the fields below describe only the provider's alternate physical realization.
    pub declaration: LibraryMember,
    /// The synthetic `<init>` descriptor to invoke.
    pub descriptor: String,
    /// The REAL (source) parameter types in descriptor form — a value-class param appears here as its
    /// erased underlying. Provided args coerce to the leading `provided` of these; the rest are omitted.
    pub real_params: Vec<Ty>,
    pub mask_count: usize,
}

/// One checker-ready constructor application. Overload selection always chooses the semantic
/// constructor declaration first; its provider-attached realization never participates in selection.
#[derive(Clone, Debug)]
pub enum SelectedConstructorCall {
    Direct(Box<LibraryMember>),
    Platform(Box<SyntheticCtorCall>),
}

fn select_constructor_call(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    internal: TypeName,
    args: &[CallArgKind],
) -> Option<SelectedConstructorCall> {
    let classifier = src.classifier(internal)?;
    select_constructor_call_from_type(lib, src, internal, &classifier, args)
}

/// Select one semantic constructor overload, then couple that declaration to its opaque platform
/// invocation. Overload selection is shared with ordinary members; marker/default discovery never
/// participates in applicability and therefore cannot act as a fallback resolver.
pub(crate) fn select_constructor_call_from_type(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    internal: TypeName,
    classifier: &crate::libraries::LibraryType,
    args: &[CallArgKind],
) -> Option<SelectedConstructorCall> {
    crate::trace_compiler!(
        "resolve",
        "select constructor {internal} declarations={:?} args={args:?}",
        classifier
            .constructors
            .iter()
            .map(|constructor| (
                &constructor.params,
                constructor.visibility,
                &constructor.descriptor
            ))
            .collect::<Vec<_>>()
    );
    let mut declaration = select_constructor_declaration_from_type(lib, src, classifier, args)?;
    // Constructors are ordinary selected callables. Normalize the classifier identity onto the
    // selected declaration even when a provider stores constructor records inside the owning
    // classifier and therefore omits the redundant owner field at rest.
    declaration.owner.get_or_insert(internal);
    let omitted = args.len() < declaration.params.len();
    if omitted || (declaration.descriptor.is_empty() && declaration.default_realization.is_some()) {
        let realization = declaration.default_realization.as_deref()?;
        let descriptor = realization.descriptor.clone();
        let real_params = realization.real_params.clone();
        let mask_count = realization.mask_count;
        return Some(SelectedConstructorCall::Platform(Box::new(
            SyntheticCtorCall {
                declaration,
                descriptor,
                real_params,
                mask_count,
            },
        )));
    }
    Some(SelectedConstructorCall::Direct(Box::new(declaration)))
}

/// Select the source constructor declaration. This operation knows nothing about marker constructors,
/// `constructor-impl`, or any other platform realization; it is the same overload-selection step used
/// before a selected declaration is coupled to an emit target.
pub(crate) fn select_constructor_declaration_from_type(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    classifier: &crate::libraries::LibraryType,
    args: &[CallArgKind],
) -> Option<LibraryMember> {
    select_constructor_declaration_from_type_with_type_arguments(lib, src, classifier, args, &[])
}

pub(crate) fn select_constructor_declaration_from_type_with_type_arguments(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    classifier: &crate::libraries::LibraryType,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Option<LibraryMember> {
    let classifier_bindings = classifier
        .type_parameters
        .type_params
        .iter()
        .cloned()
        .zip(type_args.iter().copied())
        .collect::<GSigBinds>();
    let candidates = classifier
        .constructors
        .iter()
        .map(|constructor| {
            let mut declaration = constructor.clone();
            declaration.params = specialized_constructor_params(src, constructor, args, type_args);
            if !classifier_bindings.is_empty() {
                declaration.params = declaration
                    .params
                    .iter()
                    .map(|parameter| ty_subst_keep_unbound(*parameter, &classifier_bindings))
                    .collect();
            }
            declaration
        })
        .collect::<Vec<_>>();
    let selected = best_callable_member_overload(lib, src, candidates.iter(), "<init>", args, &[])?;
    Some(selected.clone())
}

/// Resolve a companion member `Type.name(args)` (the receiver type must be public).
fn select_companion_member(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    internal: TypeName,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Option<LibraryMember> {
    let t = src.classifier(internal)?;
    let candidates = t.classifier_callables(internal);
    best_callable_member_overload(lib, src, candidates.iter(), name, args, type_args).map(
        |selected| {
            let params = specialized_member_params(selected, args, type_args);
            let mut member = selected.clone();
            member.params = params;
            // A generic STATIC's return erases in the descriptor (`<T> T read(Key<T>)` →
            // `Object`); bind it from the arguments exactly as instance members do, so
            // `Fields.read(Fields.PAYLOAD).message()` types as the field's argument.
            if let Some(gsig) = member.generic_sig.as_ref() {
                // Explicit call type arguments (`Maps.create<String, Int> { … }`) seed the
                // formals positionally; argument unification fills the rest.
                let mut binds = seeded_gsig_binds(gsig, type_args);
                for (&parameter, argument) in gsig.params.iter().zip(args) {
                    unify_ty(parameter, argument.ty(), &mut binds);
                }
                member.ret = merge_specialized_return(member.ret, ty_subst(gsig.ret, &binds));
                // Static and instance calls consume the same declaration-owned return policy. Applying it
                // here, after binding, preserves a flexible reference contract without a static/provider fork.
                member.ret = gsig.apply_return_policy(lib, member.ret);
            }
            member
        },
    )
}

/// Resolve an instance member `recv.name(args)` — the receiver's static type must be public, but the
/// member may be inherited from a (possibly non-public) supertype. Candidates come from the consolidated
/// `functions` query, whose Member overloads carry the breadth-first `receiver_rank`; the closest rung's
/// best overload wins (most-derived first), exactly the inherited-member walk this used to do by hand.
fn select_instance_member_name(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    internal: TypeName,
    name: &str,
    args: &[CallArgKind],
) -> Option<LibraryMember> {
    select_instance_member_ty(lib, src, Ty::obj_name(internal), name, args)
}

/// [`select_instance_member_name`] against an APPLIED receiver type — the form that keeps type arguments, so a
/// generic member's return is recovered from them rather than erased.
fn select_instance_member_ty(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
) -> Option<LibraryMember> {
    select_instance_info(lib, src, recv, name, args).map(|o| {
        let ret = o.ret.apply(o.callable.ret);
        o.member_with_return(ret)
    })
}

/// Resolve a library instance member for a BOUND callable reference (`"KOTLIN"::get`) — where there are
/// no call arguments to drive overload resolution. Returns the UNIQUE natural-shape overload of `name`
/// on `internal`, or `None` when the member is absent or ambiguous. Defaults and varargs do not alter
/// that natural shape: every declared parameter remains present and a vararg is its array parameter.
fn receiver_allows_member_dispatch(receiver: Ty) -> bool {
    match receiver {
        Ty::Nullable(_) | Ty::Null => false,
        Ty::TyParam(_, bound) => receiver_allows_member_dispatch(*bound),
        // Kotlin permits direct member calls on Java platform types despite uncertain nullability.
        Ty::PlatformNullable(_) => true,
        _ => true,
    }
}

fn select_instance_reference_from_functions(
    recv: Ty,
    functions: &[FunctionInfo],
) -> Option<LibraryMember> {
    let mut fixed = functions
        .iter()
        .filter(|o| o.kind == FnKind::Member)
        .collect::<Vec<_>>();
    let nearest = fixed
        .iter()
        .map(|candidate| candidate.receiver_rank)
        .min()?;
    fixed.retain(|candidate| candidate.receiver_rank == nearest);
    let mut fixed = fixed.into_iter();
    let o = fixed.next()?;
    // Duplicate facts for the same signature are not ambiguous; distinct signatures are.
    if fixed.any(|other| {
        other.semantic_params() != o.semantic_params() || other.callable.ret != o.callable.ret
    }) {
        return None;
    }
    // Object members are ordinary callable-reference candidates for a concrete non-null receiver
    // (`A::equals`, `Int::toString`). Nullable and type-parameter receivers need kotlinc's null-guarded
    // realization (`null::toString`); decline only those semantic receiver shapes instead of deleting
    // the inherited declaration for every class and synthesizing its names in the checker.
    if !receiver_allows_member_dispatch(recv) {
        return None;
    }
    let ret = o.ret.apply(o.callable.ret);
    Some(o.member_with_return(ret))
}

#[derive(Clone, Debug)]
pub struct ResolvedPropertyRef {
    pub name: String,
    pub getter: LibraryCallable,
    pub setter: Option<LibraryCallable>,
    pub getter_visibility: crate::types::Visibility,
    pub setter_visibility: crate::types::Visibility,
    /// Semantic receiver written at the reference site. The physical accessor may be inherited from
    /// a different `getter.owner`; any platform reflection carrier is an emission concern.
    pub reflection_owner: Ty,
    pub prop_ty: Ty,
    pub source_key: Option<(u32, u32)>,
    /// Stable declaration identity for a property from the current compilation. Parser-arena
    /// source keys remain a transient resolver detail and must not cross into checked FIR.
    pub stable_declaration: Option<crate::fir::DeclarationId>,
    /// An associated `companion val/var C.name`. Its classifier receiver participates in lookup and
    /// reflection ownership, but is not a callable-reference parameter or runtime accessor receiver.
    pub companion_extension: bool,
    /// `None` for an instance property; `Some(None)` for a same-file extension property;
    /// `Some(Some(owner))` for an extension property emitted on another facade.
    pub extension_facade: Option<Option<TypeName>>,
}

fn select_extension_property_ref(property: PropertyInfo) -> Option<ResolvedPropertyRef> {
    let companion_extension = property.is_companion_extension();
    let name = property.name;
    let source_key = property.source_key;
    let getter_visibility = property.visibility;
    let setter_visibility = property.setter_visibility;
    let getter = property.getter;
    crate::trace_compiler!(
        "callable_ref",
        "extension property target getter={}.{}{} params={:?} suspend={} default_call={} source={source_key:?}",
        getter.owner,
        getter.name,
        getter.descriptor,
        getter.params,
        getter.suspend,
        getter.default_call,
    );
    if getter.suspend || getter.default_call || getter.params.len() != 1 {
        return None;
    }
    let setter = property
        .setter
        .filter(|setter| setter.params.len() == 2 && setter.physical_ret == Ty::Unit);
    let prop_ty = getter.ret;
    let reflection_owner = *getter.params.first()?;
    let facade = (!getter.owner.matches("")).then_some(getter.owner);
    Some(ResolvedPropertyRef {
        name,
        extension_facade: Some(facade),
        getter,
        setter,
        getter_visibility,
        setter_visibility,
        reflection_owner,
        prop_ty,
        source_key,
        stable_declaration: property.stable_declaration,
        companion_extension,
    })
}

fn build_property_reference_from_declaration(
    recv: Ty,
    name: &str,
    property: &PropertyInfo,
) -> Option<ResolvedPropertyRef> {
    if property.getter.suspend {
        crate::trace_compiler!(
            "resolve",
            "property ref rejected {recv:?}.{name}: suspend getter"
        );
        return None;
    }
    let prop_ty = property.ty;
    let getter_visibility = property.visibility;
    let setter_visibility = property.setter_visibility;
    let getter = property.getter.clone();
    let setter = property
        .setter
        .clone()
        .filter(|setter| setter.params.len() == 1 && setter.physical_ret == Ty::Unit);
    if !getter.params.is_empty() {
        crate::trace_compiler!(
            "resolve",
            "property ref rejected {recv:?}.{name}: getter {}.{}{} is not a plain zero-argument accessor",
            getter.owner.render(),
            getter.name,
            getter.descriptor,
        );
        return None;
    }
    Some(ResolvedPropertyRef {
        name: property.name.clone(),
        getter,
        setter,
        getter_visibility,
        setter_visibility,
        reflection_owner: recv,
        prop_ty,
        source_key: property.source_key,
        stable_declaration: property.stable_declaration,
        companion_extension: false,
        extension_facade: None,
    })
}

#[derive(Clone, Debug)]
pub struct ResolvedMember {
    /// Semantic dispatch receiver selected with this overload. It is part of the call target rather
    /// than a lowering-side reconstruction, and is identical for module and dependency providers.
    pub receiver: Ty,
    pub member: LibraryMember,
    /// Declaration/ABI parameter types before call-site generic substitution. Checking uses
    /// `member.params`; module linking and argument realization use this parallel physical shape.
    pub physical_params: Vec<Ty>,
    /// One entry per leading context parameter. `Some` is supplied from scope; `None` is an
    /// explicitly named source argument. Ordinary members have an empty vector.
    pub context_args: Vec<Option<crate::resolve::ResolvedContextArgument>>,
    pub ret: Ty,
    pub projected_return_hazard: bool,
    /// The resolved member is a `suspend fun` — the caller (a suspend body) must thread a
    /// `Continuation` into the emitted call and treat the (Object-erased) result as `ret`.
    pub suspend: bool,
    /// Declaration origin affects linkage only. Keeping it on the selected target removes the old
    /// module/dependency variant split without conflating local lifted functions with members.
    pub origin: Origin,
}

impl ResolvedMember {
    pub(crate) fn from_member(
        receiver: Ty,
        member: LibraryMember,
        ret: Ty,
        projected_return_hazard: bool,
        origin: Origin,
    ) -> Self {
        let physical_params = member.physical_params.clone();
        let suspend = member.suspend();
        Self {
            receiver,
            member,
            physical_params,
            context_args: Vec::new(),
            ret,
            projected_return_hazard,
            suspend,
            origin,
        }
    }

    pub(crate) fn from_callable(
        receiver: Ty,
        callable: LibraryCallable,
        projected_return_hazard: bool,
    ) -> Self {
        let origin = callable.origin.clone();
        let ret = callable.ret;
        let suspend = callable.suspend;
        let physical_params = callable.physical_params.clone();
        let mut member = LibraryMember::new(
            callable.name,
            callable.params,
            callable.ret,
            callable.descriptor,
        );
        member.external_identity = callable.external_identity;
        member.external_property_identity = callable.external_property_identity;
        member.physical_params = callable.physical_params.clone();
        member.owner = Some(callable.owner);
        member.physical_ret = callable.physical_ret;
        member.signature = callable.signature;
        member.generic_sig = callable.generic_sig.map(|signature| *signature);
        member.set_is_interface(callable.owner_is_interface);
        member.set_is_abstract(callable.is_abstract);
        member.set_suspend(callable.suspend);
        member.realization = callable.member_realization;
        member.context_count = callable.context_count;
        member.inline = callable.inline;
        member.inline_body_plan = callable.inline_body_plan;
        member.declared_ret = callable.declared_ret;
        member.contract = callable.contract;
        member.default_realization = callable.default_realization;
        member.plugin_expression = callable.plugin_expression;
        Self {
            receiver,
            member,
            physical_params,
            context_args: Vec::new(),
            ret,
            projected_return_hazard,
            suspend,
            origin,
        }
    }
}

pub struct SelectedMemberProperty {
    pub owner: TypeName,
    pub ty: Ty,
    pub interface: bool,
    pub visibility: crate::types::Visibility,
    /// The selected Kotlin declaration. Target storage realization remains opaque on its accessors.
    pub property: Option<PropertyInfo>,
}

/// Resolve an instance member and carry the logical return selected for this call. Generic member
/// returns may bind from the receiver (`List<Int>.get(Int): Int`) or, for erased-`Any` returns, from
/// the call arguments (`decodeFromString(serializer, text): T`).
fn resolved_member_from_info(
    lib: &dyn SemanticPlatform,
    source: &dyn SymbolSource,
    recv: Ty,
    args: &[CallArgKind],
    type_args: &[Ty],
    o: FunctionInfo,
) -> ResolvedMember {
    let ret = o
        .generic_sig
        .as_ref()
        .map(|gsig| {
            bind_member_return_from_call_args(
                source,
                gsig,
                recv,
                args,
                type_args,
                o.call_sig.vararg_index,
                &o.call_sig.no_infer_params,
                o.callable.ret,
            )
        })
        .unwrap_or(o.callable.ret);
    let ret = o.ret.apply(
        o.generic_sig
            .as_ref()
            .map_or(ret, |sig| sig.apply_return_policy(lib, ret)),
    );
    let member = o.member_with_return(o.callable.ret);
    ResolvedMember {
        receiver: recv,
        ret,
        member,
        physical_params: o.callable.physical_params.clone(),
        context_args: Vec::new(),
        projected_return_hazard: o.projected_return_hazard,
        suspend: o.flags.suspend,
        origin: o.callable.origin.clone(),
    }
}

fn member_property_from_callables(callables: &Callables) -> Option<PropertyInfo> {
    callables
        .properties()
        .iter()
        .filter(|property| property.kind == PropKind::Member && property.context_count == 0)
        .min_by_key(|property| property.receiver_rank)
        .cloned()
}

fn member_property_read_from_declaration(
    recv: Ty,
    declaration: &PropertyInfo,
) -> Option<ResolvedMember> {
    let callable = declaration.getter.clone();
    let mut selected = ResolvedMember::from_callable(recv, callable, false);
    selected.ret = declaration.ty;
    selected.member.visibility = declaration.visibility;
    selected.member.source_member = declaration.source_member;
    selected.member.stable_declaration = declaration
        .getter_declaration
        .or(declaration.stable_declaration);
    Some(selected)
}

fn member_property_write_from_declaration(
    declaration: &PropertyInfo,
) -> Option<ResolvedPropertySetter> {
    let setter = declaration.setter.clone()?;
    (setter.params.len() == 1 && setter.ret == Ty::Unit).then_some(ResolvedPropertySetter {
        callable: setter,
        visibility: declaration.setter_visibility,
        source_member: declaration.source_member,
        stable_declaration: declaration
            .setter_declaration
            .or(declaration.stable_declaration),
    })
}

fn member_property_write_from_callables(callables: &Callables) -> Option<ResolvedPropertySetter> {
    let rank = callables
        .properties()
        .iter()
        .filter(|property| property.kind == PropKind::Member && property.context_count == 0)
        .map(|property| property.receiver_rank)
        .min()?;
    callables
        .properties()
        .iter()
        .filter(|property| {
            property.kind == PropKind::Member
                && property.context_count == 0
                && property.receiver_rank == rank
        })
        .find_map(member_property_write_from_declaration)
}

fn select_instance_info(
    lib: &dyn SemanticPlatform,
    source: &dyn SymbolSource,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
) -> Option<FunctionInfo> {
    select_overload(
        lib,
        recv,
        name,
        args,
        &[],
        FnKind::Member,
        ExtCtx {
            fn_scope: None,
            source,
        },
    )
}

/// The shared unqualified-name resolution LOOP (spec § Resolution): form a candidate FQN `pkg/name` for
/// each in-scope `packages` entry and query [`crate::symbol_source::SymbolSource::resolve_symbols`] once
/// per candidate, returning each `(fqn, record)` whose namespace record is non-empty. The helper does
/// ONLY the loop — it does not decide anything. Because the record keeps the two namespaces SEPARATE
/// (`classifier` vs `callables`), each caller applies its own selection rules organically: a type
/// position reads `classifier` under level-precedence + within-level ambiguity; a call position flattens
/// `callables` and runs overload resolution. The `fqn` is returned so a classifier caller can name the
/// resolved internal (a non-alias classifier's internal name IS its fqn).
/// The rung of `decl_recv` in `recv`'s SOURCE-type supertype closure (0 = same class), or `None` if the
/// extension's declared receiver is neither `recv` nor a supertype of it. Uses `erased_recv` Kotlin-level
/// keys + `resolve_type` supertypes — NO JVM descriptors — so `kotlin/UInt` ≠ `kotlin/Int` ≠ `kotlin/Result`
/// are distinct by their class, a generic value-class receiver (`Result<T>`) binds a concrete one
/// (`Result<String>` — `erased_recv` drops type arguments), and `UInt` never binds an `Int` extension.
/// Replaces the descriptor-based `extension_receiver_rank`, whose value-class special-case existed only
/// because the erased `I`/`Object` descriptors tied distinct value classes together.
/// Whether the declared receiver's type arguments are consistent with the actual receiver's, position by
/// position, under Kotlin's COVARIANT reading of a receiver position: each actual argument must be
/// assignable to the declared one (`ReceiverMro::rank` reaching from actual to declared). A declared
/// argument that is a type variable or `Any`/`Object` is a wildcard (an `Iterable<T>` / erased
/// `Iterable<Any>` extension binds any element). This rejects the `@JvmName` reduction variant whose
/// element does not match (`Iterable<Byte>.averageOfByte` against a `List<Double>` — `Double` is not
/// assignable to `Byte`) while accepting a nested-generic supertype (`Iterable<Iterable<T>>.flatten`
/// against `List<List<Int>>` — `List<Int>` IS assignable to `Iterable<Any>`). The erased supertype walk
/// in `ReceiverMro` alone keys on the outer class only, so it would tie the reduction variants.
fn receiver_type_args_match(src: &dyn SymbolSource, decl_recv: Ty, recv: Ty) -> bool {
    // Each actual argument must be assignable to the declared one under Kotlin's covariant receiver
    // reading. A declared argument that is a type variable or erased `Any` is a WILDCARD — the metadata
    // decode drops the nullability flag, so a `T?` receiver element reads as bare `Any`, and a nullable
    // actual (`Int?`) must still match it (`is_assignable(Int?, Any)` is correctly `false` under strict
    // Kotlin, but here `Any` stands for the erased variable, not the type `Any`).
    let cx = crate::assignable::TyCtx::new();
    let oracle = SourceOracle(src);
    if decl_recv.mentions_ty_param() {
        let mut bindings = GSigBinds::new();
        unify_ty_from_symbols(src, decl_recv, recv, &mut bindings);
        let specialized = ty_subst_keep_unbound(decl_recv, &bindings);
        // A callee-owned receiver variable may bind to a still-symbolic variable owned by the
        // caller: `Flow<Flow<T@flattenMerge>>` against `Flow<Flow<R@flatMapMerge>>`. The
        // specialized shapes are then exactly equal even though the caller's `R` quite correctly
        // remains a type parameter. Do not mistake that retained caller identity for an unbound
        // callee formal.
        if specialized == recv {
            return true;
        }
        if !specialized.mentions_ty_param() {
            return crate::assignable::is_assignable(&cx, &oracle, recv, specialized);
        }
    }
    let wildcard = |t: Ty| {
        let t = t.projection_inner().unwrap_or(t);
        t.is_ty_param()
            || matches!(t.non_null(), Ty::Obj(n, _)
                if crate::types::same(n, crate::types::wk::any())
                    || crate::types::same(n, crate::types::wk::java_object()))
    };
    decl_recv
        .type_args()
        .iter()
        .zip(recv.type_args().iter())
        .all(|(&d, &r)| {
            if wildcard(d) || wildcard(r) {
                return true;
            }
            match d {
                Ty::InProjection(expected) => {
                    crate::assignable::is_assignable(&cx, &oracle, *expected, r)
                }
                Ty::OutProjection(expected) => {
                    crate::assignable::is_assignable(&cx, &oracle, r, *expected)
                }
                _ => crate::assignable::is_assignable(&cx, &oracle, r, d),
            }
        })
}

/// The receiver's erased supertype closure with its BFS rungs, computed ONCE per receiver and probed
/// per candidate. Every rank query used to run a fresh supertype BFS (hash-set churn included) per
/// candidate even though the receiver is FIXED across a call site's whole candidate set. The closure
/// is small (a handful of supertypes), so a `Vec` probe beats hashing.
pub(crate) struct ReceiverMro {
    recv: Ty,
    /// `(applied supertype, BFS rung)` in first-seen order.
    /// Empty for a receiver with no class-name key (an array): such a receiver ranks only by exact
    /// `Ty` equality or the universal `Any` fallback, exactly as the per-candidate BFS did.
    ranks: Vec<(Ty, u32)>,
}

/// Whether two semantic function shapes form an applicable extension-receiver match. Declared
/// type parameters are bound from the actual callable shape; erased tops remain wildcards. Kotlin
/// receiver-function notation shares the same value representation and parameter list, so the
/// `has_receiver` marker does not participate here.
pub(crate) fn function_shape_matches(src: &dyn SymbolSource, actual: Ty, declared: Ty) -> bool {
    let (Ty::Fun(actual), Ty::Fun(declared)) = (actual.non_null(), declared.non_null()) else {
        return false;
    };
    let mut bindings = GSigBinds::new();
    let component_matches = |declared: Ty, actual: Ty, bindings: &mut GSigBinds| {
        if declared.is_erased_top() {
            return true;
        }
        unify_ty_from_symbols(src, declared, actual, bindings);
        ty_subst_keep_unbound(declared, bindings) == actual
    };
    actual.params.len() == declared.params.len()
        && actual.suspend == declared.suspend
        && declared
            .params
            .iter()
            .zip(&actual.params)
            .all(|(&declared, &actual)| component_matches(declared, actual, &mut bindings))
        && component_matches(declared.ret, actual.ret, &mut bindings)
}

impl ReceiverMro {
    pub(crate) fn new(src: &dyn SymbolSource, recv: Ty) -> ReceiverMro {
        let mut ranks = Vec::new();
        if let Some(internal) = recv.erased_recv().kotlin_class_internal() {
            let root = if recv.non_null().obj_internal().is_some() {
                recv.non_null()
            } else {
                Ty::obj_name(internal)
            };
            let mut frontier = vec![root];
            let mut seen = std::collections::HashSet::new();
            let mut rung = 0u32;
            while !frontier.is_empty() {
                let mut next = Vec::new();
                for ty in frontier {
                    let Some(internal) = ty.kotlin_class_internal() else {
                        continue;
                    };
                    if !seen.insert(internal) {
                        continue;
                    }
                    ranks.push((ty, rung));
                    next.extend(direct_supertypes(src, ty));
                }
                frontier = next;
                rung += 1;
            }
        }
        ReceiverMro { recv, ranks }
    }

    /// Use the applied supertype unless its classpath signature erased every argument.
    fn binding_receiver_for(&self, applied: Ty) -> Ty {
        let applied_args = applied.type_args();
        let recv_args = self.recv.type_args();
        if !recv_args.is_empty()
            && (applied_args.is_empty()
                || (recv_args.len() == applied_args.len()
                    && applied_args
                        .iter()
                        .all(|arg| arg.is_erased_top() || arg.is_ty_param())))
        {
            self.recv
        } else {
            applied
        }
    }

    fn match_receiver(&self, src: &dyn SymbolSource, decl_recv: Ty) -> Option<(u32, Ty)> {
        // A generic receiver may bind its parameter to a nullable type even though a bare T is not
        // itself a nullable value occurrence. An explicit non-null upper bound closes that route.
        let accepts_nullable = decl_recv.admits_null()
            || matches!(decl_recv, Ty::TyParam(_, bound) if bound.upper_bound_admits_null());
        // The null literal has no classifier hierarchy of its own, but it is a valid receiver for
        // every nullable extension receiver. Candidate specificity is decided after this applicability
        // rung; inventing a class key for `Null` would incorrectly make it a member of `Any`'s MRO.
        if self.recv == Ty::Null || (self.recv.is_nullable() && self.recv.non_null() == Ty::Nothing)
        {
            return accepts_nullable.then_some((0, self.recv));
        }
        if self.recv.is_nullable() && !accepts_nullable {
            return None;
        }
        // Function types have no classifier hierarchy to walk. A generic extension receiver such as
        // `suspend () -> T` must nevertheless admit `suspend () -> Unit`; the later generic-binding
        // pass binds `T`. Compare the semantic function shape here and treat only declared type
        // parameters/erased tops as wildcards—no classifier-name or arity reconstruction is involved.
        // Receiver-function notation is not a distinct function class: `A.() -> R` and `(A) -> R`
        // have the same parameter list and values freely cross that notation boundary. Keep the flag
        // for lambda binding, but do not make it part of extension-receiver applicability.
        if function_shape_matches(src, self.recv, decl_recv) {
            return Some((0, self.recv));
        }
        if declared_function_type(src, decl_recv)
            .is_some_and(|declared| function_shape_matches(src, self.recv, declared))
        {
            return Some((0, self.recv));
        }
        // A nominal classifier may implement a function type directly or through an interface.
        // Its member scope remains nominal, but extension applicability uses the exact callable
        // supertype shape published by the provider. Bind generic receiver slots from that shape;
        // returning the nominal receiver here loses `T` in `suspend () -> T` before selection.
        if matches!(decl_recv.non_null(), Ty::Fun(_)) {
            let callable = crate::symbol_resolver::classifier_callable_signature(src, self.recv)?;
            if function_shape_matches(src, callable, decl_recv) {
                let callable_rung = self
                    .ranks
                    .iter()
                    .find_map(|(applied, rung)| {
                        let internal = applied.kotlin_class_internal()?;
                        src.classifier(internal)?
                            .callable_signature
                            .is_some()
                            .then_some(rung.saturating_add(1))
                    })
                    .unwrap_or(1);
                return Some((callable_rung, callable));
            }
        }
        // Same source type — rung 0. Plain `Ty` equality (interned, NO erasure): the exact receiver an
        // extension is declared on. This is the ONLY rank an ARRAY receiver (`IntArray.sum()`) can carry
        // besides the universal `Any` — an array has no class-name key in the closure, and its
        // element type must be matched exactly (an `IntArray` extension must not bind an `Array<String>`).
        if self.recv.non_null() == decl_recv.non_null() {
            return Some((0, self.recv));
        }
        let want = decl_recv.erased_recv().kotlin_class_internal();
        if let Some(want) = want {
            if let Some(&(applied, rung)) = self.ranks.iter().find(|(applied, _)| {
                if applied.kotlin_class_internal() != Some(want) {
                    return false;
                }
                let binding_receiver = self.binding_receiver_for(*applied);
                receiver_type_args_match(src, decl_recv, binding_receiver)
            }) {
                let binding_receiver = if decl_recv.is_ty_param() || decl_recv.is_erased_top() {
                    self.recv
                } else {
                    self.binding_receiver_for(applied)
                };
                return Some((rung, binding_receiver));
            }
        }
        // A universal `Any`-receiver extension (`<T> T.let`) applies to every receiver — arrays included
        // — at lowest precedence.
        want.is_some_and(|n| n.matches("kotlin/Any"))
            .then_some((u32::MAX - 1, self.recv))
    }

    pub(crate) fn rank(&self, src: &dyn SymbolSource, decl_recv: Ty) -> Option<u32> {
        self.match_receiver(src, decl_recv).map(|(rank, _)| rank)
    }

    fn binding_receiver(&self, src: &dyn SymbolSource, decl_recv: Ty) -> Option<Ty> {
        self.match_receiver(src, decl_recv)
            .map(|(_, applied)| applied)
    }
}

pub(crate) fn symbols_at_scope_level(
    src: &dyn SymbolSource,
    name: &str,
    packages: &[TypeName],
) -> Vec<std::rc::Rc<crate::libraries::ResolvedSymbols>> {
    let lib = src;
    packages
        .iter()
        .filter_map(|pkg| {
            let r = lib.symbols(SymbolNamespace::Package(*pkg), name);
            (!r.is_empty()).then_some(r)
        })
        .collect()
}

fn has_callables(record: &crate::libraries::ResolvedSymbols) -> bool {
    !matches!(record.callables, crate::libraries::Callables::None)
}

fn symbols_in_function_scope(
    src: &dyn SymbolSource,
    name: &str,
    scope: FunctionScopeRef<'_>,
) -> Vec<std::rc::Rc<crate::libraries::ResolvedSymbols>> {
    symbol_levels_in_function_scope(src, name, scope)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn symbol_levels_in_function_scope(
    src: &dyn SymbolSource,
    name: &str,
    scope: FunctionScopeRef<'_>,
) -> Vec<Vec<std::rc::Rc<crate::libraries::ResolvedSymbols>>> {
    tagged_symbol_levels_in_function_scope(src, name, scope)
        .into_iter()
        .map(|level| level.symbols)
        .collect()
}

#[derive(Clone, Copy)]
enum FunctionScopeLevelKind {
    Flat,
    Explicit,
    CurrentPackage,
    Import,
}

impl FunctionScopeLevelKind {
    fn ambiguity_checks(self) -> bool {
        matches!(self, Self::Import)
    }
}

struct FunctionScopeLevel {
    kind: FunctionScopeLevelKind,
    symbols: Vec<std::rc::Rc<crate::libraries::ResolvedSymbols>>,
}

fn tagged_symbol_levels_in_function_scope(
    src: &dyn SymbolSource,
    name: &str,
    scope: FunctionScopeRef<'_>,
) -> Vec<FunctionScopeLevel> {
    match scope {
        FunctionScopeRef::Flat(packages) => {
            let records = symbols_at_scope_level(src, name, packages);
            (!records.is_empty())
                .then_some(FunctionScopeLevel {
                    kind: FunctionScopeLevelKind::Flat,
                    symbols: records,
                })
                .into_iter()
                .collect()
        }
        FunctionScopeRef::Imports(imports) => {
            let mut result = Vec::new();
            if let Some((owner, declared_name)) = imports.explicit_target(name) {
                let record = match owner {
                    SymbolNamespace::Classifier(classifier) => {
                        imported_object_member_symbols(src, classifier, &declared_name)
                            .unwrap_or_else(|| src.symbols(owner, &declared_name))
                    }
                    SymbolNamespace::Package(_) => src.symbols(owner, &declared_name),
                };
                let records = (!record.is_empty())
                    .then_some(record)
                    .into_iter()
                    .collect::<Vec<_>>();
                crate::trace_compiler!(
                    "resolve",
                    "import scope {name}: explicit target={:?}.{} records={}",
                    owner,
                    declared_name,
                    records.len()
                );
                if !records.is_empty() {
                    result.push(FunctionScopeLevel {
                        kind: FunctionScopeLevelKind::Explicit,
                        symbols: records,
                    });
                }
            }
            for (index, level) in imports.levels().iter().enumerate() {
                let records = symbols_at_scope_level(src, name, level)
                    .into_iter()
                    .filter(|record| has_callables(record))
                    .collect::<Vec<_>>();
                if !records.is_empty() {
                    crate::trace_compiler!(
                        "resolve",
                        "import scope {name}: level packages={} records={}",
                        level.len(),
                        records.len()
                    );
                    result.push(FunctionScopeLevel {
                        kind: if index == 0 {
                            FunctionScopeLevelKind::CurrentPackage
                        } else {
                            FunctionScopeLevelKind::Import
                        },
                        symbols: records,
                    });
                }
            }
            result
        }
    }
}

fn function_set_from_symbols(
    symbols: impl IntoIterator<Item = std::rc::Rc<crate::libraries::ResolvedSymbols>>,
) -> FunctionSet {
    FunctionSet {
        overloads: symbols
            .into_iter()
            .flat_map(|r| match &r.callables {
                crate::libraries::Callables::Functions(f) => f.overloads.clone(),
                crate::libraries::Callables::Both { functions, .. } => functions.overloads.clone(),
                _ => Vec::new(),
            })
            .collect(),
    }
}

fn callables_from_symbols(symbols: &[std::rc::Rc<crate::libraries::ResolvedSymbols>]) -> Callables {
    let mut functions = FunctionSet::default();
    let mut properties = PropertySet::default();
    for record in symbols {
        functions
            .overloads
            .extend(record.callables.functions().iter().cloned());
        properties
            .overloads
            .extend(record.callables.properties().iter().cloned());
    }
    Callables::from_parts(functions, properties)
}

/// Whether callable overload `o` is visible for an UNQUALIFIED (top-level or extension) call given the
/// in-scope packages `fn_scope`. A same-module callable ([`Origin::Module`]) is always visible — module
/// visibility is resolved separately, and its facade owner may be package-less. Only a CLASSPATH
/// ([`Origin::Library`]) callable must have its facade's package imported (same-package / star / explicit
/// / default), matching kotlinc. `None` scope keeps everything (a context with no import scope).
fn fn_in_scope(o: &FunctionInfo, fn_scope: Option<FunctionScopeRef<'_>>) -> bool {
    if !matches!(o.callable.origin, Origin::Library) {
        return true;
    }
    match fn_scope {
        None => true,
        Some(FunctionScopeRef::Flat(scope)) => scope
            .iter()
            .any(|&p| o.callable.owner_package_matches_name(p)),
        Some(FunctionScopeRef::Imports(_)) => true,
    }
}

/// Extension-selection context for [`select_overload`]: whether non-public `@InlineOnly` candidates are
/// admitted (the bytecode inliner), and the packages in scope for an extension (`None` = unscoped). Both
/// only affect EXTENSION selection — a member is always visible on its type.
#[derive(Clone, Copy)]
struct ExtCtx<'a> {
    fn_scope: Option<FunctionScopeRef<'a>>,
    source: &'a dyn SymbolSource,
}

/// The single call-overload selector for a receiver call `recv.name(args)`. It is parameterized by
/// [`FnKind`] — MEMBER and EXTENSION resolution differ only in the *calling convention* the backend emits
/// (invokevirtual with `this` vs invokestatic with the receiver as the leading arg), NOT in how the best
/// overload is chosen. The receiver is always an ATTRIBUTE, never `params[0]`: candidates are matched
/// against their LOGICAL value parameters (a member's `callable.params` are value-only; an extension's
/// prepend the receiver in the JVM emit shape, so [`logical_value_params`] strips it). Overloads are tried
/// closest-receiver-rank first, and within a rank by the ordered applicability passes below.
fn select_overload(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    kind: FnKind,
    ext: ExtCtx<'_>,
) -> Option<FunctionInfo> {
    let mut ambiguous = false;
    select_overload_tracking(lib, recv, name, args, type_args, kind, ext, &mut ambiguous)
}

#[derive(Clone, Copy, Debug)]
enum SelectionMode {
    Kind(FnKind),
    Receiver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexedConvention {
    Ordinary,
    Get,
    Set,
}

/// Project a fixed-arity indexed convention onto the operands written by indexed syntax. For
/// `set(i1, i2 = default, value)`, `receiver[i1] = value` maps to parameters `[0, 2]`; ordinary
/// positional mapping cannot express the required value after an omitted defaulted index.
fn indexed_fixed_parameter_projection(
    candidate: &FunctionInfo,
    params: &[Ty],
    argument_count: usize,
    convention: IndexedConvention,
) -> Option<Vec<Ty>> {
    let set = convention == IndexedConvention::Set;
    let parameter_indices = if set {
        let index_count = argument_count.checked_sub(1)?;
        if index_count >= params.len() {
            return None;
        }
        (0..index_count)
            .chain(std::iter::once(params.len() - 1))
            .collect::<Vec<_>>()
    } else {
        if argument_count > params.len() {
            return None;
        }
        (0..argument_count).collect::<Vec<_>>()
    };
    let defaults = candidate
        .call_sig
        .param_defaults
        .get(candidate.context_count..)
        .unwrap_or_default();
    if (0..params.len()).any(|parameter| {
        !parameter_indices.contains(&parameter)
            && !defaults.get(parameter).copied().unwrap_or(false)
    }) {
        return None;
    }
    Some(
        parameter_indices
            .into_iter()
            .map(|parameter| params[parameter])
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
fn select_receiver_overload_from_functions_tracking(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    ext: ExtCtx<'_>,
    functions: &[FunctionInfo],
    indexed: IndexedConvention,
) -> CandidateSelection<FunctionInfo> {
    let mut ambiguous = false;
    let selected = select_overload_tracking_with_functions(
        lib,
        recv,
        name,
        args,
        type_args,
        SelectionMode::Receiver,
        ext,
        Some(functions),
        &mut ambiguous,
        indexed,
    );
    if ambiguous {
        CandidateSelection::Ambiguous
    } else if let Some(selected) = selected {
        CandidateSelection::Selected(selected)
    } else {
        CandidateSelection::None
    }
}

fn select_overload_tracking(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    kind: FnKind,
    ext: ExtCtx<'_>,
    ambiguous: &mut bool,
) -> Option<FunctionInfo> {
    select_overload_tracking_with_functions(
        lib,
        recv,
        name,
        args,
        type_args,
        SelectionMode::Kind(kind),
        ext,
        None,
        ambiguous,
        IndexedConvention::Ordinary,
    )
}

#[allow(clippy::too_many_arguments)]
fn select_overload_tracking_with_functions(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    mode: SelectionMode,
    ext: ExtCtx<'_>,
    provided_functions: Option<&[FunctionInfo]>,
    ambiguous: &mut bool,
    indexed: IndexedConvention,
) -> Option<FunctionInfo> {
    let src = ext.source;
    // Argument assignability and candidate enumeration consume the same federated source. Access
    // control is checked after selection from the declaration metadata carried by the winner.
    let assign_src = src;
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    // EXTENSION candidates come from the ONE query — union `resolve_symbols`' function callables over the
    // in-scope packages (scope-pruned, tree-driven), so an unqualified extension binds only when its
    // facade's package is imported. No import scope → the whole-classpath `functions()` fallback
    // (removed once every consumer is scoped — task A). MEMBERS are always visible on their type.
    // A MEMBER's return can be RECEIVER-COUPLED (`Repo<Cfg>.byId(): Cfg`, a suspend `Continuation<T>`
    // bound from the receiver's type argument) — recovery the receiver-agnostic `resolve_type` cannot
    // do — so member candidates come from the platform's receiver-aware member query. EXTENSIONS come
    // from the scope-pruned `resolve_symbols` seam (empty when there is no import scope). Extension
    // candidates are BORROWED from the `Rc`-shared namespace records (kept alive in `ext_records`) —
    // deep-cloning every overload's `FunctionInfo` (params, call-sig vecs, generic sig) per call site
    // only to discard all but the winner dominated selection; only the selected overload is cloned.
    let owned_member_set;
    let owned_ext_records;
    let overloads: Vec<&FunctionInfo> = if let Some(functions) = provided_functions {
        functions.iter().collect()
    } else {
        match (mode, ext.fn_scope) {
            (SelectionMode::Kind(FnKind::Member), _) => {
                owned_member_set = members_in_hierarchy(src, recv, name).into_parts().0;
                owned_member_set.overloads.iter().collect()
            }
            (SelectionMode::Kind(FnKind::Extension), Some(scope)) => {
                owned_ext_records = symbols_in_function_scope(src, name, scope);
                owned_ext_records
                    .iter()
                    .flat_map(|record| match &record.callables {
                        crate::libraries::Callables::Functions(functions) => {
                            functions.overloads.as_slice()
                        }
                        crate::libraries::Callables::Both { functions, .. } => {
                            functions.overloads.as_slice()
                        }
                        _ => &[],
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    };
    // Candidates from the scoped query are IN-SCOPE by construction: each came from a `resolve_symbols`
    // over an imported package, so its declared package is in scope even when `@JvmPackageName` relocated
    // its facade to a different JVM package (`kotlin.collections`'s `UArraysKt` → `kotlin/collections/
    // unsigned/`). Re-deriving scope from the JVM owner (`fn_in_scope`) would wrongly drop those, so trust
    // the query.
    let pre_scoped = ext.fn_scope.is_some();
    crate::trace_compiler!(
        "resolve",
        "select_overload name={name} recv={recv:?} mode={mode:?} scope={:?} cands={}",
        ext.fn_scope.map(FunctionScopeRef::package_count),
        overloads.len(),
    );
    for o in &overloads {
        crate::trace_compiler!(
            "resolve",
            "  raw {name} kind={:?} recv={:?} params={:?} generic={:?} context={} required={} vararg={:?} pub={} rank={} origin={:?} owner={}",
            o.kind,
            o.semantic_receiver(),
            o.callable.params,
            o.generic_sig,
            o.context_count,
            o.call_sig.required,
            o.call_sig.vararg_index,
            o.public(),
            o.receiver_rank,
            o.callable.origin,
            o.callable.owner.render(),
        );
    }
    // Declaration-priority tiers, tried in this order and falling through whenever the tier holds no
    // applicable candidate (the `by_rank` walk below). Kotlin resolves a `@kotlin.internal.HidesMembers`
    // extension above members — the annotation exists so `Iterable<T>.forEach` wins over
    // `java.lang.Iterable.forEach(Consumer)` — and, as measured against kotlinc 2.4.10, above every
    // ordinary extension level as well, whatever its receiver specificity or lexical distance. Members
    // still precede ordinary extensions.
    const HIDES_MEMBERS_PRIORITY: u8 = 0;
    const MEMBER_PRIORITY: u8 = 1;
    const EXTENSION_PRIORITY: u8 = 2;

    let mut by_rank: std::collections::BTreeMap<(u8, u32, u32), Vec<(&FunctionInfo, Vec<Ty>)>> =
        std::collections::BTreeMap::new();
    let mut ranked: Vec<(u8, u32, u32, Ty, &FunctionInfo)> = Vec::new();
    if matches!(
        mode,
        SelectionMode::Kind(FnKind::Extension) | SelectionMode::Receiver
    ) {
        ranked.extend(
            ranked_extension_candidates(
                src,
                recv,
                overloads
                    .iter()
                    .copied()
                    .filter(|o| pre_scoped || fn_in_scope(o, ext.fn_scope)),
            )
            .into_iter()
            .map(|(rank, receiver, overload)| {
                (
                    if overload
                        .annotations
                        .contains(&crate::types::type_name("kotlin/internal/HidesMembers"))
                    {
                        HIDES_MEMBERS_PRIORITY
                    } else {
                        EXTENSION_PRIORITY
                    },
                    if overload
                        .annotations
                        .contains(&crate::types::type_name("kotlin/internal/HidesMembers"))
                    {
                        0
                    } else {
                        overload.scope_rank
                    },
                    rank,
                    receiver,
                    overload,
                )
            }),
        );
    }
    if matches!(
        mode,
        SelectionMode::Kind(FnKind::Member) | SelectionMode::Receiver
    ) && receiver_allows_member_dispatch(recv)
    {
        ranked.extend(
            overloads
                .iter()
                .copied()
                .filter(|o| o.kind == FnKind::Member)
                .map(|o| (MEMBER_PRIORITY, 0, o.receiver_rank, recv, o)),
        );
    }
    ranked.sort_by_key(|(priority, scope_rank, receiver_rank, _, _)| {
        (*priority, *scope_rank, *receiver_rank)
    });
    for (priority, scope_rank, receiver_rank, binding_receiver, o) in ranked {
        let lp = if indexed != IndexedConvention::Ordinary && o.call_sig.vararg_index.is_some() {
            let Some((params, _)) = indexed_call_shape(
                lib,
                src,
                o,
                binding_receiver,
                args,
                type_args,
                indexed == IndexedConvention::Set,
            ) else {
                crate::trace_compiler!(
                    "resolve",
                    "  drop {name} because indexed operands do not map to the declaration"
                );
                continue;
            };
            params
        } else {
            if !generic_function_call_admits(src, o, binding_receiver, args, type_args) {
                crate::trace_compiler!(
                    "resolve",
                    "  drop {name} because inferred type arguments violate declared bounds"
                );
                continue;
            }
            logical_call_params(src, o, binding_receiver, args, type_args)
        };
        let lp = apply_platform_call_parameter_nullability(
            lp,
            &o.call_sig.platform_nullable_params,
            &arg_tys,
            o.call_sig.vararg,
        );
        crate::trace_compiler!(
            "resolve",
            "  cand {name} scope_rank={scope_rank} receiver_rank={receiver_rank} logical_params={lp:?} owner={}",
            o.callable.owner.render()
        );
        by_rank
            .entry((priority, scope_rank, receiver_rank))
            .or_default()
            .push((o, lp));
    }
    if indexed != IndexedConvention::Ordinary {
        for cands in by_rank.values() {
            let fixed = cands
                .iter()
                .filter(|(candidate, _)| candidate.call_sig.vararg_index.is_none())
                .filter_map(|(candidate, params)| {
                    indexed_fixed_parameter_projection(candidate, params, args.len(), indexed)
                        .map(|projected| (*candidate, projected))
                })
                .collect::<Vec<_>>();
            match best_by_args(lib, assign_src, &fixed, args) {
                CandidateSelection::Selected(overload) => return Some(overload.clone()),
                CandidateSelection::Ambiguous => {
                    *ambiguous = true;
                    return None;
                }
                CandidateSelection::None => {}
            }
            let indexed_candidates = cands
                .iter()
                .filter_map(|(o, lp)| {
                    let vararg = o.call_sig.vararg_index?.checked_sub(o.context_count)?;
                    let element = lp.get(vararg)?.array_read_elem()?;
                    let trailing = usize::from(indexed == IndexedConvention::Set);
                    if vararg + 1 + trailing != lp.len() {
                        return None;
                    }
                    let index_count = args.len().checked_sub(trailing)?;
                    let mut expanded = lp[..vararg].to_vec();
                    expanded.extend(std::iter::repeat_n(
                        element,
                        index_count.checked_sub(vararg)?,
                    ));
                    if indexed == IndexedConvention::Set {
                        expanded.push(*lp.last()?);
                    }
                    Some((*o, expanded))
                })
                .collect::<Vec<_>>();
            match best_by_args(lib, assign_src, &indexed_candidates, args) {
                CandidateSelection::Selected(overload) => return Some(overload.clone()),
                CandidateSelection::Ambiguous => {
                    *ambiguous = true;
                    return None;
                }
                CandidateSelection::None => {}
            }
        }
        return None;
    }
    for cands in by_rank.values() {
        match best_by_args(lib, assign_src, cands, args) {
            CandidateSelection::Selected(overload) => return Some(overload.clone()),
            CandidateSelection::Ambiguous => {
                *ambiguous = true;
                return None;
            }
            CandidateSelection::None => {}
        }
    }
    // Vararg ELEMENT-expansion pass: a call passing loose elements (or nothing) where a
    // candidate declares a vararg (`"a.b".trim('.')` against `trim(vararg chars: Char)` — the
    // logical param is the ARRAY; `split('.')` against `split(vararg delimiters: Char,
    // ignoreCase: Boolean = false, limit: Int = 0)` — params after the vararg are reachable
    // only by name, so they must be defaulted). Two tiers per rank: EXACT element matches
    // first (`Char` argument selects the `Char` vararg over the `String` one, mirroring
    // most-specific selection), then platform/source-assignable elements.
    let vararg_applicable = |o: &FunctionInfo, lp: &[Ty], exact: bool| -> bool {
        // A suspend callee's element-form vararg call would route the $default emission
        // outside the CPS pass's coverage — skip (unresolved), never ICE.
        if o.flags.suspend {
            return false;
        }
        let Some(vararg_index) = o.call_sig.vararg_index else {
            return false;
        };
        let Some(array) = lp.get(vararg_index).copied() else {
            return false;
        };
        let Some(elem) = array.array_read_elem() else {
            return false;
        };
        args.len() >= vararg_index
            && lp[..vararg_index].iter().zip(args).all(|(p, a)| {
                let ty = a.ty();
                fun_arg_matches(assign_src, p, &ty, a.is_lambda_literal())
                    || semantic_arg_assignable(assign_src, p, &ty)
                    || a.binds_result_to(assign_src, *p)
            })
            && args[vararg_index..].iter().all(|a| {
                let ty = a.ty();
                // A SPREAD argument (`*xs`) fits the vararg's ARRAY type; a plain one, the element.
                let expected = if a.is_spread() { array } else { elem };
                ty == expected
                    || (!exact
                        && (semantic_arg_assignable(assign_src, &expected, &ty)
                            || a.binds_result_to(assign_src, expected)))
            })
            && (vararg_index + 1..lp.len()).all(|index| o.call_sig.param_has_default(index))
    };
    for exact in [true, false] {
        for cands in by_rank.values() {
            let mut applicable = cands
                .iter()
                .filter(|(o, lp)| vararg_applicable(o, lp, exact));
            if let Some((o, _)) = applicable.next() {
                if applicable.next().is_some() {
                    *ambiguous = true;
                    return None;
                }
                return Some((*o).clone());
            }
        }
    }
    None
}

fn generic_bounds_admit(
    src: &dyn SymbolSource,
    generic_sig: Option<&GenericSig>,
    receiver: Ty,
    args: &[Ty],
    type_args: &[Ty],
) -> bool {
    let Some(gsig) = generic_sig else {
        return true;
    };
    let mut binds = seeded_gsig_binds(gsig, type_args);
    let mut inferred = GSigBinds::new();
    if let Some(declared_receiver) = gsig.receiver {
        unify_inferred_ty_impl(Some(src), declared_receiver, receiver, &mut inferred);
    }
    for (&parameter, &argument) in gsig.params.iter().zip(args) {
        unify_inferred_ty_impl(Some(src), parameter, argument, &mut inferred);
    }
    merge_generic_bindings_from(Some(src), gsig, type_args, &mut binds, inferred);
    generic_bindings_satisfy_bounds(gsig, &binds, |actual, bound| {
        actual == bound
            || crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &SourceOracle(src),
                actual,
                bound,
            )
    })
}

/// Validate a function candidate's complete generic inference policy. Bounds and
/// `@OnlyInputTypes` are both declaration-owned constraints and must be decided before overload
/// ranking; otherwise a synthesized common supertype can make an inapplicable member/extension win
/// and the checker discovers the mismatch only after committing the wrong target.
fn generic_function_call_admits(
    src: &dyn SymbolSource,
    overload: &FunctionInfo,
    receiver: Ty,
    arguments: &[CallArgKind],
    type_args: &[Ty],
) -> bool {
    let signature = overload.semantic_signature();
    let mut bindings = seeded_gsig_binds(&signature, type_args);
    if let Some(declared_receiver) = signature.receiver {
        unify_ty_from_symbols(src, declared_receiver, receiver, &mut bindings);
    }
    let receiver_bindings = bindings.clone();
    let value_start = overload.context_count.min(signature.params.len());
    let actuals = signature.params[value_start..]
        .iter()
        .zip(arguments)
        .enumerate()
        .filter_map(|(value_parameter, (&declared, argument))| {
            let parameter = value_start + value_parameter;
            if !overload
                .call_sig
                .parameter_contributes_to_inference(parameter)
            {
                return None;
            }
            if argument.is_lambda_literal()
                || argument.is_expected_type_callable()
                || argument.is_omitted_default()
            {
                return None;
            }
            Some((
                parameter,
                argument.inference_type(src, declared),
                argument.is_spread(),
            ))
        })
        .collect::<Vec<_>>();
    let only_input_actuals = signature.params[value_start..]
        .iter()
        .zip(arguments)
        .enumerate()
        .filter_map(|(value_parameter, (_, argument))| {
            let parameter = value_start + value_parameter;
            overload
                .call_sig
                .parameter_contributes_to_inference(parameter)
                .then(|| (parameter, argument.clone(), argument.is_spread()))
        })
        .collect::<Vec<_>>();
    let inferred = infer_generic_call_bindings_from_symbols(
        src,
        &signature,
        actuals.iter().copied(),
        overload.call_sig.vararg_index,
    );
    merge_call_argument_bindings(
        src,
        &signature,
        type_args,
        &receiver_bindings,
        &mut bindings,
        inferred,
    );
    apply_only_input_type_bindings(
        src,
        &signature,
        &overload.call_sig.only_input_type_formals,
        type_args,
        Some(receiver),
        &only_input_actuals,
        overload.call_sig.vararg_index,
        None,
        &mut bindings,
    ) && generic_bindings_satisfy_bounds(&signature, &bindings, |actual, bound| {
        resolution_subtype(src, actual, bound)
    })
}

fn generic_bounds_admit_slots(
    src: &dyn SymbolSource,
    generic_sig: Option<&GenericSig>,
    call_sig: &CallSig,
    receiver: Ty,
    slots: &[Option<Ty>],
    type_args: &[Ty],
) -> bool {
    let Some(gsig) = generic_sig else {
        return true;
    };
    let mut binds = seeded_gsig_binds(gsig, type_args);
    let mut inferred = GSigBinds::new();
    if let Some(declared_receiver) = gsig.receiver {
        unify_inferred_ty_impl(Some(src), declared_receiver, receiver, &mut inferred);
    }
    for (index, (&parameter, argument)) in gsig.params.iter().zip(slots).enumerate() {
        if !call_sig.parameter_contributes_to_inference(index) {
            continue;
        }
        if let Some(argument) = argument {
            unify_inferred_ty_impl(Some(src), parameter, *argument, &mut inferred);
        }
    }
    merge_generic_bindings_from(Some(src), gsig, type_args, &mut binds, inferred);
    let actuals = slots
        .iter()
        .enumerate()
        .filter_map(|(parameter, argument)| {
            call_sig
                .parameter_contributes_to_inference(parameter)
                .then(|| argument.map(|argument| (parameter, CallArgKind::Typed(argument), false)))
                .flatten()
        })
        .collect::<Vec<_>>();
    apply_only_input_type_bindings(
        src,
        gsig,
        &call_sig.only_input_type_formals,
        type_args,
        Some(receiver),
        &actuals,
        call_sig.vararg_index,
        None,
        &mut binds,
    ) && generic_bindings_satisfy_bounds(gsig, &binds, |actual, bound| {
        actual == bound
            || crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &SourceOracle(src),
                actual,
                bound,
            )
    })
}

/// LOGICAL value parameters of an overload — what a call site's arguments are matched against, with the
/// receiver excluded (it is an attribute). Member/top-level `callable.params` are already value-only; an
/// extension's `callable.params` prepend the receiver in the JVM emit shape, so bind the generic signature
/// to `recv` and drop the leading receiver, preferring each parameter's value-class LOGICAL type over its
/// erased underlying (`Id` over `kotlin/String`).
fn logical_value_params(
    source: &dyn SymbolSource,
    o: &FunctionInfo,
    recv: Ty,
    type_args: &[Ty],
) -> Vec<Ty> {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, type_args);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, recv, &mut binds);
    }
    let params = semantic
        .params
        .iter()
        .map(|parameter| {
            instantiate_slot(
                source,
                Some(&semantic),
                *parameter,
                &binds,
                TypePosition::In,
                UnboundSpecialization::Preserve,
            )
        })
        .collect::<Vec<_>>();
    params[o.context_count.min(params.len())..].to_vec()
}

/// Logical value parameters specialized by the complete call constraint set. Extension receivers and
/// arguments constrain the same declaration formals, so shaping them in separate passes can freeze a
/// bottom receiver result (`() -> Nothing`) before a concrete lambda supplies `String`.
fn logical_call_params(
    source: &dyn SymbolSource,
    overload: &FunctionInfo,
    receiver: Ty,
    arguments: &[CallArgKind],
    type_arguments: &[Ty],
) -> Vec<Ty> {
    let signature = overload.semantic_signature();
    let mut bindings = seeded_gsig_binds(&signature, type_arguments);
    if let Some(declared_receiver) = signature.receiver {
        unify_ty(declared_receiver, receiver, &mut bindings);
    }
    // A receiver occurrence fixes the callable formal before value arguments are considered:
    // `String : Comparable<String>` makes the `T` in
    // `fun <T> Comparable<T>.compareTo(other: T)` exactly `String`. Joining a later `Int`
    // argument into that binding as the common supertype `Any` would rewrite the receiver to
    // `Comparable<Any>` and admit a call the receiver does not implement. Bottom is the sole open
    // receiver constraint: `() -> Nothing` may still be refined by a concrete value argument.
    let receiver_bindings = bindings.clone();
    let inferred = infer_generic_call_bindings_from_symbols(
        source,
        &signature,
        signature
            .params
            .iter()
            .zip(arguments)
            .enumerate()
            .filter_map(|(parameter, (&declared, argument))| {
                // A nested generic call's erased provisional result is not an input constraint.
                // Once this candidate supplies a parameter, argument checking propagates it into
                // the nested call.
                (!argument.is_expected_type_callable()
                    && !argument.is_lambda_literal()
                    && !argument.is_omitted_default())
                .then_some((
                    parameter,
                    argument.inference_type(source, declared),
                    argument.is_spread(),
                ))
            }),
        overload.call_sig.vararg_index,
    );
    merge_call_argument_bindings(
        source,
        &signature,
        type_arguments,
        &receiver_bindings,
        &mut bindings,
        inferred,
    );
    let parameters = signature
        .params
        .iter()
        .map(|parameter| {
            instantiate_slot(
                source,
                Some(&signature),
                *parameter,
                &bindings,
                TypePosition::In,
                UnboundSpecialization::Preserve,
            )
        })
        .collect::<Vec<_>>();
    parameters[overload.context_count.min(parameters.len())..].to_vec()
}

/// Specialize a declaration for Kotlin's indexed-set operand convention. The final written operand
/// binds the final value parameter after all loose index operands bind the preceding vararg element.
fn indexed_call_shape(
    lib: &dyn SemanticPlatform,
    source: &dyn SymbolSource,
    overload: &FunctionInfo,
    receiver: Ty,
    arguments: &[CallArgKind],
    type_arguments: &[Ty],
    set: bool,
) -> Option<(Vec<Ty>, Ty)> {
    let signature = overload.semantic_signature();
    let value_start = overload.context_count.min(signature.params.len());
    let vararg = overload.call_sig.vararg_index?;
    let logical_vararg = vararg.checked_sub(value_start)?;
    // Defaults between the vararg and value are valid Kotlin, but synthetic operator-call lowering
    // cannot realize omitted default slots yet. Keep selection aligned with that handoff.
    let trailing = usize::from(set);
    if vararg + 1 + trailing != signature.params.len() {
        return None;
    }
    let index_arguments = if set {
        arguments.split_last()?.1
    } else {
        arguments
    };
    if index_arguments.len() < logical_vararg {
        return None;
    }

    let mut bindings = seeded_gsig_binds(&signature, type_arguments);
    if let Some(declared_receiver) = signature.receiver {
        unify_ty(declared_receiver, receiver, &mut bindings);
    }
    let receiver_bindings = bindings.clone();
    let value_parameter = signature.params.len() - 1;
    let actuals = arguments
        .iter()
        .enumerate()
        .filter_map(|(source_index, argument)| {
            if argument.is_expected_type_callable()
                || argument.is_lambda_literal()
                || argument.is_omitted_default()
            {
                return None;
            }
            let parameter = if source_index < logical_vararg {
                value_start + source_index
            } else if set && source_index + 1 == arguments.len() {
                value_parameter
            } else {
                vararg
            };
            let declared = *signature.params.get(parameter)?;
            let whole_array = argument.is_spread();
            let expected = if parameter == vararg && !whole_array {
                declared.array_read_elem().unwrap_or(declared)
            } else {
                declared
            };
            Some((
                parameter,
                argument.inference_type(source, expected),
                whole_array,
            ))
        });
    let inferred =
        infer_generic_call_bindings_from_symbols(source, &signature, actuals, Some(vararg));
    merge_call_argument_bindings(
        source,
        &signature,
        type_arguments,
        &receiver_bindings,
        &mut bindings,
        inferred,
    );
    if !generic_bindings_satisfy_bounds(&signature, &bindings, |actual, bound| {
        resolution_subtype(source, actual, bound)
    }) {
        return None;
    }
    let params = signature.params[value_start..]
        .iter()
        .map(|parameter| ty_subst_keep_unbound(*parameter, &bindings))
        .collect::<Vec<_>>();
    let inferred_ret = if overload.is_extension() {
        specialize_signature_output_type(source, signature.ret, &bindings)
    } else {
        ty_subst_keep_unbound(signature.ret, &bindings)
    };
    let ret = if overload.is_extension() {
        specialized_extension_return(lib, overload, inferred_ret)
    } else {
        overload
            .ret
            .apply(signature.apply_return_policy(lib, inferred_ret))
    };
    Some((params, ret))
}

/// Assignability through the SOURCE symbol federation (module classes first): a module-declared
/// class passed where a library member expects its (library) supertype — `class V : Thread()` into
/// `take(Thread)` — is invisible to the platform oracle, which only walks classpath supertypes.
fn semantic_arg_assignable(src: &dyn SymbolSource, param: &Ty, arg: &Ty) -> bool {
    if let Ty::TyParam(_, bound) = param.non_null() {
        if *arg == Ty::Null {
            return param.admits_null();
        }
        let expected = if param.is_nullable() {
            Ty::nullable(*bound)
        } else {
            *bound
        };
        return crate::assignable::is_assignable(
            &crate::assignable::TyCtx::new(),
            &SourceOracle(src),
            *arg,
            expected,
        );
    }
    crate::assignable::is_assignable(
        &crate::assignable::TyCtx::new(),
        &SourceOracle(src),
        *arg,
        *param,
    )
}

fn distinct_source_declarations(left: &FunctionInfo, right: &FunctionInfo) -> bool {
    left.source_key.is_some() && right.source_key.is_some() && left.source_key != right.source_key
}

fn source_aware_most_specific<'a, I>(
    candidates: I,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelection<&'a FunctionInfo>
where
    I: Iterator<Item = (Vec<Ty>, &'a FunctionInfo)> + Clone,
{
    unique_most_specific_with_conflicts(candidates, at_least_as_specific, |left, right| {
        distinct_source_declarations(left, right)
    })
}

fn declaration_specificity_params(candidate: &FunctionInfo) -> Vec<Ty> {
    let signature = candidate.semantic_signature();
    signature
        .params
        .iter()
        .skip(candidate.context_count.min(signature.params.len()))
        .map(|parameter| ty_subst(*parameter, &GSigBinds::new()))
        .collect()
}

/// Pick the best overload whose logical value parameters accept `args`, in Kotlin applicability order:
/// exact, then `Any`-widened / function-arity, then a prefix under-application (omitted trailing params
/// must be optional), then a trailing-lambda call that omits leading DEFAULTED params (`m.withLock { … }`).
pub(crate) fn best_by_args<'a>(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    cands: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[CallArgKind],
) -> CandidateSelection<&'a FunctionInfo> {
    let ordinary = cands
        .iter()
        .filter(|(candidate, _)| {
            !candidate.annotations.contains(&crate::types::type_name(
                "kotlin/internal/LowPriorityInOverloadResolution",
            ))
        })
        .cloned()
        .collect::<Vec<_>>();
    match best_by_args_at_priority(lib, src, &ordinary, args) {
        CandidateSelection::None => {
            let low = cands
                .iter()
                .filter(|(candidate, _)| {
                    candidate.annotations.contains(&crate::types::type_name(
                        "kotlin/internal/LowPriorityInOverloadResolution",
                    ))
                })
                .cloned()
                .collect::<Vec<_>>();
            best_by_args_at_priority(lib, src, &low, args)
        }
        selected => selected,
    }
}

/// Select within one declaration-priority tier. Applicability and specificity deliberately know
/// nothing about `@LowPriorityInOverloadResolution`; the outer tiering step invokes this same selector
/// first for ordinary declarations and only then for low-priority declarations.
fn best_by_args_at_priority<'a>(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    cands: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[CallArgKind],
) -> CandidateSelection<&'a FunctionInfo> {
    // Exact passes see runtime types; literal provenance only drives the adaptation passes.
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    let adapts = |p: &Ty, arg: &CallArgKind, _i: usize| arg.adapts_integer_literal_to(*p);
    let function_like_fits = |p: &Ty, arg: &CallArgKind| {
        arg.function_type()
            .filter(|function| *function != arg.ty())
            .is_some_and(|function| {
                arg_fits_platform(lib, p, &function) || semantic_arg_assignable(src, p, &function)
            })
    };
    // The DEFAULT-omitting passes accept a reference SUBTYPE / value-class-underlying argument (a
    // `joinToString(separator: CharSequence = …)` call with a `String`), matching the assignability the
    // exact-arity subtype pass in `select_overload` applies — the exact/`Any`-widened passes above stay
    // stricter so an exact call still prefers its precise overload.
    let fits = |_position: usize, p: &Ty, arg: &CallArgKind| {
        if arg.is_omitted_default() {
            return true;
        }
        let sam = p.non_null().fun_arity().is_none()
            && (arg.is_lambda_literal() || arg.function_type().is_some())
            && sam_arg_matches(lib, src, *p, arg.function_type().unwrap_or(arg.ty()));
        if arg.is_lambda_literal() && arg.ty() == Ty::Error {
            return untyped_lambda_pertinent(lib, src, *p);
        }
        sam || fun_arg_matches(src, p, &arg.ty(), arg.is_lambda_literal())
            || semantic_arg_assignable(src, p, &arg.ty())
            || (arg.ty() == Ty::Null && matches!(p.non_null(), Ty::TyParam(..)))
            || function_like_fits(p, arg)
            || arg.binds_result_to(src, *p)
    };
    match source_aware_most_specific(
        cands
            .iter()
            .filter(|(_, params)| params.as_slice() == arg_tys)
            .map(|(candidate, _)| (declaration_specificity_params(candidate), *candidate)),
        |_, left, right| {
            parameter_at_least_as_specific(src, left, right, CallArgKind::Typed(Ty::Error))
        },
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }
    match integer_literal_overload(
        cands
            .iter()
            .map(|(candidate, params)| (params.clone(), *candidate)),
        args,
        |position, param, arg| fits(position, param, arg),
        |_position, left, right, arg| parameter_at_least_as_specific(src, left, right, arg),
        |left, right| distinct_source_declarations(left, right),
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }
    let specificity = |_: usize, left: Ty, right: Ty| {
        parameter_at_least_as_specific(src, left, right, CallArgKind::Typed(Ty::Error))
    };

    // Expected-result inference may make several otherwise unrelated overloads applicable. Unlike
    // ordinary classpath assignability, declaration order cannot choose among them: the inferred
    // result would then depend on provider iteration order. Run the same unique-most-specific rule
    // used for source declarations and report incomparable maxima as an ambiguity.
    if args.iter().any(CallArgKind::is_expected_type_callable) {
        match unique_most_specific_with_conflicts(
            cands.iter().filter_map(|(candidate, params)| {
                fixed_parameter_shape(params, args, |position, param, arg| {
                    fits(position, param, arg)
                })
                .map(|shape| (shape, *candidate))
            }),
            specificity,
            |left, right| distinct_source_declarations(left, right),
        ) {
            CandidateSelection::Selected(candidate) => {
                return CandidateSelection::Selected(candidate);
            }
            CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
            CandidateSelection::None => {}
        }
    }

    // Exact arity is judged by the one semantic assignability relation. Every applicable overload
    // competes in the same most-specific selection; there is no later descriptor/erasure retry.
    match source_aware_most_specific(
        cands.iter().filter_map(|(candidate, params)| {
            fixed_parameter_shape(params, args, |position, param, arg| {
                fits(position, param, arg)
            })
            .map(|shape| (shape, *candidate))
        }),
        specificity,
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }

    match source_aware_most_specific(
        cands.iter().filter_map(|(candidate, params)| {
            (candidate.call_sig.required == 0 || candidate.call_sig.required <= args.len())
                .then(|| {
                    omitted_parameter_shape(params, args, |position, param, arg| {
                        fits(position, param, arg) || adapts(param, arg, position)
                    })
                    .map(|shape| (shape, *candidate))
                })
                .flatten()
        }),
        specificity,
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }

    if matches!(args.last(), Some(arg) if arg.ty().fun_arity().is_some()) {
        match source_aware_most_specific(
            cands.iter().filter_map(|(candidate, params)| {
                let last = params.len().checked_sub(1)?;
                let prefix = args.len().checked_sub(1)?;
                let lambda_fits = prefix <= last && fits(last, &params[last], args.last().unwrap());
                let defaults_fit = (prefix..last)
                    .all(|i| candidate.call_sig.param_has_default(i))
                    || candidate.call_sig.required <= prefix;
                let prefix_fits = params[..prefix.min(params.len())]
                    .iter()
                    .zip(&arg_tys[..prefix])
                    .enumerate()
                    .all(|(i, (param, _arg))| {
                        fits(i, param, &args[i]) || adapts(param, &args[i], i)
                    });
                crate::trace_compiler!(
                    "resolve",
                    "trailing lambda candidate={} prefix={prefix} last={last} lambda_fits={lambda_fits} defaults_fit={defaults_fit} prefix_fits={prefix_fits} expected={:?} actual={:?}",
                    candidate.callable.name,
                    params[last],
                    args.last(),
                );
                (prefix <= last && lambda_fits && defaults_fit && prefix_fits)
                    .then(|| (params.clone(), *candidate))
            }),
            specificity,
        ) {
            CandidateSelection::Selected(candidate) => {
                return CandidateSelection::Selected(candidate);
            }
            CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
            CandidateSelection::None => {}
        }
    }

    source_aware_most_specific(
        cands.iter().filter_map(|(candidate, params)| {
            candidate.call_sig.vararg.then(|| {
                vararg_parameter_shape(params, args, |position, param, arg| {
                    fits(position, param, arg) || adapts(param, arg, position)
                })
                .map(|shape| (shape, *candidate))
            })?
        }),
        specificity,
    )
}

/// A lambda argument (`Ty::Fun`) matches a decoded function-typed parameter of the same arity.
/// Providers must expose callable shape from metadata; source resolution never derives it from a
/// runtime classifier's spelling.
fn fun_arg_matches(
    src: &dyn SymbolSource,
    param: &Ty,
    arg: &Ty,
    allow_unit_coercion: bool,
) -> bool {
    let Some(arg_arity) = arg.fun_arity() else {
        return false;
    };
    let param = match param {
        Ty::Nullable(inner) => **inner,
        _ => *param,
    };
    param.fun_arity().is_some_and(|pn| pn == arg_arity)
        && fun_return_compatible(src, param, *arg, allow_unit_coercion)
}

/// A function-typed argument fits a function-typed parameter's RETURN. A parameter `(T) -> R` with a
/// CONCRETE `R` (`sumOfInt`'s `(T) -> Int`) accepts ONLY a lambda whose body returns that `R` — this is
/// how a `@OverloadResolutionByLambdaReturnType` group (whose overloads share value params and differ only
/// in the selector's return) is resolved: the lambda's return is just another parameter of the check. A
/// type-variable / erased-`Any` parameter return (an ordinary generic HOF `(T) -> R`), or an unresolved
/// lambda body, stays permissive so normal HOFs keep matching.
fn fun_return_compatible(
    src: &dyn SymbolSource,
    param: Ty,
    arg: Ty,
    allow_unit_coercion: bool,
) -> bool {
    let (Some(pr), Some(ar)) = (param.fun_ret(), arg.fun_ret()) else {
        return true;
    };
    if matches!(pr.non_null(), Ty::TyParam(_, _))
        || matches!(pr, Ty::Error)
        || pr
            .non_null()
            .obj_internal()
            .is_some_and(|n| n.matches("kotlin/Any"))
        || (allow_unit_coercion && pr == Ty::Unit)
    {
        return true;
    }
    if matches!(ar, Ty::Error | Ty::Nothing) {
        return true;
    }
    if pr.non_null() == ar.non_null() {
        return true;
    }
    // A CONCRETE REFERENCE return is covariant: a lambda whose body returns a SUBTYPE (`String`) fits a
    // `(T) -> CharSequence` transform parameter (`joinToString`). Primitive returns stay INVARIANT — the
    // `@OverloadResolutionByLambdaReturnType` families (`sumOf { Int } / { Double }`) differ only by their
    // exact primitive return and must not cross-match.
    if let (Some(p), Some(a)) = (
        pr.non_null().kotlin_class_internal(),
        ar.non_null().kotlin_class_internal(),
    ) {
        if pr.is_reference() && ar.is_reference() {
            let compatible = resolution_subtype(src, Ty::obj_name(a), Ty::obj_name(p));
            crate::trace_compiler!(
                "resolve",
                "function return covariance actual={} expected={} compatible={compatible} actual_supertypes={:?}",
                a.render(),
                p.render(),
                src.classifier(a)
                    .map(|classifier| classifier.supertypes.iter_rendered().collect::<Vec<_>>())
                    .unwrap_or_default(),
            );
            return compatible;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        CallSig, DefaultCallRealization, FunctionSet, GenericReturnPolicy, LibraryCallable,
        LibraryMember, LibraryType, Origin, TypeKind,
    };
    use crate::symbol_source::SymbolSource;
    use crate::types::type_name;

    struct EmptySource;
    impl SymbolSource for EmptySource {}
    impl SemanticPlatform for EmptySource {}
    const EMPTY_SOURCE: EmptySource = EmptySource;

    #[test]
    fn classifier_bindings_reconstruct_recursive_star_bound_from_declaration() {
        let nullable_any = Ty::nullable(Ty::obj("kotlin/Any"));
        let r = Ty::ty_param("R", nullable_any);
        let t = Ty::ty_param("T", nullable_any);
        let mut classifier = LibraryType::declaration_header();
        classifier.type_parameters = crate::types::TypeParameters::new(
            vec!["R".to_string(), "T".to_string()],
            vec![vec![nullable_any], vec![Ty::obj_args("a/Rec", &[r, t])]],
            vec![
                crate::types::TypeVariance::Invariant,
                crate::types::TypeVariance::Out,
            ],
        );
        let metadata_star = Ty::star_projection(nullable_any);
        let bindings = classifier_bindings(
            &classifier,
            Ty::obj_args("a/Rec", &[metadata_star, metadata_star]),
        );

        assert_eq!(bindings.get("R"), Some(&metadata_star));
        assert_eq!(
            bindings.get("T"),
            Some(&Ty::obj_args("a/Rec", &[metadata_star, metadata_star]))
        );
    }

    #[test]
    fn classifier_bindings_do_not_reapply_a_source_star_upper_bound() {
        let nullable_any = Ty::nullable(Ty::obj("kotlin/Any"));
        let self_parameter = Ty::ty_param("S", nullable_any);
        let mut classifier = LibraryType::declaration_header();
        classifier.type_parameters = crate::types::TypeParameters::new(
            vec!["S".to_string()],
            vec![vec![Ty::obj_args("a/Entity", &[self_parameter])]],
            vec![crate::types::TypeVariance::Invariant],
        );
        let denotable_bound = Ty::obj_args("a/Entity", &[Ty::star_projection(nullable_any)]);
        let source_star = Ty::star_projection(denotable_bound);

        let bindings = classifier_bindings(&classifier, Ty::obj_args("a/Entity", &[source_star]));

        assert_eq!(bindings.get("S"), Some(&source_star));
    }

    #[test]
    fn untyped_lambda_applicability_does_not_infer_function_shape_from_a_name() {
        assert!(!untyped_lambda_pertinent(
            &EMPTY_SOURCE,
            &EMPTY_SOURCE,
            Ty::obj("kotlin/FunctionalButNotCallable"),
        ));
    }

    #[test]
    fn partial_and_final_substitution_share_the_recursive_type_walk() {
        // Owner binding happens before method inference. Exercise every recursive carrier here so
        // adding a new substitution shape cannot make the partial path preserve structure differently
        // from the final path: the ONLY intended policy difference is the unbound method variable.
        let any = Ty::obj("kotlin/Any");
        let owner = Ty::ty_param("Owner", any);
        let method = Ty::ty_param("Method", any);
        let signature = Ty::nullable(Ty::fun(
            vec![Ty::obj_args("fixtures/Box", &[owner]), method],
            Ty::obj_args("fixtures/Result", &[method]),
        ));
        let bindings = GSigBinds::from([("Owner".to_string(), Ty::String)]);

        assert_eq!(
            ty_subst_keep_unbound(signature, &bindings),
            Ty::nullable(Ty::fun(
                vec![Ty::obj_args("fixtures/Box", &[Ty::String]), method],
                Ty::obj_args("fixtures/Result", &[method]),
            ))
        );
        assert_eq!(
            ty_subst(signature, &bindings),
            Ty::nullable(Ty::fun(
                vec![Ty::obj_args("fixtures/Box", &[Ty::String]), any],
                Ty::obj_args("fixtures/Result", &[any]),
            ))
        );
    }

    #[test]
    fn final_extension_output_consumes_projection_without_preserving_unbound_formals() {
        let any = Ty::obj("kotlin/Any");
        let output = Ty::ty_param("R", any);

        assert_eq!(
            specialize_signature_output_type(&EMPTY_SOURCE, output, &GSigBinds::new()),
            output
        );
        assert_eq!(
            specialize_final_signature_output_type(&EMPTY_SOURCE, output, &GSigBinds::new()),
            any
        );
        assert_eq!(
            specialize_final_signature_output_type(
                &EMPTY_SOURCE,
                output,
                &GSigBinds::from([("R".to_string(), Ty::out_projection(Ty::String),)]),
            ),
            Ty::String
        );
    }

    #[test]
    fn inferred_generic_binding_joins_null_with_the_non_null_element_type() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let mut inferred = GSigBinds::new();
        unify_inferred_ty(parameter, Ty::Int, &mut inferred);
        unify_inferred_ty(parameter, Ty::Null, &mut inferred);
        unify_inferred_ty(parameter, Ty::Int, &mut inferred);
        assert_eq!(inferred.get("T"), Some(&Ty::nullable(Ty::Int)));

        let mut explicit = GSigBinds::from([("T".to_string(), Ty::Int)]);
        unify_ty(parameter, Ty::Null, &mut explicit);
        assert_eq!(explicit.get("T"), Some(&Ty::Int));
    }

    fn subtype_chain_signature() -> GenericSig {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let c = Ty::ty_param("C", any);
        let r = Ty::ty_param("R", any);
        let q = Ty::ty_param("Q", any);
        GenericSig {
            formals: vec!["C".to_string(), "R".to_string(), "Q".to_string()],
            formal_bounds: vec![vec![r], vec![q], vec![any]],
            receiver: Some(c),
            params: Vec::new(),
            ret: q,
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    #[test]
    fn bottom_bindings_complete_transitively_in_the_real_binding_map() {
        let signature = subtype_chain_signature();
        let mut bindings = GSigBinds::from([
            ("C".to_string(), Ty::String),
            ("R".to_string(), Ty::Nothing),
            ("Q".to_string(), Ty::Null),
        ]);

        complete_bottom_constraint_bindings(&signature, &mut bindings, 0);

        assert_eq!(bindings.get("R"), Some(&Ty::String));
        assert_eq!(bindings.get("Q"), Some(&Ty::nullable(Ty::String)));
    }

    #[test]
    fn explicitly_written_bottom_binding_is_not_completed() {
        let signature = subtype_chain_signature();
        let mut bindings = GSigBinds::from([
            ("C".to_string(), Ty::String),
            ("R".to_string(), Ty::Nothing),
            ("Q".to_string(), Ty::Null),
        ]);

        complete_bottom_constraint_bindings(&signature, &mut bindings, 3);

        assert_eq!(bindings.get("R"), Some(&Ty::Nothing));
        assert_eq!(bindings.get("Q"), Some(&Ty::Null));
    }

    #[test]
    fn source_constraint_join_is_order_independent_for_flexible_and_projected_types() {
        let flexible = Ty::platform_nullable(Ty::String);
        for (left, right) in [(Ty::String, flexible), (flexible, Ty::String)] {
            assert_eq!(
                merge_inferred_ty_from_symbols(Some(&EMPTY_SOURCE), left, right),
                flexible
            );
        }

        let projected = Ty::out_projection(Ty::String);
        assert_eq!(
            merge_inferred_ty_from_symbols(Some(&EMPTY_SOURCE), projected, Ty::String),
            Ty::String
        );
        assert_eq!(
            merge_inferred_ty_from_symbols(Some(&EMPTY_SOURCE), Ty::String, projected),
            Ty::String
        );
    }

    #[test]
    fn specialized_return_does_not_restore_an_unbound_provider_formal() {
        let result = Ty::ty_param("R", Ty::nullable(Ty::obj("kotlin/Any")));

        assert_eq!(merge_specialized_return(result, Ty::Int), Ty::Int);
        assert_eq!(
            merge_specialized_return(
                Ty::obj_args("kotlin/collections/List", &[result]),
                Ty::obj_args("kotlin/collections/List", &[Ty::Int]),
            ),
            Ty::obj_args("kotlin/collections/List", &[Ty::Int]),
        );
        assert_eq!(
            merge_specialized_return(
                Ty::platform_nullable(Ty::obj_args(
                    "kotlin/Array",
                    &[Ty::out_projection(Ty::platform_nullable(Ty::obj(
                        "kotlin/Any",
                    )))],
                )),
                Ty::platform_nullable(
                    Ty::obj_args("kotlin/Array", &[Ty::out_projection(Ty::Int)],)
                ),
            ),
            Ty::platform_nullable(Ty::obj_args("kotlin/Array", &[Ty::out_projection(Ty::Int)],)),
        );
    }

    #[test]
    fn diverging_constraint_does_not_freeze_an_ordinary_generic_binding() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let mut bindings = GSigBinds::new();

        unify_ty(parameter, Ty::Nothing, &mut bindings);
        unify_ty(parameter, Ty::String, &mut bindings);

        assert_eq!(bindings.get("T"), Some(&Ty::String));
    }

    #[test]
    fn postponed_identity_constraint_does_not_hide_later_concrete_evidence() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let mut inferred = GSigBinds::new();

        unify_inferred_ty(parameter, parameter, &mut inferred);
        assert!(inferred.is_empty());

        unify_inferred_ty(parameter, Ty::obj("fixtures/Concrete"), &mut inferred);
        assert_eq!(inferred.get("T"), Some(&Ty::obj("fixtures/Concrete")));
    }

    #[test]
    fn contextual_identity_is_collected_through_a_nested_result() {
        let formal = Ty::ty_param("T", Ty::obj("fixtures/Marker"));
        let declared = Ty::obj_args("kotlin/collections/List", &[formal]);
        let expected = Ty::obj_args("kotlin/collections/List", &[formal]);

        assert_eq!(
            infer_generic_symbolic_return_constraints(declared, expected, &["T".to_string()])
                .bindings
                .get("T"),
            Some(&formal)
        );
        assert!(infer_generic_symbolic_return_constraints(
            Ty::obj_args("kotlin/collections/Set", &[formal]),
            expected,
            &["T".to_string()],
        )
        .bindings
        .is_empty());
    }

    #[test]
    fn contextual_identity_preserves_nested_expected_nullability() {
        let formal = Ty::ty_param("T", Ty::obj("fixtures/Marker"));
        let declared = Ty::obj_args("kotlin/collections/List", &[formal]);
        let expected = Ty::obj_args("kotlin/collections/List", &[Ty::nullable(formal)]);

        assert_eq!(
            infer_generic_symbolic_return_constraints(declared, expected, &["T".to_string()])
                .bindings
                .get("T"),
            Some(&Ty::nullable(formal))
        );
    }

    #[test]
    fn repeated_symbolic_return_constraints_must_agree() {
        let callee = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let caller = Ty::ty_param("U", Ty::obj("kotlin/Any"));
        let declared = Ty::obj_args("fixtures/Duo", &[callee, callee, callee]);
        let expected = Ty::obj_args("fixtures/Duo", &[caller, Ty::nullable(caller), caller]);

        let constraints =
            infer_generic_symbolic_return_constraints(declared, expected, &["T".to_string()]);
        assert!(constraints.bindings.is_empty());
        assert!(constraints.constrained_formals.contains("T"));
        assert!(constraints.conflicting_formals.contains("T"));
    }

    #[test]
    fn symbolic_and_concrete_return_constraints_conflict() {
        let callee = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let caller = Ty::ty_param("U", Ty::obj("kotlin/Any"));
        let constraints = infer_generic_symbolic_return_constraints(
            Ty::obj_args("fixtures/Duo", &[callee, callee]),
            Ty::obj_args("fixtures/Duo", &[caller, Ty::String]),
            &["T".to_string()],
        );

        assert!(constraints.bindings.is_empty());
        assert!(constraints.conflicting_formals.contains("T"));
    }

    #[test]
    fn null_argument_does_not_constrain_a_nullable_type_parameter() {
        let any = Ty::obj("kotlin/Any");
        let parameter = Ty::ty_param("T", any);
        let signature = GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![any]],
            receiver: None,
            params: vec![Ty::nullable(parameter), parameter],
            ret: parameter,
            return_policy: GenericReturnPolicy::Exact,
        };
        let binding = Ty::obj("fixtures/B");

        assert_eq!(
            infer_generic_bindings(&signature, [(0, Ty::Null), (1, binding)]).get("T"),
            Some(&binding)
        );
    }

    #[test]
    fn null_is_assignable_to_a_nullable_type_parameter_with_a_non_null_bound() {
        let parameter = Ty::nullable(Ty::ty_param("T", Ty::obj("kotlin/Any")));
        assert!(semantic_arg_assignable(
            &EMPTY_SOURCE,
            &parameter,
            &Ty::Null
        ));
    }

    #[test]
    fn nullable_upper_bound_does_not_make_a_bare_type_parameter_nullable() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        assert!(!semantic_arg_assignable(
            &EMPTY_SOURCE,
            &parameter,
            &Ty::Null
        ));
    }

    #[test]
    fn generic_unification_requires_matching_outer_classifiers() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let mut bindings = GSigBinds::new();

        unify_inferred_ty(
            Ty::array(parameter),
            Ty::obj_args("fixtures/Wrapper", &[Ty::Int]),
            &mut bindings,
        );

        assert!(bindings.is_empty());
    }

    #[test]
    fn generic_vararg_inference_uses_the_element_shape() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let parameter = Ty::ty_param("T", any);
        let signature = GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![any]],
            receiver: None,
            params: vec![Ty::array(parameter)],
            ret: parameter,
            return_policy: GenericReturnPolicy::Exact,
        };
        let wrapper = Ty::nullable(Ty::obj_args("fixtures/Wrapper", &[Ty::Int]));

        let bindings = infer_generic_call_bindings(&signature, [(0, wrapper, false)], Some(0));

        assert_eq!(bindings.get("T"), Some(&wrapper));
    }

    #[test]
    fn nullable_function_result_binds_its_non_null_type_parameter() {
        let parameter = Ty::fun(
            vec![Ty::String],
            Ty::nullable(Ty::ty_param("R", Ty::obj("kotlin/Any"))),
        );
        let argument = Ty::fun(vec![Ty::String], Ty::nullable(Ty::Int));
        let mut bindings = GSigBinds::new();

        unify_ty(parameter, argument, &mut bindings);

        assert_eq!(bindings.get("R"), Some(&Ty::Int));
    }

    #[test]
    fn nullable_function_preserves_lambda_parameter_types() {
        let function = Ty::nullable(Ty::fun(vec![Ty::String], Ty::String));

        assert_eq!(
            function_input_types(&VarianceSource, function, &GSigBinds::new()),
            vec![Ty::String]
        );
    }

    #[test]
    fn lambda_input_shape_preserves_an_unbound_type_parameter() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let function = Ty::fun(vec![parameter], Ty::Unit);

        assert_eq!(
            function_input_types(&VarianceSource, function, &GSigBinds::new()),
            vec![parameter]
        );
    }

    #[test]
    fn member_return_separates_method_and_owner_bindings() {
        let any = Ty::obj("kotlin/Any");
        let value = Ty::obj("demo/Value");
        let parameter = Ty::ty_param("T", any);
        let class_of = |ty| Ty::obj_args("java/lang/Class", &[ty]);
        let optional_of = |ty| Ty::obj_args("java/util/Optional", &[ty]);

        let method_generic = GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![Vec::new()],
            receiver: None,
            params: vec![class_of(parameter)],
            ret: optional_of(parameter),
            return_policy: Default::default(),
        };
        assert_eq!(
            bind_member_return(
                &EMPTY_SOURCE,
                &method_generic,
                Ty::obj("demo/Provider"),
                &[class_of(value)],
                &[],
                optional_of(any),
            ),
            optional_of(value)
        );

        let owner_generic = GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: vec![parameter],
            ret: parameter,
            return_policy: Default::default(),
        };
        assert_eq!(
            bind_member_return(
                &EMPTY_SOURCE,
                &owner_generic,
                optional_of(value),
                &[Ty::Null],
                &[],
                value,
            ),
            value
        );
        assert_eq!(
            bind_member_return(
                &EMPTY_SOURCE,
                &owner_generic,
                optional_of(any),
                &[Ty::String],
                &[],
                any,
            ),
            any
        );

        let owner_member = GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: Some(Ty::obj_args("demo/Owner", &[parameter])),
            params: Vec::new(),
            ret: parameter,
            return_policy: Default::default(),
        };
        assert_eq!(
            bind_member_return(
                &EMPTY_SOURCE,
                &owner_member,
                Ty::obj_args("demo/Owner", &[parameter]),
                &[],
                &[],
                any,
            ),
            parameter
        );
    }

    #[test]
    fn java_platform_wrapper_does_not_hide_method_type_parameter_binding() {
        let any = Ty::obj("kotlin/Any");
        let annotation = Ty::obj("java/lang/annotation/Annotation");
        let char_tag = Ty::obj("demo/CharTag");
        let formal = Ty::ty_param("A", annotation);
        let class_of = |ty| Ty::obj_args("java/lang/Class", &[ty]);
        let signature = GenericSig {
            formals: vec!["A".to_string()],
            formal_bounds: vec![vec![annotation]],
            receiver: None,
            params: vec![Ty::platform_nullable(class_of(formal))],
            ret: Ty::platform_nullable(formal),
            return_policy: GenericReturnPolicy::FlexibleReference,
        };

        assert_eq!(
            bind_member_return(
                &EMPTY_SOURCE,
                &signature,
                class_of(any),
                &[class_of(char_tag)],
                &[],
                Ty::platform_nullable(annotation),
            ),
            Ty::platform_nullable(char_tag),
        );
    }

    #[test]
    fn member_return_preserves_canonical_provider_types() {
        let any = Ty::obj("kotlin/Any");
        let kotlin_list = Ty::obj_args("kotlin/collections/List", &[Ty::Int]);
        let jvm_list = Ty::obj_args("java/util/List", &[Ty::obj("java/lang/Integer")]);
        let signature = GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: Vec::new(),
            ret: jvm_list,
            return_policy: Default::default(),
        };

        assert_eq!(
            bind_member_return(
                &EMPTY_SOURCE,
                &signature,
                Ty::obj("demo/Provider"),
                &[],
                &[],
                kotlin_list,
            ),
            kotlin_list
        );

        let erased_signature = GenericSig {
            ret: Ty::obj_args("java/util/List", &[any]),
            ..signature
        };
        assert_eq!(
            bind_member_return(
                &EMPTY_SOURCE,
                &erased_signature,
                Ty::obj("demo/Provider"),
                &[],
                &[],
                kotlin_list
            ),
            kotlin_list
        );
    }

    fn fake_library_type(supertypes: Vec<String>, constructors: Vec<LibraryMember>) -> LibraryType {
        LibraryType {
            is_kotlin: true,
            access: crate::libraries::ClassifierAccess::Public,
            source_file: None,
            stable_declaration: None,
            is_nested: false,
            outer_instance: None,
            kind: TypeKind::Class,
            inheritance: Default::default(),
            supertypes: supertypes.into(),
            supertype_templates: Vec::new(),
            constructors,
            hidden_member_properties: Default::default(),
            declared_callables: std::collections::HashMap::new(),
            declared_callable_order: Vec::new(),
            members: vec![],
            companion: vec![],
            constants: std::collections::HashMap::new(),
            sam_eligible: false,
            callable_signature: None,
            callable_signatures: Vec::new(),
            companion_object: None,
            value_underlying: None,
            value_underlying_property: None,
            alias_target: None,
            type_parameters: crate::types::TypeParameters::default(),
            own_type_parameter_count: 0,
            sealed_subclasses: crate::types::TypeNameList::new(),
            enum_entries: Vec::new(),
            enum_entries_accessor: None,
            named_parameter_lists: Vec::new(),
            retention: None,
            annotation_targets: None,
        }
    }

    struct SamHierarchySource {
        classifiers: std::collections::HashMap<TypeName, std::sync::Arc<LibraryType>>,
    }

    impl SymbolSource for SamHierarchySource {
        fn classifier(&self, internal: TypeName) -> Option<std::sync::Arc<LibraryType>> {
            self.classifiers.get(&internal).cloned()
        }
    }

    fn sam_classifier(
        formals: &[&str],
        supertypes: Vec<Ty>,
        members: Vec<LibraryMember>,
    ) -> LibraryType {
        let mut classifier = fake_library_type(Vec::new(), Vec::new());
        classifier.kind = TypeKind::Interface;
        classifier.sam_eligible = true;
        classifier.type_parameters = crate::types::TypeParameters::invariant(
            formals.iter().map(|formal| (*formal).to_string()).collect(),
            vec![Vec::new(); formals.len()],
        );
        classifier.own_type_parameter_count = formals.len();
        classifier.supertypes = supertypes
            .iter()
            .filter_map(|supertype| supertype.obj_internal())
            .collect::<Vec<_>>()
            .into();
        classifier.supertype_templates = supertypes;
        classifier.members = members;
        classifier
    }

    fn abstract_generic_member(name: &str, params: Vec<Ty>, ret: Ty) -> LibraryMember {
        let mut member = LibraryMember::new(name.to_string(), params.clone(), ret, String::new());
        member.generic_sig = Some(GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params,
            ret,
            return_policy: Default::default(),
        });
        member.set_is_abstract(true);
        member
    }

    #[test]
    fn sam_signature_uses_the_applied_inherited_declaration() {
        let bound = Ty::nullable(Ty::obj("kotlin/Any"));
        let input = Ty::ty_param("I", bound);
        let output = Ty::ty_param("O", bound);
        let parameter = Ty::ty_param("T", bound);
        let base = sam_classifier(
            &["I", "O"],
            Vec::new(),
            vec![abstract_generic_member("apply", vec![input], output)],
        );
        let operation = sam_classifier(
            &["T"],
            vec![Ty::obj_args("test/Base", &[parameter, parameter])],
            Vec::new(),
        );
        let source = SamHierarchySource {
            classifiers: [
                (
                    crate::types::type_name("test/Base"),
                    std::sync::Arc::new(base),
                ),
                (
                    crate::types::type_name("test/Operation"),
                    std::sync::Arc::new(operation),
                ),
            ]
            .into(),
        };

        let signature =
            semantic_sam_signature(&source, Ty::obj_args("test/Operation", &[Ty::String]))
                .expect("the inherited abstract declaration is the SAM");
        assert_eq!(
            signature.internal,
            crate::types::type_name("test/Operation")
        );
        assert_eq!(signature.method, "apply");
        assert_eq!(signature.params, [Ty::String]);
        assert_eq!(signature.ret, Ty::String);
    }

    #[test]
    fn sam_signature_rejects_two_distinct_abstract_declarations() {
        let left = sam_classifier(
            &[],
            Vec::new(),
            vec![abstract_generic_member("left", Vec::new(), Ty::Unit)],
        );
        let right = sam_classifier(
            &[],
            Vec::new(),
            vec![abstract_generic_member("right", Vec::new(), Ty::Unit)],
        );
        let both = sam_classifier(
            &[],
            vec![Ty::obj("test/Left"), Ty::obj("test/Right")],
            Vec::new(),
        );
        let source = SamHierarchySource {
            classifiers: [
                (
                    crate::types::type_name("test/Left"),
                    std::sync::Arc::new(left),
                ),
                (
                    crate::types::type_name("test/Right"),
                    std::sync::Arc::new(right),
                ),
                (
                    crate::types::type_name("test/Both"),
                    std::sync::Arc::new(both),
                ),
            ]
            .into(),
        };

        assert!(semantic_sam_signature(&source, Ty::obj("test/Both")).is_none());
    }

    #[test]
    fn sam_signature_merges_platform_flexible_override_slot() {
        let kotlin = sam_classifier(
            &[],
            Vec::new(),
            vec![abstract_generic_member("get", vec![Ty::Int], Ty::Int)],
        );
        let mut java = sam_classifier(
            &[],
            Vec::new(),
            vec![abstract_generic_member(
                "get",
                vec![Ty::platform_nullable(Ty::Int)],
                Ty::platform_nullable(Ty::Int),
            )],
        );
        java.is_kotlin = false;
        let mixed = sam_classifier(
            &[],
            vec![Ty::obj("test/KotlinGet"), Ty::obj("test/JavaGet")],
            vec![abstract_generic_member("get", vec![Ty::Int], Ty::Int)],
        );
        let source = SamHierarchySource {
            classifiers: [
                (
                    crate::types::type_name("test/KotlinGet"),
                    std::sync::Arc::new(kotlin),
                ),
                (
                    crate::types::type_name("test/JavaGet"),
                    std::sync::Arc::new(java),
                ),
                (
                    crate::types::type_name("test/MixedGet"),
                    std::sync::Arc::new(mixed),
                ),
            ]
            .into(),
        };

        let signature = semantic_sam_signature(&source, Ty::obj("test/MixedGet"))
            .expect("the direct Kotlin override occupies the inherited platform method slot");
        assert_eq!(signature.method, "get");
        assert_eq!(signature.params, [Ty::Int]);
        assert_eq!(signature.ret, Ty::Int);
    }

    #[test]
    fn sam_signature_keeps_an_explicit_abstract_object_method() {
        let some_fun = sam_classifier(
            &[],
            Vec::new(),
            vec![abstract_generic_member("toString", Vec::new(), Ty::String)],
        );
        let source = SamHierarchySource {
            classifiers: [(
                crate::types::type_name("test/SomeFun"),
                std::sync::Arc::new(some_fun),
            )]
            .into(),
        };

        let signature = semantic_sam_signature(&source, Ty::obj("test/SomeFun"))
            .expect("the directly redeclared abstract method is the SAM");
        assert_eq!(signature.method, "toString");
        assert!(signature.params.is_empty());
        assert_eq!(signature.ret, Ty::String);
    }

    #[test]
    fn java_sam_ignores_an_explicit_abstract_object_method() {
        let mut comparator = sam_classifier(
            &["T"],
            Vec::new(),
            vec![
                abstract_generic_member("equals", vec![Ty::obj("java/lang/Object")], Ty::Boolean),
                abstract_generic_member(
                    "compare",
                    vec![
                        Ty::ty_param("T", Ty::obj("kotlin/Any")),
                        Ty::ty_param("T", Ty::obj("kotlin/Any")),
                    ],
                    Ty::Int,
                ),
            ],
        );
        comparator.is_kotlin = false;
        let source = SamHierarchySource {
            classifiers: [(
                crate::types::type_name("test/Comparator"),
                std::sync::Arc::new(comparator),
            )]
            .into(),
        };

        let signature =
            semantic_sam_signature(&source, Ty::obj_args("test/Comparator", &[Ty::String]))
                .expect("Object-shaped Java declarations do not count against SAM eligibility");
        assert_eq!(signature.method, "compare");
        assert_eq!(signature.params, [Ty::String, Ty::String]);
        assert_eq!(signature.ret, Ty::Int);
    }

    #[test]
    fn generic_constraint_collection_projects_callable_references_through_sam() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let input = Ty::ty_param("I", any);
        let output = Ty::ty_param("O", any);
        let function = sam_classifier(
            &["I", "O"],
            Vec::new(),
            vec![abstract_generic_member("apply", vec![input], output)],
        );
        let source = SamHierarchySource {
            classifiers: [(
                crate::types::type_name("test/Function"),
                std::sync::Arc::new(function),
            )]
            .into(),
        };
        let t = Ty::ty_param("T", any);
        let u = Ty::ty_param("U", any);
        let signature = GenericSig {
            formals: vec!["T".to_string(), "U".to_string()],
            formal_bounds: vec![vec![any], vec![any]],
            receiver: None,
            params: vec![Ty::obj_args(
                "test/Function",
                &[Ty::in_projection(t), Ty::out_projection(u)],
            )],
            ret: Ty::obj_args("test/Result", &[t]),
            return_policy: GenericReturnPolicy::Exact,
        };
        let actual = Ty::fun(vec![Ty::String], Ty::Boolean);

        let constrained = infer_generic_call_constraints_from_symbols(
            &source,
            &signature,
            [(0, actual, false)],
            None,
        );
        let mut complete = constrained.bindings.clone();
        complete.extend(constrained.tightest_upper_bindings(&source));
        assert_eq!(complete.get("T"), Some(&Ty::String));
        assert_eq!(complete.get("U"), Some(&Ty::Boolean));

        let bindings = infer_generic_call_bindings_from_symbols(
            &source,
            &signature,
            [(0, actual, false)],
            None,
        );
        assert_eq!(bindings.get("T"), Some(&Ty::String));
        assert_eq!(bindings.get("U"), Some(&Ty::Boolean));
    }

    struct AppliedHierarchySource;

    impl SymbolSource for AppliedHierarchySource {
        fn symbols(
            &self,
            namespace: SymbolNamespace,
            name: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            let classifier_name = namespace.existing_classifier(name);
            let classifier =
                if classifier_name.is_some_and(|name| name.matches("fixtures/KSerializer")) {
                    let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));
                    let mut classifier = fake_library_type(
                        vec!["fixtures/DeserializationStrategy".to_string()],
                        Vec::new(),
                    );
                    classifier.type_parameters = crate::types::TypeParameters::invariant(
                        vec!["T".to_string()],
                        vec![Vec::new()],
                    );
                    classifier.supertype_templates = vec![Ty::obj_args(
                        "fixtures/DeserializationStrategy",
                        &[parameter],
                    )];
                    Some(classifier)
                } else if classifier_name
                    .is_some_and(|name| name.matches("fixtures/DeserializationStrategy"))
                {
                    let mut classifier = fake_library_type(Vec::new(), Vec::new());
                    classifier.type_parameters = crate::types::TypeParameters::invariant(
                        vec!["T".to_string()],
                        vec![Vec::new()],
                    );
                    Some(classifier)
                } else {
                    None
                };
            std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                classifier_name: classifier.as_ref().and(classifier_name),
                classifier: classifier.map(std::sync::Arc::new),
                callables: Callables::None,
                importable_declaration: false,
            })
        }
    }

    #[test]
    fn member_return_infers_through_an_applied_argument_supertype() {
        let any = Ty::obj("kotlin/Any");
        let foo = Ty::obj("fixtures/Foo");
        let parameter = Ty::ty_param("T", any);
        let signature = GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![any]],
            receiver: None,
            params: vec![Ty::obj_args(
                "fixtures/DeserializationStrategy",
                &[parameter],
            )],
            ret: parameter,
            return_policy: GenericReturnPolicy::Exact,
        };

        assert_eq!(
            bind_member_return(
                &AppliedHierarchySource,
                &signature,
                Ty::obj("fixtures/Format"),
                &[Ty::obj_args("fixtures/KSerializer", &[foo])],
                &[],
                any,
            ),
            foo,
        );
    }

    /// `fixtures/Reply<B>` is invariant, `fixtures/Producer<out B>` is covariant.
    struct VarianceSource;

    impl SymbolSource for VarianceSource {
        fn symbols(
            &self,
            namespace: SymbolNamespace,
            name: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            let classifier_name = namespace.existing_classifier(name);
            let variance = if classifier_name.is_some_and(|name| name.matches("fixtures/Reply")) {
                Some(crate::types::TypeVariance::Invariant)
            } else if classifier_name.is_some_and(|name| name.matches("fixtures/Producer")) {
                Some(crate::types::TypeVariance::Out)
            } else {
                None
            };
            let classifier = variance.map(|variance| {
                let mut classifier = fake_library_type(Vec::new(), Vec::new());
                classifier.type_parameters = crate::types::TypeParameters::new(
                    vec!["B".to_string()],
                    vec![Vec::new()],
                    vec![variance],
                );
                classifier
            });
            std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                classifier_name: classifier.as_ref().and(classifier_name),
                classifier: classifier.map(std::sync::Arc::new),
                callables: Callables::None,
                importable_declaration: false,
            })
        }
    }

    /// `fun <T> reply(body: T): Owner<T>`.
    fn expected_result_signature(owner: &str) -> GenericSig {
        let any = Ty::obj("kotlin/Any");
        let parameter = Ty::ty_param("T", any);
        GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![any]],
            receiver: None,
            params: vec![parameter],
            ret: Ty::obj_args(owner, &[parameter]),
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    fn widened(
        owner: &str,
        inferred: Ty,
        expected_argument: Ty,
        explicit_type_argument_count: usize,
    ) -> Option<Ty> {
        let signature = expected_result_signature(owner);
        let mut bindings = GSigBinds::new();
        bindings.insert("T".to_string(), inferred);
        let mut expected_bindings = GSigBinds::new();
        expected_bindings.insert("T".to_string(), expected_argument);
        widen_invariant_expected_bindings(
            &VarianceSource,
            &signature,
            explicit_type_argument_count,
            &mut bindings,
            &expected_bindings,
            Ty::obj_args(owner, &[expected_argument]),
            |actual, bound| bound == Ty::obj("kotlin/Any") || actual == bound,
        );
        bindings.get("T").copied()
    }

    #[test]
    fn an_invariant_expected_result_widens_the_argument_binding() {
        assert_eq!(
            widened("fixtures/Reply", Ty::String, Ty::obj("kotlin/Any"), 0),
            Some(Ty::obj("kotlin/Any")),
        );
    }

    #[test]
    fn a_covariant_expected_result_keeps_the_argument_binding() {
        assert_eq!(
            widened("fixtures/Producer", Ty::String, Ty::obj("kotlin/Any"), 0),
            Some(Ty::String),
        );
    }

    #[test]
    fn an_unsatisfiable_expected_result_keeps_the_argument_binding() {
        assert_eq!(
            widened("fixtures/Reply", Ty::Int, Ty::String, 0),
            Some(Ty::Int),
        );
    }

    #[test]
    fn a_projected_expected_result_keeps_the_argument_binding() {
        assert_eq!(
            widened(
                "fixtures/Reply",
                Ty::String,
                Ty::out_projection(Ty::obj("kotlin/Any")),
                0,
            ),
            Some(Ty::String),
        );
    }

    #[test]
    fn an_explicit_type_argument_is_never_widened() {
        assert_eq!(
            widened("fixtures/Reply", Ty::String, Ty::obj("kotlin/Any"), 1),
            Some(Ty::String),
        );
    }

    struct FakeSource {
        name: &'static str,
        receiver: Option<Ty>,
        info: FunctionInfo,
    }

    impl SymbolSource for FakeSource {
        fn symbols(
            &self,
            namespace: SymbolNamespace,
            name: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            let classifier_name = namespace.existing_classifier(name);
            let classifier =
                classifier_name.and_then(|internal| fake_classifier_record(self, internal));
            let callables =
                if namespace == SymbolNamespace::Package(TypeName::ROOT) && name == self.name {
                    crate::libraries::Callables::Functions(FunctionSet {
                        overloads: vec![self.info.clone()],
                    })
                } else {
                    crate::libraries::Callables::None
                };
            std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                classifier_name: classifier.as_ref().map(|classifier| {
                    classifier
                        .alias_target
                        .unwrap_or_else(|| classifier_name.expect("classifier identity"))
                }),
                classifier,
                callables,
                importable_declaration: false,
            })
        }
    }

    fn fake_classifier_record(
        source: &FakeSource,
        internal: TypeName,
    ) -> Option<std::sync::Arc<crate::libraries::LibraryType>> {
        if internal.matches("demo/Base")
            && source
                .receiver
                .and_then(Ty::kotlin_class_internal)
                .is_some_and(|receiver| receiver != internal)
        {
            return None;
        }
        let supertypes = if internal.matches("demo/Leaf") {
            vec!["demo/Mid".to_string()]
        } else if internal.matches("demo/Mid") {
            vec!["demo/Base".to_string()]
        } else if internal.matches("demo/Base")
            || internal.matches("kotlin/UInt")
            || internal.matches("demo/Box")
        {
            vec!["kotlin/Any".to_string()]
        } else {
            return None;
        };
        let mut ty = fake_library_type(supertypes, Vec::new());
        ty.value_underlying = internal.matches("kotlin/UInt").then_some(Ty::Int);
        if source.receiver.and_then(Ty::kotlin_class_internal) == Some(internal) {
            ty.declared_callables.insert(
                source.name.to_string(),
                Callables::Functions(FunctionSet {
                    overloads: vec![source.info.clone()],
                }),
            );
        }
        Some(std::sync::Arc::new(ty))
    }

    impl crate::libraries::SemanticPlatform for FakeSource {
        fn value_underlying(&self, ty: Ty) -> Option<Ty> {
            fake_classifier_record(self, ty.obj_internal()?).and_then(|t| t.value_underlying)
        }

        fn library_value_form(&self, ty: Ty) -> Ty {
            ty.obj_internal()
                .and_then(|n| crate::jvm::jvm_class_map::kotlin_builtin_to_jvm(&n.render()))
                .map(Ty::obj)
                .unwrap_or(ty)
        }
    }

    impl crate::runtime::TargetRuntime for FakeSource {}

    struct CountingSource {
        inner: FakeSource,
        counted_name: &'static str,
        queries: std::cell::Cell<usize>,
    }

    impl SymbolSource for CountingSource {
        fn symbols(
            &self,
            namespace: SymbolNamespace,
            name: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            let matches = namespace
                .existing_classifier(name)
                .is_some_and(|name| name.matches(self.counted_name))
                || (namespace == SymbolNamespace::Package(TypeName::ROOT)
                    && name == self.counted_name);
            if matches {
                self.queries.set(self.queries.get() + 1);
            }
            self.inner.symbols(namespace, name)
        }
    }

    impl crate::libraries::SemanticPlatform for CountingSource {
        fn value_underlying(&self, ty: Ty) -> Option<Ty> {
            self.inner.value_underlying(ty)
        }

        fn library_value_form(&self, ty: Ty) -> Ty {
            self.inner.library_value_form(ty)
        }
    }

    impl crate::runtime::TargetRuntime for CountingSource {}

    #[test]
    fn a_receiver_call_queries_its_imported_name_once() {
        let source = CountingSource {
            inner: FakeSource {
                name: "maybeSuffix",
                receiver: Some(Ty::String),
                info: extension_nullable_string_info(),
            },
            counted_name: "maybeSuffix",
            queries: std::cell::Cell::new(0),
        };
        let scope = [crate::types::type_name("")];

        let call = SymbolResolver::new_scoped(&source, &scope)
            .resolve_symbol(SymRecv::Value(Ty::String), "maybeSuffix", &[], &[])
            .and_then(Symbol::extension_call);

        assert!(call.is_some());
        assert_eq!(source.queries.get(), 1);
    }

    #[test]
    fn hierarchy_collection_queries_each_classifier_once() {
        let source = CountingSource {
            inner: FakeSource {
                name: "maybe",
                receiver: Some(Ty::obj("demo/Box")),
                info: member_nullable_string_info(),
            },
            counted_name: "demo/Box",
            queries: std::cell::Cell::new(0),
        };

        let functions = members_in_hierarchy(&source, Ty::obj("demo/Box"), "maybe")
            .into_parts()
            .0;

        assert_eq!(functions.overloads.len(), 1);
        assert_eq!(source.queries.get(), 1);
    }

    #[test]
    fn mapped_mutable_list_inherits_the_mutable_iterator_declaration() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let source = crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let receiver = Ty::obj_args("kotlin/collections/MutableList", &[Ty::Int]);
        let functions = members_in_hierarchy(&source, receiver, "iterator")
            .into_parts()
            .0;
        let nearest = functions
            .overloads
            .iter()
            .min_by_key(|function| function.receiver_rank)
            .expect("inherited iterator declaration");
        assert_eq!(
            nearest.callable.ret,
            Ty::obj_args("kotlin/collections/MutableIterator", &[Ty::Int]),
        );
    }

    #[test]
    fn a_receiver_call_collects_its_member_hierarchy_once() {
        let source = CountingSource {
            inner: FakeSource {
                name: "maybe",
                receiver: Some(Ty::obj("demo/Box")),
                info: member_nullable_string_info(),
            },
            counted_name: "demo/Box",
            queries: std::cell::Cell::new(0),
        };

        let call = SymbolResolver::new(&source)
            .resolve_symbol(SymRecv::Value(Ty::obj("demo/Box")), "maybe", &[], &[])
            .and_then(Symbol::call);

        assert!(call.is_some());
        assert_eq!(source.queries.get(), 1);
    }

    #[test]
    fn a_default_callable_uses_the_provider_target_identity() {
        let mut base = top_level_nullable_string_info();
        base.callable.default_realization = Some(Box::new(DefaultCallRealization {
            owner: crate::types::type_name("demo/Defaults"),
            name: "realized$default".to_string(),
            descriptor: "(ILjava/lang/Object;)Ljava/lang/String;".to_string(),
            declaration_owner: crate::types::type_name("demo/DefaultsBody"),
            real_params: vec![Ty::Int],
            mask_count: 1,
            ret: Ty::String,
            suspend: false,
        }));

        let realized = selected_default_callable(&base).expect("default realization");

        assert_eq!(realized.owner, crate::types::type_name("demo/Defaults"));
        assert_eq!(realized.name, "realized$default");
        assert_eq!(
            realized.descriptor,
            "(ILjava/lang/Object;)Ljava/lang/String;"
        );
        assert_eq!(realized.physical_params, vec![Ty::Int]);
        assert_eq!(realized.physical_ret, Ty::String);
        assert!(!realized.suspend);
        assert!(realized.default_call);
        let target = realized
            .default_realization
            .as_deref()
            .expect("attached default target");
        assert_eq!(target.owner, crate::types::type_name("demo/Defaults"));
        assert_eq!(target.name, "realized$default");
        assert_eq!(target.descriptor, "(ILjava/lang/Object;)Ljava/lang/String;");
        assert_eq!(
            target.declaration_owner,
            crate::types::type_name("demo/DefaultsBody")
        );
        assert_eq!(target.real_params, vec![Ty::Int]);
        assert_eq!(target.mask_count, 1);
        assert_eq!(target.ret, Ty::String);
        assert!(!target.suspend);
    }

    #[test]
    fn omitted_extension_vararg_realizes_as_an_empty_pack_without_a_default_bridge() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let parameter = Ty::ty_param("R", any);
        let receiver = Ty::obj_args("kotlin/reflect/KCallable", &[parameter]);
        let values = Ty::obj_args("kotlin/Array", &[any]);
        let mut callable = LibraryCallable::library(
            "kotlin/reflect/full/KCallables",
            "callSuspend",
            vec![receiver, values],
            parameter,
            Ty::obj("kotlin/Any"),
            "(Lkotlin/reflect/KCallable;[Ljava/lang/Object;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        );
        callable.suspend = true;
        let mut overload = FunctionInfo::plain(FnKind::Extension, Some(receiver), callable);
        overload.flags.suspend = true;
        overload.call_sig = CallSig::metadata_function(
            1,
            vec!["args".to_string()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some(0),
        );
        overload.generic_sig = Some(GenericSig {
            formals: vec!["R".to_string()],
            formal_bounds: vec![vec![any]],
            receiver: Some(receiver),
            params: vec![values],
            ret: parameter,
            return_policy: GenericReturnPolicy::Exact,
        });
        let source = FakeSource {
            name: "callSuspend",
            receiver: Some(receiver),
            info: overload.clone(),
        };
        let selected_receiver = Ty::obj_args("kotlin/reflect/KCallable", &[Ty::String]);

        let realized = SymbolResolver::new(&source)
            .build_extension_callable_for_slots(
                "callSuspend",
                selected_receiver,
                &[],
                &overload,
                &[None],
            )
            .expect("an omitted vararg is a direct call with an empty packed array");

        assert!(!realized.default_call);
        assert_eq!(realized.vararg_index, Some(0));
        assert_eq!(realized.vararg_elem, Some(any));
        assert_eq!(realized.ret, Ty::String);
    }

    #[test]
    fn constructor_selection_consumes_only_the_declaration_attached_realization() {
        let mut declaration =
            LibraryMember::new("<init>".into(), vec![Ty::Int], Ty::Unit, String::new());
        declaration.default_realization = Some(Box::new(DefaultCallRealization {
            owner: crate::types::type_name("demo/Category"),
            name: "<init>".to_string(),
            descriptor: "(ILplatform/Marker;)V".to_string(),
            declaration_owner: crate::types::type_name("demo/Category"),
            real_params: vec![Ty::Int],
            mask_count: 0,
            ret: Ty::Unit,
            suspend: false,
        }));
        let classifier = fake_library_type(Vec::new(), vec![declaration]);
        let source = FakeSource {
            name: "",
            receiver: None,
            info: top_level_nullable_string_info(),
        };

        assert!(matches!(
            select_constructor_call_from_type(
                &source,
                &source,
                crate::types::type_name("demo/Category"),
                &classifier,
                &[CallArgKind::Typed(Ty::Int)],
            ),
            Some(SelectedConstructorCall::Platform(_))
        ));
    }

    #[test]
    fn constructor_selection_infers_class_type_parameters_before_applicability() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let mut constructor = LibraryMember::new(
            "<init>".into(),
            vec![Ty::obj("kotlin/Any")],
            Ty::Unit,
            "(Ljava/lang/Object;)V".into(),
        );
        constructor.generic_sig = Some(GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: vec![parameter],
            ret: Ty::Unit,
            return_policy: Default::default(),
        });
        let mut classifier = fake_library_type(Vec::new(), vec![constructor]);
        classifier.type_parameters = crate::types::TypeParameters::invariant(
            vec!["T".to_string()],
            vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
        );
        let source = FakeSource {
            name: "",
            receiver: None,
            info: top_level_nullable_string_info(),
        };

        let selected = select_constructor_declaration_from_type(
            &source,
            &source,
            &classifier,
            &[CallArgKind::IntegerLiteral {
                ty: Ty::Int,
                value: 1,
            }],
        )
        .expect("the inferred constructor parameter must be applicable");

        assert_eq!(selected.params, vec![Ty::Int]);
        assert_eq!(selected.physical_params, vec![Ty::obj("kotlin/Any")]);
    }

    #[test]
    fn constructor_result_inference_combines_arguments_and_expected_type() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let first = Ty::ty_param("A", any);
        let mut constructor = LibraryMember::new(
            "<init>".into(),
            vec![Ty::obj("kotlin/Any")],
            Ty::Unit,
            "(Ljava/lang/Object;)V".into(),
        );
        constructor.generic_sig = Some(GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: vec![first],
            ret: Ty::Unit,
            return_policy: Default::default(),
        });
        let mut classifier = fake_library_type(Vec::new(), vec![constructor]);
        classifier.type_parameters = crate::types::TypeParameters::invariant(
            vec!["A".to_string(), "B".to_string()],
            vec![vec![any], vec![any]],
        );
        let owner = type_name("demo/Pair");

        assert_eq!(
            infer_constructor_type_args(
                &EMPTY_SOURCE,
                owner,
                &classifier,
                &[Ty::String],
                Some(Ty::obj_args_name(owner, &[Ty::String, Ty::Int])),
            ),
            Some(vec![Ty::String, Ty::Int])
        );
    }

    #[test]
    fn symbolic_expected_result_does_not_widen_concrete_constructor_evidence() {
        let owner = type_name("demo/Box");
        let formal = "T".to_string();
        let outer = Ty::ty_param("OuterT", Ty::nullable(Ty::obj("kotlin/Any")));
        let mut bindings = GSigBinds::from([(formal.clone(), Ty::Int)]);

        constrain_constructor_result(
            owner,
            std::slice::from_ref(&formal),
            Some(Ty::obj_args_name(owner, &[outer])),
            &mut bindings,
        );

        assert_eq!(bindings.get(&formal), Some(&Ty::Int));
    }

    #[test]
    fn generic_vararg_constructor_keeps_its_declaration_array_shape() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let mut constructor = LibraryMember::new(
            "<init>".into(),
            vec![Ty::array(Ty::obj("kotlin/Any"))],
            Ty::Unit,
            "([Ljava/lang/Object;)V".into(),
        );
        constructor.call_sig =
            CallSig::metadata_member(1, vec!["values".to_string()], vec![false], Some(0));
        constructor.generic_sig = Some(GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
            receiver: None,
            params: vec![Ty::array(parameter)],
            ret: Ty::Unit,
            return_policy: Default::default(),
        });
        let classifier = fake_library_type(Vec::new(), vec![constructor]);
        let source = FakeSource {
            name: "",
            receiver: None,
            info: top_level_nullable_string_info(),
        };

        let selected = select_constructor_declaration_from_type(
            &source,
            &source,
            &classifier,
            &[CallArgKind::Typed(Ty::array(Ty::String))],
        )
        .expect("generic vararg constructor");

        assert_eq!(selected.params, vec![Ty::array(Ty::String)]);
        assert_eq!(
            selected.physical_params,
            vec![Ty::array(Ty::obj("kotlin/Any"))]
        );
    }

    fn top_level_default_uint_info() -> FunctionInfo {
        let callable = LibraryCallable {
            external_identity: None,
            external_property_identity: None,
            owner: "kotlin/UIntKt".into(),
            name: "make$default".to_string(),
            reflection_name: None,
            compiler_intrinsic: None,
            inline_body_plan: None,
            plugin_expression: None,
            params: vec![Ty::Int],
            physical_params: vec![Ty::Int],
            ret: Ty::Int,
            physical_ret: Ty::Int,
            descriptor: "(I)I".to_string(),
            suspend: false,
            is_abstract: false,
            owner_is_interface: false,
            member_realization: crate::libraries::MemberRealization::Dispatch,
            inline: crate::libraries::InlineKind::None,
            default_call: true,
            vararg_elem: None,
            vararg_index: None,
            signature: None,
            origin: Origin::Library,
            source_receiver: None,
            declared_params: None,
            context_count: 0,
            contract: None,
            equality_bound: None,
            generic_sig: None,
            singleton_dispatch: None,
            default_realization: None,
            constructor_realization: None,
            declared_ret: None,
        };
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(false, Some(Ty::UInt)),
            call_sig: CallSig {
                required: 0,
                param_defaults: vec![true],
                ..Default::default()
            },
            ..FunctionInfo::plain(FnKind::TopLevel, None, callable)
        }
    }

    fn attach_default_target(base: &mut FunctionInfo, bridge: &FunctionInfo) {
        base.callable.default_realization = Some(Box::new(DefaultCallRealization {
            owner: bridge.callable.owner,
            name: bridge.callable.name.clone(),
            descriptor: bridge.callable.descriptor.clone(),
            declaration_owner: bridge.callable.owner,
            real_params: base.callable.physical_params.clone(),
            mask_count: 1,
            ret: bridge.callable.physical_ret,
            suspend: bridge.callable.suspend,
        }));
    }

    fn top_level_nullable_string_info() -> FunctionInfo {
        let callable = LibraryCallable::library(
            "kotlin/FooKt",
            "maybe",
            vec![],
            Ty::String,
            Ty::String,
            "()Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::TopLevel, None, callable)
        }
    }

    fn extension_nullable_string_info() -> FunctionInfo {
        let receiver = Ty::String;
        let callable = LibraryCallable::library(
            "kotlin/text/StringsKt",
            "maybeSuffix",
            vec![receiver],
            Ty::String,
            Ty::String,
            "(Ljava/lang/String;)Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
        }
    }

    #[test]
    fn nested_generic_conflicts_erase_only_the_later_sam_slot() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let generic_values = Ty::obj_args("fixture/Duo", &[parameter, parameter]);
        let generic_sink = Ty::obj_args("fixture/Sink", &[parameter]);
        let mut member = LibraryMember::new(
            "use".to_string(),
            vec![Ty::obj("fixture/Duo"), Ty::obj("fixture/Sink")],
            Ty::Unit,
            "(Lfixture/Duo;Lfixture/Sink;)V".to_string(),
        );
        member.generic_sig = Some(GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![Vec::new()],
            receiver: None,
            params: vec![generic_values, generic_sink],
            ret: Ty::Unit,
            return_policy: Default::default(),
        });

        let params = specialized_member_params(
            &member,
            &[
                CallArgKind::Typed(Ty::obj_args(
                    "fixture/Duo",
                    &[Ty::String, Ty::obj("kotlin/Any")],
                )),
                CallArgKind::LambdaLiteral(Ty::Error),
            ],
            &[],
        );

        assert_eq!(params[0], Ty::obj("fixture/Duo"));
        assert_eq!(
            params[1],
            Ty::obj_args("fixture/Sink", &[Ty::obj("kotlin/Any")])
        );
    }

    #[test]
    fn receiver_mro_walks_supertypes() {
        let src = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let mro = ReceiverMro::new(&src, Ty::obj("demo/Leaf"));
        assert_eq!(mro.rank(&src, Ty::obj("demo/Base")), Some(2));
        assert_eq!(mro.rank(&src, Ty::obj("demo/Leaf")), Some(0));
        assert_eq!(mro.rank(&src, Ty::obj("demo/Unrelated")), None);
    }

    #[test]
    fn receiver_mro_treats_receiver_function_notation_as_the_same_function_type() {
        let src = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let ordinary = Ty::fun(vec![Ty::Int], Ty::String);
        let receiver = Ty::fun_with_shape(vec![Ty::Int], Ty::String, 0, true, false);

        assert_eq!(
            ReceiverMro::new(&src, ordinary).rank(&src, receiver),
            Some(0)
        );
        assert_eq!(
            ReceiverMro::new(&src, receiver).rank(&src, ordinary),
            Some(0)
        );
    }

    #[test]
    fn receiver_mro_binds_nested_callee_formal_to_caller_formal() {
        let src = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let callee = Ty::ty_param("T@flattenMerge", Ty::nullable(Ty::obj("kotlin/Any")));
        let caller = Ty::ty_param("R@flatMapMerge", Ty::nullable(Ty::obj("kotlin/Any")));
        let declared = Ty::obj_args("fixture/Flow", &[Ty::obj_args("fixture/Flow", &[callee])]);
        let actual = Ty::obj_args("fixture/Flow", &[Ty::obj_args("fixture/Flow", &[caller])]);

        assert_eq!(ReceiverMro::new(&src, actual).rank(&src, declared), Some(0));
    }

    #[test]
    fn core_walks_exact_provider_members_once_and_assigns_rank() {
        let mut info = top_level_nullable_string_info();
        info.kind = FnKind::Member;
        info.receiver = Some(Ty::obj("demo/Base"));
        info.receiver_rank = 0;
        let src = FakeSource {
            name: "greet",
            receiver: Some(Ty::obj("demo/Base")),
            info,
        };

        assert!(
            declared_member_callables(&src, Ty::obj("demo/Leaf"), "greet")
                .into_parts()
                .0
                .overloads
                .is_empty()
        );

        let inherited = members_in_hierarchy(&src, Ty::obj("demo/Leaf"), "greet")
            .into_parts()
            .0;
        assert_eq!(inherited.overloads.len(), 1);
        assert_eq!(inherited.overloads[0].receiver_rank, 2);
    }

    #[test]
    fn core_inherits_operator_convention_into_nearest_override() {
        let mut leaf = top_level_nullable_string_info();
        leaf.kind = FnKind::Member;
        leaf.receiver = Some(Ty::obj("demo/Leaf"));
        leaf.callable.params = vec![Ty::obj("demo/Leaf")];

        let mut base = leaf.clone();
        base.receiver = Some(Ty::obj("demo/Base"));
        base.flags.operator = true;

        let leaf_source = FakeSource {
            name: "compareTo",
            receiver: Some(Ty::obj("demo/Leaf")),
            info: leaf,
        };
        let base_source = FakeSource {
            name: "compareTo",
            receiver: Some(Ty::obj("demo/Base")),
            info: base,
        };
        let source = crate::symbol_source::CompositeSource::new(vec![&leaf_source, &base_source]);

        let functions = members_in_hierarchy(&source, Ty::obj("demo/Leaf"), "compareTo")
            .into_parts()
            .0;
        let nearest = functions
            .overloads
            .iter()
            .find(|function| function.receiver_rank == 0)
            .expect("exact override");
        assert!(nearest.flags.operator);
    }

    #[test]
    fn callable_reference_selects_nearest_override_before_comparing_signatures() {
        let mut leaf = top_level_nullable_string_info();
        leaf.kind = FnKind::Member;
        leaf.receiver = Some(Ty::obj("demo/Leaf"));
        leaf.callable.params.clear();
        leaf.callable.ret = Ty::String;
        leaf.ret = crate::libraries::ReturnInfo::default();

        let mut base = leaf.clone();
        base.receiver = Some(Ty::obj("demo/Base"));
        base.callable.ret = Ty::platform_nullable(Ty::String);

        let leaf_source = FakeSource {
            name: "render",
            receiver: Some(Ty::obj("demo/Leaf")),
            info: leaf,
        };
        let base_source = FakeSource {
            name: "render",
            receiver: Some(Ty::obj("demo/Base")),
            info: base,
        };
        let source = crate::symbol_source::CompositeSource::new(vec![&leaf_source, &base_source]);
        let callables = members_in_hierarchy(&source, Ty::obj("demo/Leaf"), "render");
        let selected =
            select_instance_reference_from_functions(Ty::obj("demo/Leaf"), callables.functions())
                .expect("nearest override");

        assert_eq!(selected.ret, Ty::String);
    }

    #[test]
    fn receiver_mro_respects_concrete_extension_nullability() {
        let src = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let nullable = ReceiverMro::new(&src, Ty::nullable(Ty::String));
        assert_eq!(nullable.rank(&src, Ty::String), None);
        assert_eq!(nullable.rank(&src, Ty::nullable(Ty::String)), Some(0));

        let non_null = ReceiverMro::new(&src, Ty::String);
        assert_eq!(non_null.rank(&src, Ty::String), Some(0));
        assert_eq!(non_null.rank(&src, Ty::nullable(Ty::String)), Some(0));

        let unbounded_generic = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        assert_eq!(nullable.rank(&src, unbounded_generic), Some(u32::MAX - 1));
        let explicitly_non_null_generic = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        assert_eq!(nullable.rank(&src, explicitly_non_null_generic), None);
    }

    #[test]
    fn integer_literal_overloads_require_a_unique_most_specific_parameter_list() {
        let source = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let args = [
            CallArgKind::integer_literal(Ty::Int, 1),
            CallArgKind::integer_literal(Ty::Int, 1),
        ];
        let selected = integer_literal_overload(
            [
                (vec![Ty::Int, Ty::Long], "narrow"),
                (vec![Ty::Long, Ty::Long], "wide"),
            ]
            .into_iter(),
            &args,
            |_, param, arg| arg_fits(param, &arg.ty()),
            |_, left, right, arg| parameter_at_least_as_specific(&source, left, right, arg),
            |_, _| false,
        );
        assert!(matches!(selected, CandidateSelection::Selected("narrow")));

        let ambiguous = integer_literal_overload(
            [
                (vec![Ty::Int, Ty::Long], "left"),
                (vec![Ty::Long, Ty::Int], "right"),
            ]
            .into_iter(),
            &args,
            |_, param, arg| arg_fits(param, &arg.ty()),
            |_, left, right, arg| parameter_at_least_as_specific(&source, left, right, arg),
            |_, _| false,
        );
        assert!(matches!(ambiguous, CandidateSelection::Ambiguous));

        let select_integral_width = |value| {
            integer_literal_overload(
                [(vec![Ty::Byte], "byte"), (vec![Ty::Long], "long")].into_iter(),
                &[CallArgKind::integer_literal(Ty::Int, value)],
                |_, param, arg| arg_fits(param, &arg.ty()),
                |_, left, right, arg| parameter_at_least_as_specific(&source, left, right, arg),
                |_, _| false,
            )
        };
        assert!(matches!(
            select_integral_width(1),
            CandidateSelection::Ambiguous
        ));
        assert!(matches!(
            select_integral_width(1_000),
            CandidateSelection::Selected("long")
        ));
    }

    #[test]
    fn concrete_lambda_return_rejects_same_arity_fallback() {
        let source = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        assert!(fun_return_compatible(
            &source,
            Ty::fun(vec![Ty::String], Ty::Unit),
            Ty::fun(vec![Ty::String], Ty::String),
            true,
        ));
        assert!(!fun_return_compatible(
            &source,
            Ty::fun(vec![Ty::String], Ty::Unit),
            Ty::fun(vec![Ty::String], Ty::String),
            false,
        ));
        assert!(fun_return_compatible(&source, Ty::Unit, Ty::Nothing, false,));

        let int_transform = Ty::fun(vec![Ty::Int], Ty::Int);
        let string_transform = Ty::fun(vec![Ty::Int], Ty::String);
        let candidate = |name: &str, transform: Ty| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "fixture/Calls",
                    name,
                    vec![transform],
                    Ty::Unit,
                    Ty::Unit,
                    "(Lkotlin/jvm/functions/Function1;)V",
                ),
            )
        };
        let int_candidate = candidate("chooseInt", int_transform);
        let string_candidate = candidate("chooseString", string_transform);
        let candidates = [
            (&int_candidate, vec![int_transform]),
            (&string_candidate, vec![string_transform]),
        ];
        let argument = Ty::fun(vec![Ty::Error], Ty::String);

        let selected = best_by_args(
            &source,
            &source,
            &candidates,
            &[CallArgKind::LambdaLiteral(argument)],
        );

        let CandidateSelection::Selected(selected) = selected else {
            panic!("concrete lambda return should select one overload");
        };
        assert_eq!(selected.callable.name, "chooseString");
    }

    #[test]
    fn companion_overloads_use_the_composite_source_hierarchy() {
        let source = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let member =
            |params| LibraryMember::new("make".to_string(), params, Ty::Unit, String::new());

        let broad = member(vec![Ty::obj("demo/Base")]);
        let specific = member(vec![Ty::obj("demo/Mid")]);
        let specific_duplicate = member(vec![Ty::obj("demo/Mid")]);
        let selected = best_callable_member_overload(
            &source,
            &source,
            [&broad, &specific, &specific_duplicate].into_iter(),
            "make",
            &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
            &[],
        )
        .expect("the most specific source supertype should be selected");
        assert_eq!(selected.params, vec![Ty::obj("demo/Mid")]);

        let left = member(vec![Ty::obj("demo/Mid"), Ty::obj("demo/Base")]);
        let right = member(vec![Ty::obj("demo/Base"), Ty::obj("demo/Mid")]);
        assert!(best_callable_member_overload(
            &source,
            &source,
            [&left, &right].into_iter(),
            "make",
            &[
                CallArgKind::Typed(Ty::obj("demo/Leaf")),
                CallArgKind::Typed(Ty::obj("demo/Leaf")),
            ],
            &[],
        )
        .is_none());

        let aliases = unique_most_specific(
            [
                (vec![Ty::obj("kotlin/Any")], "any"),
                (vec![Ty::String], "string"),
            ],
            |_, left, right| resolution_subtype(&source, left, right),
        );
        assert!(matches!(aliases, CandidateSelection::Selected("string")));
    }

    #[test]
    fn companion_default_and_vararg_shapes_accept_source_subtypes() {
        let source = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let member =
            |params| LibraryMember::new("make".to_string(), params, Ty::Unit, String::new());

        let with_trailing_default = |mut member: LibraryMember| {
            member.call_sig.required = 1;
            member.call_sig.param_names = vec!["value".into(), "label".into()];
            member.call_sig.param_defaults = vec![false, true];
            member
        };
        let default_broad = with_trailing_default(member(vec![Ty::obj("demo/Base"), Ty::String]));
        let default_specific = with_trailing_default(member(vec![Ty::obj("demo/Mid"), Ty::String]));
        let selected = best_callable_member_overload(
            &source,
            &source,
            [&default_broad, &default_specific].into_iter(),
            "make",
            &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
            &[],
        )
        .expect("the defaulted source-supertype overload should resolve");
        assert_eq!(selected.params[0], Ty::obj("demo/Mid"));

        let as_vararg = |mut member: LibraryMember| {
            member.call_sig.vararg = true;
            member.call_sig.vararg_index = Some(0);
            member.call_sig.param_names = vec!["values".into()];
            member.call_sig.param_defaults = vec![false];
            member
        };
        let vararg_broad = as_vararg(member(vec![Ty::array(Ty::obj("demo/Base"))]));
        let vararg_specific = as_vararg(member(vec![Ty::array(Ty::obj("demo/Mid"))]));
        let selected = best_callable_member_overload(
            &source,
            &source,
            [&vararg_broad, &vararg_specific].into_iter(),
            "make",
            &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
            &[],
        )
        .expect("the vararg source-supertype overload should resolve");
        assert_eq!(selected.params[0], Ty::array(Ty::obj("demo/Mid")));

        let ordinary = member(vec![Ty::obj("demo/Base"), Ty::String]);
        assert!(
            best_callable_member_overload(
                &source,
                &source,
                [&ordinary].into_iter(),
                "make",
                &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
                &[],
            )
            .is_none(),
            "an unmarked trailing parameter is required"
        );
    }

    #[test]
    fn type_name_identity_normalizes_nested_source_spellings() {
        let dotted = crate::types::type_name("lib/Flex.Inner.Deep");
        let dollar = crate::types::type_name("lib/Flex$Inner$Deep");
        let other_pkg = crate::types::type_name("other/Flex$Inner$Deep");
        let other_tail = crate::types::type_name("lib/Flex$Inner$Other");
        assert_eq!(dotted, dollar);
        assert_ne!(dotted, other_pkg);
        assert_ne!(dotted, other_tail);
    }

    fn member_nullable_string_info() -> FunctionInfo {
        let receiver = Ty::obj("demo/Box");
        let callable = LibraryCallable::library(
            "demo/Box",
            "maybe",
            vec![],
            Ty::String,
            Ty::String,
            "()Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
        }
    }

    fn member_metadata_class_info() -> FunctionInfo {
        let receiver = Ty::obj("demo/Box");
        let callable = LibraryCallable::library(
            "demo/Box",
            "names",
            vec![],
            Ty::obj("kotlin/Any"),
            Ty::obj("kotlin/Any"),
            "()Ljava/lang/Object;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(
                false,
                Some(Ty::obj_args("kotlin/collections/List", &[Ty::String])),
            ),
            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
        }
    }

    #[test]
    fn top_level_default_callable_preserves_metadata_return_type() {
        struct DefaultSource {
            bridge: FunctionInfo,
            base: FunctionInfo,
        }

        impl SymbolSource for DefaultSource {
            fn symbols(
                &self,
                namespace: SymbolNamespace,
                name: &str,
            ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
                let overloads = match (namespace, name) {
                    (SymbolNamespace::Package(TypeName::ROOT), "make") => vec![self.base.clone()],
                    (SymbolNamespace::Package(TypeName::ROOT), "make$default") => {
                        vec![self.bridge.clone()]
                    }
                    _ => Vec::new(),
                };
                std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                    classifier_name: None,
                    classifier: None,
                    callables: crate::libraries::Callables::Functions(FunctionSet { overloads }),
                    importable_declaration: false,
                })
            }
        }

        impl crate::libraries::SemanticPlatform for DefaultSource {}

        let bridge = top_level_default_uint_info();
        let mut base = bridge.clone();
        base.callable.name = "make".to_string();
        base.callable.default_call = false;
        attach_default_target(&mut base, &bridge);
        let source = DefaultSource { bridge, base };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_symbol(SymRecv::TopLevel, "make", &[], &[])
            .and_then(Symbol::top_level_call)
            .expect("default callable should resolve");
        assert!(call.default_call);
        assert_eq!(call.ret, Ty::UInt);
        assert_eq!(call.physical_ret, Ty::Int);
    }

    #[test]
    fn top_level_default_callable_preserves_specialized_source_parameters() {
        struct DefaultSource {
            bridge: FunctionInfo,
            base: FunctionInfo,
        }

        impl SymbolSource for DefaultSource {
            fn symbols(
                &self,
                namespace: SymbolNamespace,
                name: &str,
            ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
                let overloads = match (namespace, name) {
                    (SymbolNamespace::Package(TypeName::ROOT), "same") => vec![self.base.clone()],
                    (SymbolNamespace::Package(TypeName::ROOT), "same$default") => {
                        vec![self.bridge.clone()]
                    }
                    _ => Vec::new(),
                };
                std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                    classifier_name: None,
                    classifier: None,
                    callables: crate::libraries::Callables::Functions(FunctionSet { overloads }),
                    importable_declaration: false,
                })
            }
        }

        impl crate::libraries::SemanticPlatform for DefaultSource {}

        let any = Ty::obj("kotlin/Any");
        let nullable_any = Ty::nullable(any);
        let parameter = Ty::ty_param("T", nullable_any);
        let mut callable = LibraryCallable::library(
            "demo/AssertionsKt",
            "same$default",
            vec![any, any, Ty::nullable(Ty::String)],
            Ty::Unit,
            Ty::Unit,
            "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/String;)V",
        );
        callable.default_call = true;
        let mut bridge = FunctionInfo::plain(FnKind::TopLevel, None, callable);
        bridge.call_sig.required = 2;
        bridge.call_sig.param_defaults = vec![false, false, true];
        let mut base = bridge.clone();
        base.callable.name = "same".to_string();
        base.callable.default_call = false;
        attach_default_target(&mut base, &bridge);
        base.generic_sig = Some(GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![nullable_any]],
            receiver: None,
            params: vec![parameter, parameter, Ty::nullable(Ty::String)],
            ret: Ty::Unit,
            return_policy: GenericReturnPolicy::Exact,
        });
        let source = DefaultSource { bridge, base };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_symbol(
                SymRecv::TopLevel,
                "same",
                &[Ty::nullable(Ty::String), Ty::String],
                &[],
            )
            .and_then(Symbol::top_level_call)
            .expect("generic default callable should resolve");

        assert!(call.default_call);
        assert_eq!(call.params[0], Ty::nullable(Ty::String));
        assert_eq!(call.params[1], Ty::nullable(Ty::String));
    }

    #[test]
    fn top_level_callable_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybe",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_symbol(SymRecv::TopLevel, "maybe", &[], &[])
            .and_then(Symbol::top_level_call)
            .expect("nullable callable should resolve");
        assert_eq!(call.ret, Ty::nullable(Ty::String));
        assert_eq!(call.physical_ret, Ty::String);
    }

    #[test]
    fn top_level_callable_uses_source_subtypes_for_fixed_and_vararg_shapes() {
        let broad = LibraryCallable::library(
            "demo/FunctionsKt",
            "accept",
            vec![Ty::obj("demo/Base")],
            Ty::Unit,
            Ty::Unit,
            "(Ldemo/Base;)V",
        );
        let specific = LibraryCallable::library(
            "demo/FunctionsKt",
            "accept",
            vec![Ty::obj("demo/Mid")],
            Ty::Unit,
            Ty::Unit,
            "(Ldemo/Mid;)V",
        );
        let source = FakeSource {
            name: "accept",
            receiver: None,
            info: FunctionInfo::plain(FnKind::TopLevel, None, broad.clone()),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .pick_top_level(
                "accept",
                &FunctionSet {
                    overloads: vec![
                        FunctionInfo::plain(FnKind::TopLevel, None, broad),
                        FunctionInfo::plain(FnKind::TopLevel, None, specific),
                    ],
                },
                &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
                &[],
                None,
            )
            .expect("source subtype should fit top-level parameter");
        assert_eq!(call.params, vec![Ty::obj("demo/Mid")]);

        let alias = |param, descriptor| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "demo/FunctionsKt",
                    "accept",
                    vec![param],
                    Ty::Unit,
                    Ty::Unit,
                    descriptor,
                ),
            )
        };
        let call = resolver
            .pick_top_level(
                "accept",
                &FunctionSet {
                    overloads: vec![
                        alias(Ty::obj("kotlin/Any"), "(Ljava/lang/Object;)V"),
                        alias(Ty::String, "(Ljava/lang/String;)V"),
                    ],
                },
                // Providers normalize platform aliases before publishing semantic declarations or
                // argument types. Core selection therefore compares the one Kotlin identity.
                &[CallArgKind::Typed(Ty::String)],
                &[],
                None,
            )
            .expect("the normalized semantic string type should remain applicable");
        assert_eq!(call.params, vec![Ty::String]);

        let vararg = |element, descriptor| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "demo/FunctionsKt",
                    "accept",
                    vec![Ty::array(element)],
                    Ty::Unit,
                    Ty::Unit,
                    descriptor,
                ),
            )
        };
        let call = resolver
            .pick_top_level(
                "accept",
                &FunctionSet {
                    overloads: vec![
                        vararg(Ty::obj("demo/Base"), "([Ldemo/Base;)V"),
                        vararg(Ty::obj("demo/Mid"), "([Ldemo/Mid;)V"),
                    ],
                },
                &[
                    CallArgKind::Typed(Ty::obj("demo/Leaf")),
                    CallArgKind::Typed(Ty::obj("demo/Leaf")),
                ],
                &[],
                None,
            )
            .expect("source subtypes should fit top-level varargs");
        assert_eq!(call.params, vec![Ty::array(Ty::obj("demo/Mid"))]);

        let literal = |second, descriptor| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "demo/FunctionsKt",
                    "accept",
                    vec![Ty::Long, second],
                    Ty::Unit,
                    Ty::Unit,
                    descriptor,
                ),
            )
        };
        let call = resolver
            .pick_top_level(
                "accept",
                &FunctionSet {
                    overloads: vec![
                        literal(Ty::obj("demo/Base"), "(JLdemo/Base;)V"),
                        literal(Ty::obj("demo/Mid"), "(JLdemo/Mid;)V"),
                    ],
                },
                &[
                    CallArgKind::integer_literal(Ty::Int, 1),
                    CallArgKind::Typed(Ty::obj("demo/Leaf")),
                ],
                &[],
                None,
            )
            .expect("integer adaptation should retain source-type specificity");
        assert_eq!(call.params, vec![Ty::Long, Ty::obj("demo/Mid")]);
    }

    #[test]
    fn ambiguous_source_subtype_overloads_do_not_fall_back_to_vararg() {
        let info = |params, descriptor| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "demo/FunctionsKt",
                    "accept",
                    params,
                    Ty::Unit,
                    Ty::Unit,
                    descriptor,
                ),
            )
        };
        let source = FakeSource {
            name: "accept",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let selected = resolver.pick_top_level(
            "accept",
            &FunctionSet {
                overloads: vec![
                    info(
                        vec![Ty::obj("demo/Mid"), Ty::obj("demo/Base")],
                        "(Ldemo/Mid;Ldemo/Base;)V",
                    ),
                    info(
                        vec![Ty::obj("demo/Base"), Ty::obj("demo/Mid")],
                        "(Ldemo/Base;Ldemo/Mid;)V",
                    ),
                    info(
                        vec![Ty::array(Ty::obj("kotlin/Any"))],
                        "([Ljava/lang/Object;)V",
                    ),
                ],
            },
            &[
                CallArgKind::Typed(Ty::obj("demo/Leaf")),
                CallArgKind::Typed(Ty::obj("demo/Leaf")),
            ],
            &[],
            None,
        );
        assert!(selected.is_none());
    }

    #[test]
    fn extension_callable_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybeSuffix",
            receiver: Some(Ty::String),
            info: extension_nullable_string_info(),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_symbol(SymRecv::Value(Ty::String), "maybeSuffix", &[], &[])
            .and_then(Symbol::extension_call)
            .expect("nullable extension callable should resolve");
        assert_eq!(call.ret, Ty::nullable(Ty::String));
        assert_eq!(call.physical_ret, Ty::String);
    }

    #[test]
    fn instance_member_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybe",
            receiver: Some(Ty::obj("demo/Box")),
            info: member_nullable_string_info(),
        };
        let resolved = SymbolResolver::new(&source)
            .resolve_symbol(SymRecv::Value(Ty::obj("demo/Box")), "maybe", &[], &[])
            .and_then(Symbol::call)
            .expect("nullable member should resolve");
        assert_eq!(resolved.ret, Ty::nullable(Ty::String));
        assert_eq!(resolved.member.physical_ret, Ty::String);
    }

    #[test]
    fn instance_member_preserves_metadata_return_class() {
        let source = FakeSource {
            name: "names",
            receiver: Some(Ty::obj("demo/Box")),
            info: member_metadata_class_info(),
        };
        let resolved = SymbolResolver::new(&source)
            .resolve_symbol(SymRecv::Value(Ty::obj("demo/Box")), "names", &[], &[])
            .and_then(Symbol::call)
            .expect("member with metadata return class should resolve");
        assert_eq!(
            resolved.ret,
            Ty::obj_args("kotlin/collections/List", &[Ty::String])
        );
        assert_eq!(resolved.member.physical_ret, Ty::obj("kotlin/Any"));
    }
}

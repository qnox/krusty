//! Applied classifier hierarchy and receiver projection.

use super::{ty_subst_applied_arguments, ty_subst_keep_unbound, unify_ty_from_symbols, GSigBinds};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// The classifier whose declarations form an expression type's member scope. `Nothing` has no
/// instances of its own, but Kotlin still exposes the ordinary `Any` members on a bottom-typed
/// expression. This projection is only for dispatch-member lookup and applicability; extension
/// receivers continue to use the expression's actual type.
pub(crate) fn member_scope_receiver(receiver: Ty) -> Ty {
    match receiver {
        Ty::Nothing => Ty::obj_name(crate::types::wk::any()),
        Ty::TyParam(_, bound) => member_scope_receiver(*bound),
        Ty::Byte => Ty::obj("kotlin/Byte"),
        Ty::Short => Ty::obj("kotlin/Short"),
        Ty::Int => Ty::obj("kotlin/Int"),
        Ty::Long => Ty::obj("kotlin/Long"),
        Ty::Float => Ty::obj("kotlin/Float"),
        Ty::Double => Ty::obj("kotlin/Double"),
        Ty::Boolean => Ty::obj("kotlin/Boolean"),
        Ty::Char => Ty::obj("kotlin/Char"),
        Ty::UByte => Ty::obj("kotlin/UByte"),
        Ty::UShort => Ty::obj("kotlin/UShort"),
        Ty::UInt => Ty::obj("kotlin/UInt"),
        Ty::ULong => Ty::obj("kotlin/ULong"),
        Ty::String => Ty::obj("kotlin/String"),
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

pub(super) fn direct_supertypes_from_classifier(
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
            // `ty_subst_applied_arguments`, not `ty_subst_keep_unbound`: `ty`'s arguments were
            // already validated against this classifier's bounds when the type was FORMED, so
            // re-narrowing them here only loses information. It lost nullability in particular —
            // a Java class's type parameters carry a non-null upper bound (a Java type variable
            // has no nullability), so projecting `HashMap<String, Any?>` onto its `Map<K, V>`
            // template produced `Map<String, Any>` and every member reached through the supertype
            // then rejected a nullable argument.
            .map(|supertype| ty_subst_applied_arguments(*supertype, &bindings))
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

pub(super) fn receiver_hierarchy(source: &dyn SymbolSource, receiver: Ty) -> Vec<(Ty, u32)> {
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
pub(super) fn classifier_type_parameter_bounds(
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

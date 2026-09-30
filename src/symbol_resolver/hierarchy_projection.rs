//! Applied classifier hierarchy and receiver projection.

use std::cell::{Cell, RefCell};

use super::{ty_subst_keep_unbound, unify_ty_from_symbols, GSigBinds};
use crate::name_tree::FxHashMap;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

thread_local! {
    /// Owned projection lists. These are hierarchy results, not `Ty::Obj` argument slices, so they
    /// stay in this map and are freed when the compilation drops it.
    static PROJECTION_CACHE: RefCell<Option<FxHashMap<Ty, Box<[Ty]>>>> = RefCell::new(None);
    static PROJECTION_CACHE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Remembers applied supertypes for the enclosing compilation. The map is empty until a
/// compilation enters it, and it dies with that compilation, so a later source set cannot reuse a
/// classifier that happened to share a name. Each stored list is owned by the map.
pub(crate) struct SupertypeProjectionCache;

impl SupertypeProjectionCache {
    pub(crate) fn enter() -> Self {
        PROJECTION_CACHE_DEPTH.with(|depth| {
            if depth.get() == 0 {
                PROJECTION_CACHE.with(|cache| *cache.borrow_mut() = Some(FxHashMap::default()));
            }
            depth.set(depth.get().saturating_add(1));
        });
        Self
    }
}

impl Drop for SupertypeProjectionCache {
    fn drop(&mut self) {
        PROJECTION_CACHE_DEPTH.with(|depth| {
            let next = depth.get().saturating_sub(1);
            depth.set(next);
            if next == 0 {
                PROJECTION_CACHE.with(|cache| *cache.borrow_mut() = None);
            }
        });
    }
}

fn projection_cache_get(ty: Ty) -> Option<Vec<Ty>> {
    PROJECTION_CACHE.with(|cache| {
        cache
            .borrow()
            .as_ref()?
            .get(&ty)
            .map(|applied| applied.to_vec())
    })
}

fn projection_cache_insert(ty: Ty, applied: &[Ty]) {
    PROJECTION_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let Some(cache) = cache.as_mut() else {
            return;
        };
        cache.insert(ty, applied.to_vec().into_boxed_slice());
    });
}

#[cfg(test)]
fn stored_projection_ptr(ty: Ty) -> Option<*const Ty> {
    PROJECTION_CACHE.with(|cache| {
        cache
            .borrow()
            .as_ref()?
            .get(&ty)
            .map(|applied| applied.as_ptr())
    })
}

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
        ClassifierAccess::PackagePrivate => inheritor.namespace() == internal.namespace(),
        ClassifierAccess::Private => false,
    };
    accessible.then_some(classifier)
}

/// Direct applied supertypes derived from one classifier record. Providers publish symbolic templates;
/// core owns substitution and every transitive traversal.
pub(crate) fn direct_supertypes(source: &dyn SymbolSource, ty: Ty) -> Vec<Ty> {
    // A function type is the function classifier of its arity and kind: `() -> R` is
    // `kotlin.Function0<R>`, `(P) -> R` is `Function1<P, R>`, and `suspend () -> R` is
    // `kotlin.coroutines.SuspendFunction0<R>`. Every `FunctionN` / `SuspendFunctionN` extends the
    // arity-independent `kotlin.Function<R>`. The normalized `FunctionN` declaration records that
    // second edge; keeping it there makes a missing classifier record a frontend error instead of
    // silently bypassing the provider with a parallel hierarchy path.
    let key = ty.non_null();
    if matches!(key, Ty::Fun(_)) {
        if let Some(cached) = projection_cache_get(key) {
            return cached;
        }
        let applied = vec![crate::libraries::function_classifiers::supertype_classifier(key)];
        projection_cache_insert(key, &applied);
        return applied;
    }
    if let Some(cached) = projection_cache_get(ty) {
        return cached;
    }
    let Some(internal) = ty.kotlin_class_internal() else {
        return Vec::new();
    };
    let Some(classifier) = source.classifier(internal) else {
        return Vec::new();
    };
    let applied = direct_supertypes_from_classifier(&classifier, ty);
    projection_cache_insert(ty, &applied);
    applied
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
        let parents = with_implicit_any(owner, direct_supertypes(source, applied));
        pending.extend(
            parents
                .into_iter()
                .filter_map(|parent| Some((parent.kotlin_class_internal()?, parent, depth + 1))),
        );
    }
    hierarchy
}

/// A classifier's direct supertypes including Kotlin's implicit root: one that declares no
/// supertype still inherits `kotlin.Any`'s members, and an override of `toString`, `equals` or
/// `hashCode` overrides that declaration.
pub(crate) fn with_implicit_any(owner: TypeName, mut parents: Vec<Ty>) -> Vec<Ty> {
    let any = crate::types::wk::any();
    if parents.is_empty() && owner != any {
        parents.push(Ty::obj_name(any));
    }
    parents
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
    if classifier.supertype_templates.is_empty() {
        return classifier.supertypes.iter_ids().map(Ty::obj_name).collect();
    }
    // Formals are a handful of declaration names. Scanning them keeps the applied argument and
    // leaves an unbound lexical variable alone, without cloning those names into a map on every
    // hierarchy step.
    let formals = classifier.type_params();
    let arguments = ty.type_args();
    let any = Ty::obj_name(crate::types::wk::any());
    let applied = classifier
        .supertype_templates
        .iter()
        // A local/anonymous classifier's supertype may mention type variables owned by its
        // enclosing declaration. Apply only this classifier's arguments; erasing every other
        // symbolic variable loses the lexical type (`object : Converter<Box<T>, T>` became
        // `Converter<Box<Any>, Any>` during the hierarchy walk).
        // Applied arguments were already validated against this classifier's bounds when the type
        // was FORMED, so re-narrowing them here only loses information. It lost nullability in
        // particular — a Java class's type parameters carry a non-null upper bound (a Java type
        // variable has no nullability), so projecting `HashMap<String, Any?>` onto its `Map<K, V>`
        // template produced `Map<String, Any>` and every member reached through the supertype then
        // rejected a nullable argument.
        .map(|supertype| {
            crate::types::ty_subst_applied_lookup(*supertype, |name| {
                formals
                    .iter()
                    .position(|formal| formal == name)
                    .map(|index| arguments.get(index).copied().unwrap_or(any))
            })
        })
        .collect::<Vec<_>>();
    crate::trace_compiler!(
        "supertype",
        "direct supertypes ty={ty:?} formals={:?} args={:?} templates={:?} applied={applied:?}",
        classifier.type_params,
        arguments,
        classifier.supertype_templates,
    );
    applied
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assignable::{is_assignable, TyCtx};
    use crate::libraries::{function_classifiers, Callables, LibraryType, ResolvedSymbols};
    use crate::symbol_resolver::SourceOracle;
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::TypeParameters;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;

    #[test]
    fn a_classifier_without_templates_projects_its_supertype_names() {
        let mut classifier = LibraryType::declaration_header();
        classifier.supertypes = vec!["kotlin/Any".to_string()].into();
        classifier.type_parameters =
            TypeParameters::invariant(vec!["T".to_string()], vec![vec![Ty::obj("kotlin/Any")]]);
        let applied = Ty::obj_args("sample/Box", &[Ty::String]);

        assert_eq!(
            direct_supertypes_from_classifier(&classifier, applied),
            vec![Ty::obj("kotlin/Any")]
        );
    }

    #[test]
    fn an_applied_nullable_argument_survives_supertype_projection() {
        let any = Ty::obj("kotlin/Any");
        let mut classifier = LibraryType::declaration_header();
        classifier.type_parameters = TypeParameters::invariant(
            vec!["K".to_string(), "V".to_string()],
            vec![vec![any], vec![any]],
        );
        classifier.supertype_templates = vec![Ty::obj_args(
            "java/util/Map",
            &[Ty::ty_param("K", any), Ty::ty_param("V", any)],
        )];
        let applied = Ty::obj_args("java/util/HashMap", &[Ty::String, Ty::nullable(any)]);

        assert_eq!(
            direct_supertypes_from_classifier(&classifier, applied),
            vec![Ty::obj_args(
                "java/util/Map",
                &[Ty::String, Ty::nullable(any)]
            )]
        );
    }

    #[test]
    fn a_missing_type_argument_projects_as_any() {
        let any = Ty::obj("kotlin/Any");
        let mut classifier = LibraryType::declaration_header();
        classifier.type_parameters =
            TypeParameters::invariant(vec!["T".to_string()], vec![vec![any]]);
        classifier.supertype_templates = vec![Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::ty_param("T", any)],
        )];

        assert_eq!(
            direct_supertypes_from_classifier(&classifier, Ty::obj("sample/Raw")),
            vec![Ty::obj_args("kotlin/collections/List", &[any])]
        );
    }

    struct OneClassifier {
        queries: Cell<usize>,
        classifier: Arc<LibraryType>,
    }

    impl SymbolSource for OneClassifier {
        fn symbols(&self, _namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
            self.queries.set(self.queries.get() + 1);
            if name != "CacheProbe" {
                return Rc::new(ResolvedSymbols::default());
            }
            let mut record = ResolvedSymbols::default();
            record.classifier = Some(Arc::clone(&self.classifier));
            Rc::new(record)
        }
    }

    fn probe(parent: &str) -> (OneClassifier, Ty) {
        let mut classifier = LibraryType::declaration_header();
        classifier.supertypes = vec![parent.to_string()].into();
        (
            OneClassifier {
                queries: Cell::new(0),
                classifier: Arc::new(classifier),
            },
            Ty::obj("sample/CacheProbe"),
        )
    }

    #[test]
    fn a_compilation_reuses_one_applied_supertype_projection() {
        let (source, ty) = probe("kotlin/Any");
        let _cache = SupertypeProjectionCache::enter();
        assert_eq!(direct_supertypes(&source, ty), vec![Ty::obj("kotlin/Any")]);
        assert_eq!(source.queries.get(), 1);
        assert_eq!(direct_supertypes(&source, ty), vec![Ty::obj("kotlin/Any")]);
        assert_eq!(source.queries.get(), 1);
    }

    #[test]
    fn a_later_compilation_does_not_reuse_the_previous_projection() {
        let (first, ty) = probe("kotlin/Any");
        {
            let _cache = SupertypeProjectionCache::enter();
            assert_eq!(direct_supertypes(&first, ty), vec![Ty::obj("kotlin/Any")]);
        }
        let (second, ty) = probe("kotlin/Number");
        let _cache = SupertypeProjectionCache::enter();
        assert_eq!(
            direct_supertypes(&second, ty),
            vec![Ty::obj("kotlin/Number")]
        );
        assert_eq!(second.queries.get(), 1);
    }

    #[test]
    fn a_cached_projection_is_owned_by_the_compilation() {
        let (source, ty) = probe("kotlin/Any");
        {
            let _cache = SupertypeProjectionCache::enter();
            let applied = direct_supertypes(&source, ty);
            let stored = stored_projection_ptr(ty).expect("projection stored");
            let interned = crate::types::intern_tys(&applied);
            assert_eq!(applied.as_slice(), interned);
            assert_ne!(
                stored,
                interned.as_ptr(),
                "a hierarchy result must not be the global type-argument slice"
            );
        };
        assert!(stored_projection_ptr(ty).is_none());
    }

    #[test]
    fn an_absent_classifier_is_looked_up_again() {
        let (source, _) = probe("kotlin/Any");
        let absent = Ty::obj("sample/Absent");
        let _cache = SupertypeProjectionCache::enter();
        assert!(direct_supertypes(&source, absent).is_empty());
        assert_eq!(source.queries.get(), 1);
        assert!(direct_supertypes(&source, absent).is_empty());
        assert_eq!(source.queries.get(), 2);
    }

    struct FunctionClassifiers;

    impl SymbolSource for FunctionClassifiers {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
            let identity = namespace.existing_classifier(name);
            let classifier = identity.and_then(|identity| {
                function_classifiers::classifier(identity)
                    .map(|function| function_classifiers::synthetic(function))
            });
            std::rc::Rc::new(ResolvedSymbols {
                builtin_classifier: false,
                classifier_name: classifier.as_ref().and(identity),
                classifier,
                callables: Callables::None,
                importable_declaration: false,
            })
        }
    }

    fn admits(actual: Ty, expected: Ty) -> bool {
        is_assignable(
            &TyCtx::new(),
            &SourceOracle(&FunctionClassifiers),
            actual,
            expected,
        )
    }

    #[test]
    fn function_type_is_its_function_classifier_and_function() {
        let value = Ty::fun(Vec::new(), Ty::String);
        let supertypes = direct_supertypes(&FunctionClassifiers, value);
        assert_eq!(
            supertypes,
            vec![Ty::obj_args("kotlin/Function0", &[Ty::String])]
        );
        let extension = Ty::fun_with_shape(vec![Ty::String, Ty::Int], Ty::Boolean, 0, true, false);
        assert_eq!(
            direct_supertypes(&FunctionClassifiers, extension)[0],
            Ty::obj_args("kotlin/Function2", &[Ty::String, Ty::Int, Ty::Boolean])
        );
        let suspended = Ty::fun_suspend(Vec::new(), Ty::String);
        assert_eq!(
            direct_supertypes(&FunctionClassifiers, suspended)[0],
            Ty::obj_args("kotlin/coroutines/SuspendFunction0", &[Ty::String])
        );
    }

    #[test]
    fn function_value_is_assignable_to_its_function_classifier() {
        let value = Ty::fun(Vec::new(), Ty::String);
        assert!(admits(
            value,
            Ty::obj_args("kotlin/Function0", &[Ty::String])
        ));
        assert!(admits(
            value,
            Ty::obj_args("kotlin/Function0", &[Ty::obj("kotlin/Any")])
        ));
        assert!(admits(
            value,
            Ty::obj_args(
                "kotlin/Function0",
                &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))]
            )
        ));
        assert!(!admits(value, Ty::obj_args("kotlin/Function0", &[Ty::Int])));
        assert!(!admits(
            Ty::fun_suspend(Vec::new(), Ty::String),
            Ty::obj_args("kotlin/Function0", &[Ty::String])
        ));
        assert!(admits(
            Ty::fun_suspend(Vec::new(), Ty::String),
            Ty::obj_args("kotlin/coroutines/SuspendFunction0", &[Ty::String])
        ));
        assert!(admits(
            value,
            Ty::obj_args("kotlin/Function", &[Ty::String])
        ));
    }
}

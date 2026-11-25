//! Applied classifier hierarchy and receiver projection.

use std::cell::{Cell, RefCell};

use super::{ty_subst_keep_unbound, unify_ty_from_symbols, GSigBinds};
use crate::fx_hash::HashMap as FxHashMap;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind, TypeName};

pub(crate) use crate::libraries::classifier_bindings;

thread_local! {
    /// Owned projection lists. These are hierarchy results, not `Ty::Obj` argument slices, so they
    /// stay in this map and are freed when the compilation drops it.
    static PROJECTION_CACHE: RefCell<Option<FxHashMap<Ty, Box<[Ty]>>>> = const { RefCell::new(None) };
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
    match receiver.kind() {
        TyKind::Nothing => Ty::obj_name(crate::types::wk::any()),
        TyKind::TyParam(_, bound) | TyKind::Var(_, bound) => member_scope_receiver(bound),
        TyKind::BYTE => Ty::obj("kotlin/Byte"),
        TyKind::SHORT => Ty::obj("kotlin/Short"),
        TyKind::INT => Ty::obj("kotlin/Int"),
        TyKind::LONG => Ty::obj("kotlin/Long"),
        TyKind::FLOAT => Ty::obj("kotlin/Float"),
        TyKind::DOUBLE => Ty::obj("kotlin/Double"),
        TyKind::BOOLEAN => Ty::obj("kotlin/Boolean"),
        TyKind::CHAR => Ty::obj("kotlin/Char"),
        TyKind::UBYTE => Ty::obj("kotlin/UByte"),
        TyKind::USHORT => Ty::obj("kotlin/UShort"),
        TyKind::UINT => Ty::obj("kotlin/UInt"),
        TyKind::ULONG => Ty::obj("kotlin/ULong"),
        TyKind::STRING => Ty::obj("kotlin/String"),
        _ => receiver,
    }
}

/// Whether a non-null semantic type owns a classifier/function/intersection member scope. This is
/// the shared structural gate for member inventory; it does not perform lookup or reinterpret a
/// failed receiver through another origin. `Unit` is the semantic spelling of the `kotlin.Unit`
/// object type, so it owns that classifier's member scope like any other object type.
pub(crate) fn supports_member_lookup(receiver: Ty) -> bool {
    matches!(
        receiver.non_null().kind(),
        TyKind::Unit
            | TyKind::STRING
            | TyKind::Obj(..)
            | TyKind::TyParam(..)
            | TyKind::Var(..)
            | TyKind::Fun(..)
            | TyKind::Intersection(_)
    )
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
    if key.is_fun() {
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
    let applied = projected_supertypes(source, &classifier, ty);
    projection_cache_insert(ty, &applied);
    applied
}

/// The direct supertypes of `ty` as an extension receiver binds its formals from them. A Java
/// class writes its own type variables as flexible supertype arguments (`class Sub<T> extends
/// J<T>` is `J<T!>`), so binding `T` to a not-null type keeps the interval `String!` instead of
/// fixing the argument to `String`. The provider records the occurrence as the template variable's
/// platform bound, and only a Java declaration's template carries it: a Kotlin declaration, a mapped
/// builtin included, states its own nullability, and so does a pure implementation's pure supertype.
/// Only this consumer reads the interval: the shared
/// hierarchy keeps the fixed binding, so the platform wrapper does not spread through supertypes and
/// source-declared generic operations.
pub(crate) fn direct_supertypes_with_flexible_arguments(
    source: &dyn SymbolSource,
    ty: Ty,
) -> Vec<Ty> {
    let applied = direct_supertypes(source, ty);
    let Some(classifier) = ty
        .kotlin_class_internal()
        .and_then(|internal| source.classifier(internal))
    else {
        return applied;
    };
    if classifier.supertype_templates.len() != applied.len() {
        return applied;
    }
    classifier
        .supertype_templates
        .iter()
        .zip(applied)
        .map(|(template, applied)| {
            // A pure implementation states its arguments rigidly through its pure supertype.
            if classifier.purely_implemented.is_some_and(|kind| {
                applied
                    .obj_internal()
                    .and_then(|supertype| source.classifier(supertype))
                    .and_then(|supertype| supertype.mapped_collection)
                    .is_some_and(|mapped| mapped.kind == kind)
            }) {
                applied
            } else {
                retain_flexible_arguments(*template, applied)
            }
        })
        .collect()
}

/// A supertype argument that is a Java type variable (a declared bound that is platform-nullable)
/// and was bound to a not-null reference type stays flexible. A nullable binding, or one the
/// substitution left a type variable, already says everything it can.
fn retain_flexible_arguments(template: Ty, applied: Ty) -> Ty {
    let (TyKind::Obj(name, template_arguments), TyKind::Obj(applied_name, applied_arguments)) =
        (template.kind(), applied.kind())
    else {
        return applied;
    };
    if name != applied_name || template_arguments.len() != applied_arguments.len() {
        return applied;
    }
    let arguments = template_arguments
        .iter()
        .zip(applied_arguments.iter())
        .map(|(declared, bound)| {
            let flexible_variable = matches!(
                declared.kind(),
                TyKind::TyParam(_, declared_bound) if declared_bound.is_platform_nullable()
            );
            if flexible_variable
                && !bound.is_nullable()
                && !bound.is_platform_nullable()
                && bound.is_reference()
                && !bound.is_ty_param()
            {
                Ty::platform_nullable(*bound)
            } else {
                *bound
            }
        })
        .collect::<Vec<_>>();
    Ty::obj_args_name(name, &arguments)
}

/// Applied semantic hierarchy in breadth-first order. Providers expose only direct declarations;
/// core owns the transitive walk and generic substitution for every declaration origin.
pub(crate) fn applied_hierarchy(source: &dyn SymbolSource, root: Ty) -> Vec<(TypeName, Ty, usize)> {
    crate::assignable::applied_hierarchy_through(root, |_, applied| {
        direct_supertypes(source, applied)
    })
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
    let mut bindings = GSigBinds::default();
    unify_ty_from_symbols(source, applied_supertype, supertype, &mut bindings);
    let arguments = symbolic_args
        .iter()
        .map(|argument| ty_subst_keep_unbound(*argument, &bindings))
        .collect::<Vec<_>>();
    if arguments.iter().zip(&classifier.type_params).all(
        |(argument, formal)| !matches!(argument.kind(), TyKind::TyParam(identity, _) | TyKind::Var(identity, _) if identity == formal),
    ) {
        Ty::obj_args_name(subtype_name, &arguments)
    } else {
        subtype
    }
}

/// The direct supertypes of `ty` as `classifier` declares them, applied to `ty`'s arguments. The
/// supertypes of a Java raw type are raw: `Child extends Parent<C>` read raw is `Parent` raw over
/// `C`'s erasure, and a supertype argument that is not one of `Child`'s formals erases `Parent`
/// to its own raw application.
pub(super) fn projected_supertypes(
    source: &dyn SymbolSource,
    classifier: &crate::libraries::LibraryType,
    ty: Ty,
) -> Vec<Ty> {
    crate::assignable::classifier_supertypes(classifier, ty, &|owner| {
        source.classifier(owner).map(|shape| {
            (
                shape.type_params().clone(),
                shape.type_param_bounds().clone(),
            )
        })
    })
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
    let mut seen = crate::fx_hash::HashSet::default();
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

/// Components of an intersection receiver. Member and supertype walks union these into the
/// ordinary candidate list; the intersection itself has no classifier identity.
pub(crate) fn intersection_components(ty: Ty) -> Option<&'static [Ty]> {
    match ty.non_null().kind() {
        TyKind::Intersection(parts) => Some(parts.as_slice()),
        _ => None,
    }
}

/// `actual` seen as its supertype whose classifier is `owner`, with that supertype's arguments. A
/// function type has no classifier of its own: it reaches `owner` through its `FunctionN`
/// classifier, so `() -> Unit` is `Function<Unit>`.
pub(super) fn supertype_with_classifier(
    source: &dyn SymbolSource,
    actual: Ty,
    owner: TypeName,
) -> Option<Ty> {
    let roots = match actual.kind() {
        TyKind::Obj(found, _) if found == owner => return Some(actual),
        TyKind::Obj(_, _) => vec![actual],
        TyKind::Fun(_) => direct_supertypes(source, actual),
        _ => return None,
    };
    roots
        .into_iter()
        .flat_map(|root| receiver_hierarchy(source, root))
        .map(|(ty, _)| ty)
        .find(|ty| ty.obj_internal() == Some(owner))
}

impl super::SymbolResolver<'_> {
    /// Whether kotlinc's assignment plugin turns an assignment to a `val` of type `ty` into a call
    /// of `assign`: the class of `ty`, or one of its supertypes, carries one of the compilation's
    /// assignment annotations.
    pub(crate) fn supports_assignment_overloading(&self, ty: Ty) -> bool {
        let annotations = &self.src.annotation_conventions().assignment;
        !annotations.is_empty()
            && receiver_hierarchy(&self.src, ty.non_null())
                .into_iter()
                .filter_map(|(applied, _)| self.src.classifier(applied.obj_internal()?))
                .any(|classifier| {
                    classifier
                        .annotations
                        .iter()
                        .any(|annotation| annotations.contains(&annotation.annotation))
                })
    }
}

pub(super) fn receiver_hierarchy(source: &dyn SymbolSource, receiver: Ty) -> Vec<(Ty, u32)> {
    receiver_hierarchy_through(receiver, &|ty| direct_supertypes(source, ty))
}

/// The supertype of `actual` that names `owner`, read with Java's flexible type-variable arguments
/// retained, for inference that binds a declaration's variables from it.
pub(super) fn flexible_supertype_with_classifier(
    source: &dyn SymbolSource,
    actual: Ty,
    owner: TypeName,
) -> Option<Ty> {
    receiver_hierarchy_through(actual, &|ty| {
        direct_supertypes_with_flexible_arguments(source, ty)
    })
    .into_iter()
    .map(|(ty, _)| ty)
    .find(|ty| ty.obj_internal() == Some(owner))
}

fn receiver_hierarchy_through(receiver: Ty, parents: &dyn Fn(Ty) -> Vec<Ty>) -> Vec<(Ty, u32)> {
    let mut queue = std::collections::VecDeque::from([(receiver, 0)]);
    let mut seen = crate::fx_hash::HashSet::default();
    let mut hierarchy = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        if let Some(parts) = intersection_components(current) {
            queue.extend(parts.iter().copied().map(|part| (part, depth)));
            continue;
        }
        let Some(internal) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(internal) {
            continue;
        }
        hierarchy.push((current, depth));
        queue.extend(
            parents(current)
                .into_iter()
                .map(|supertype| (supertype, depth + 1)),
        );
    }
    hierarchy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assignable::{is_assignable, TyCtx};
    use crate::libraries::{function_classifiers, Callables, LibraryType, ResolvedSymbols};
    use crate::symbol_resolver::SourceOracle;
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::{type_name, TypeParameters};
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
            crate::assignable::classifier_supertypes(&classifier, applied, &|_| None),
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
            crate::assignable::classifier_supertypes(&classifier, applied, &|_| None),
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
            crate::assignable::classifier_supertypes(&classifier, Ty::obj("sample/Raw"), &|_| None,),
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
            Rc::new(ResolvedSymbols {
                classifier_name: Some(crate::types::type_name("sample/CacheProbe")),
                classifier: Some(Arc::clone(&self.classifier)),
                ..ResolvedSymbols::default()
            })
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
                if identity == type_name("sample/IndirectCallable") {
                    let mut declaration = LibraryType::declaration_header();
                    declaration.supertypes = vec![type_name("sample/DeclaredCallable")].into();
                    return Some(Arc::new(declaration));
                }
                if identity == type_name("sample/DeclaredCallable") {
                    let mut declaration = LibraryType::declaration_header();
                    declaration.supertypes = vec![type_name("kotlin/Function1")].into();
                    declaration.supertype_templates =
                        vec![Ty::obj_args("kotlin/Function1", &[Ty::Int, Ty::Int])];
                    return Some(Arc::new(declaration));
                }
                if identity == type_name("sample/Sam") {
                    let mut declaration = LibraryType::declaration_header();
                    declaration.callable_signature = Some(Ty::fun(vec![Ty::Int], Ty::Int));
                    return Some(Arc::new(declaration));
                }
                function_classifiers::classifier(identity).map(function_classifiers::synthetic)
            });
            std::rc::Rc::new(ResolvedSymbols {
                builtin_classifier: false,
                classifier_name: classifier.as_ref().and(identity),
                classifier_declaration: classifier
                    .as_ref()
                    .and(identity)
                    .map(crate::libraries::ClassifierDeclaration::Ordinary),
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

    #[test]
    fn nominal_function_values_reach_their_declared_function_type() {
        assert!(admits(
            Ty::obj_args("kotlin/Function1", &[Ty::String, Ty::Int]),
            Ty::fun(vec![Ty::String], Ty::Int),
        ));
        assert!(admits(
            Ty::obj_args("kotlin/reflect/KFunction1", &[Ty::Int, Ty::String]),
            Ty::fun(vec![Ty::Int], Ty::String),
        ));
        assert!(admits(
            Ty::obj_args("kotlin/reflect/KFunction0", &[Ty::String]),
            Ty::fun(Vec::new(), Ty::String),
        ));
        assert!(admits(
            Ty::obj("sample/IndirectCallable"),
            Ty::fun(vec![Ty::Int], Ty::Int),
        ));
    }

    #[test]
    fn nominal_function_subtyping_retains_declared_parameter_and_result_variance() {
        let any = Ty::obj("kotlin/Any");
        assert!(admits(
            Ty::obj_args("kotlin/reflect/KFunction1", &[any, Ty::Int]),
            Ty::fun(vec![Ty::String], Ty::Int),
        ));
        assert!(!admits(
            Ty::obj_args("kotlin/reflect/KFunction1", &[Ty::String, Ty::Int]),
            Ty::fun(vec![any], Ty::Int),
        ));
        assert!(admits(
            Ty::obj_args("kotlin/reflect/KFunction1", &[Ty::Int, Ty::String]),
            Ty::fun(vec![Ty::Int], any),
        ));
        assert!(!admits(
            Ty::obj_args("kotlin/reflect/KFunction1", &[Ty::Int, any]),
            Ty::fun(vec![Ty::Int], Ty::String),
        ));
    }

    #[test]
    fn nominal_function_subtyping_preserves_arity_nullability_and_suspend_kind() {
        let reference = Ty::obj_args("kotlin/reflect/KFunction1", &[Ty::Int, Ty::String]);
        let function = Ty::fun(vec![Ty::Int], Ty::String);
        assert!(!admits(reference, Ty::fun(Vec::new(), Ty::String)));
        assert!(!admits(Ty::nullable(reference), function));
        assert!(admits(Ty::nullable(reference), Ty::nullable(function)));
        assert!(!admits(
            reference,
            Ty::fun_suspend(vec![Ty::Int], Ty::String)
        ));
        let suspended = Ty::obj_args("kotlin/reflect/KSuspendFunction1", &[Ty::Int, Ty::String]);
        assert!(!admits(suspended, function));
        assert!(admits(
            suspended,
            Ty::fun_suspend(vec![Ty::Int], Ty::String),
        ));
    }

    #[test]
    fn nominal_function_subtyping_requires_declared_function_inheritance() {
        let function = Ty::fun(vec![Ty::Int], Ty::Int);
        assert!(!admits(Ty::obj("sample/Sam"), function));
        assert!(!admits(
            Ty::obj_args("sample/Function1", &[Ty::Int, Ty::Int]),
            function,
        ));
    }
}

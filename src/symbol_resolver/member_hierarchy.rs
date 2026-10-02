//! Declared and inherited member lookup over applied classifier hierarchies.

mod bound_inner_constructors;

pub(crate) use bound_inner_constructors::bound_inner_constructor_candidates;

use super::{
    classifier_bindings, direct_supertypes_from_classifier, resolution_subtype,
    specialize_call_sig, specialize_callable, specialize_member_type, ty_subst_keep_unbound,
    TypePosition,
};
use crate::libraries::{Callables, FnKind, FunctionInfo, FunctionSet, PropKind, PropertySet};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{Ty, TypeName};

/// Whether a declaration participates in a subtype's member family. Accessibility is checked at
/// the use site; private declarations are different because they are not inherited at all.
pub(crate) fn member_is_inheritable(visibility: crate::types::Visibility) -> bool {
    visibility != crate::types::Visibility::Private
}

/// Whether an overriding declaration's value-parameter types match one inherited declaration.
/// Both sides are compared after replacing their declaration-owned formal identities with the
/// same canonical coordinates. The subtype-equivalence check preserves flexible/provider types
/// without weakening the invariant position of override inputs.
pub(crate) fn override_parameter_types_match(
    source: &dyn SymbolSource,
    inherited: &[Ty],
    inherited_formals: &[String],
    implementation: &[Ty],
    implementation_formals: &[String],
) -> bool {
    inherited.len() == implementation.len()
        && inherited
            .iter()
            .copied()
            .zip(implementation.iter().copied())
            .all(|(inherited, implementation)| {
                let inherited = crate::types::ty_canonicalize_params(inherited, inherited_formals);
                let implementation =
                    crate::types::ty_canonicalize_params(implementation, implementation_formals);
                inherited == implementation
                    || resolution_subtype(source, inherited, implementation)
                        && resolution_subtype(source, implementation, inherited)
            })
}

pub(crate) struct OverrideInputShape<'a> {
    pub params: &'a [Ty],
    pub receiver: Option<Ty>,
    pub formals: &'a [String],
    /// Declared upper bounds, parallel to `formals`; an empty slot is Kotlin's implicit `Any?`.
    pub formal_bounds: &'a [Vec<Ty>],
    pub context_count: usize,
    pub suspend: bool,
}

/// Compare the complete invariant input shape of an override edge. Context and suspension are
/// declaration semantics just like value and extension-receiver inputs; keeping them in this one
/// predicate prevents transient body-local selection and the frozen override graph from accepting
/// different edges.
pub(crate) fn override_input_shapes_match(
    source: &dyn SymbolSource,
    inherited: OverrideInputShape<'_>,
    implementation: OverrideInputShape<'_>,
) -> bool {
    inherited.formals.len() == implementation.formals.len()
        && inherited.context_count == implementation.context_count
        && inherited.suspend == implementation.suspend
        && override_formal_bounds_match(source, &inherited, &implementation)
        && override_parameter_types_match(
            source,
            inherited.params,
            inherited.formals,
            implementation.params,
            implementation.formals,
        )
        && match (inherited.receiver, implementation.receiver) {
            (None, None) => true,
            (Some(inherited_receiver), Some(implementation_receiver)) => {
                override_parameter_types_match(
                    source,
                    &[inherited_receiver],
                    inherited.formals,
                    &[implementation_receiver],
                    implementation.formals,
                )
            }
            (None, Some(_)) | (Some(_), None) => false,
        }
}

/// A declaration overrides only one whose type parameters carry the same upper bounds, compared
/// after alpha-renaming both sides' formals: `fun <S : B> foo(s: S)` does not override
/// `fun <S : A> foo(s: S)`, although both take one type-parameter value.
fn override_formal_bounds_match(
    source: &dyn SymbolSource,
    inherited: &OverrideInputShape<'_>,
    implementation: &OverrideInputShape<'_>,
) -> bool {
    let bounds = |shape: &OverrideInputShape<'_>, ordinal: usize| -> Vec<Ty> {
        match shape.formal_bounds.get(ordinal).map(Vec::as_slice) {
            None | Some([]) => vec![Ty::nullable(Ty::obj("kotlin/Any"))],
            Some(declared) => declared
                .iter()
                .map(|bound| crate::types::ty_canonicalize_params(*bound, shape.formals))
                .collect(),
        }
    };
    let equivalent = |left: Ty, right: Ty| {
        left == right
            || resolution_subtype(source, left, right) && resolution_subtype(source, right, left)
    };
    (0..inherited.formals.len()).all(|ordinal| {
        let inherited = bounds(inherited, ordinal);
        let implementation = bounds(implementation, ordinal);
        inherited
            .iter()
            .all(|left| implementation.iter().any(|right| equivalent(*left, *right)))
            && implementation
                .iter()
                .all(|right| inherited.iter().any(|left| equivalent(*left, *right)))
    })
}

pub(super) fn declared_callables(
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
        specialize_member_function(source, receiver, function, &base_bindings);
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

/// Apply one classifier receiver's already-resolved formal bindings to a member candidate.
/// Providers use this through [`specialize_declared_callables`]; the active body-local overlay uses
/// the same operation after its stable classifier layout is checked on the current lexical rung.
pub(crate) fn specialize_member_function(
    source: &dyn SymbolSource,
    receiver: Ty,
    function: &mut FunctionInfo,
    classifier_bindings: &super::GSigBinds,
) {
    let mut bindings = classifier_bindings.clone();
    if let Some(signature) = &function.generic_sig {
        for formal in &signature.formals {
            // A method formal always owns its name. In `class Box<T> { fun <T> echo(T): T }`, the
            // method's `T` shadows the receiver-bound class `T`; retaining the class binding here
            // would specialize `Box<String>.echo(42)` before overload inference.
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
        signature.ret = specialize_member_type(source, signature.ret, &bindings, TypePosition::Out);
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

/// Whether the applied receiver hierarchy declares `name` only as a hidden-deprecated callable.
/// Providers retain this rejection fact beside the classifier while excluding the declaration from
/// ordinary candidate collection. Resolution therefore never infers it from a classifier/member
/// spelling and never makes it applicable.
pub(crate) fn has_hidden_deprecated_member(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
) -> bool {
    let mut queue = std::collections::VecDeque::from([receiver.non_null()]);
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = queue.pop_front() {
        if let Some(parts) = super::hierarchy_projection::intersection_components(current) {
            queue.extend(parts.iter().copied());
            continue;
        }
        let Some(internal) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(internal) {
            continue;
        }
        let Some(classifier) = source.classifier(internal) else {
            continue;
        };
        if classifier.hidden_deprecated_callables.contains(name) {
            return true;
        }
        queue.extend(direct_supertypes_from_classifier(&classifier, current));
    }
    false
}

impl super::SymbolResolver<'_> {
    /// A provider rejection fact consulted only after ordinary candidate selection has failed.
    pub(crate) fn receiver_has_hidden_deprecated_member(&self, receiver: Ty, name: &str) -> bool {
        has_hidden_deprecated_member(
            &self.src,
            super::hierarchy_projection::member_scope_receiver(receiver),
            name,
        )
    }
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
    let intersection_family = matches!(receiver.non_null(), Ty::Intersection(_));
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
        if let Some(parts) = super::hierarchy_projection::intersection_components(current) {
            queue.extend(parts.iter().copied().map(|part| (part, depth)));
            continue;
        }
        let Some(internal) = current.kotlin_class_internal() else {
            continue;
        };
        // The same classifier reached through distinct applied supertypes is a distinct hierarchy
        // rung: its substituted parameter and result types can differ. Declaration-aware
        // normalization below decides whether those members override each other.
        if !seen.insert(current) {
            continue;
        }
        let Some(classifier) = source.classifier(internal) else {
            continue;
        };
        let (mut current_functions, mut current_properties) =
            declared_callables(source, &classifier, current, name).into_parts();
        if depth > 0 {
            // Private declarations belong only to their declaring classifier. They are not an
            // inaccessible inherited candidate: omitting them lets the scope tower continue to a
            // lexical outer receiver that may legally own the same name.
            current_functions
                .overloads
                .retain(|function| member_is_inheritable(function.visibility));
            current_properties
                .overloads
                .retain(|property| member_is_inheritable(property.visibility));
        }
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
        // Preserve exact declaration identity and provenance until the established override
        // normalizer sees the complete family. Same-signature siblings can differ in defaults,
        // visibility, annotations, and realization; shape-based dedupe would silently pick one.
        functions.overloads.extend(current_functions.overloads);
        properties.overloads.extend(current_properties.overloads);
        queue.extend(
            direct_supertypes_from_classifier(&classifier, current)
                .into_iter()
                .map(|supertype| (supertype, depth + 1)),
        );
    }

    normalize_inherited_member_functions_with_family(source, &mut functions, intersection_family);
    Callables::from_parts(functions, properties)
}

/// Normalize one complete, receiver-ranked member family after hierarchy traversal. The ordinary
/// provider-backed walk and the checker's active body-local overlay both feed this same operation;
/// providers continue to expose only declarations and direct supertypes.
pub(crate) fn normalize_inherited_member_functions(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
) {
    normalize_inherited_member_functions_with_family(source, functions, false);
}

fn normalize_inherited_member_functions_with_family(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
    intersection_family: bool,
) {
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
    inherit_overridden_default_arguments(source, functions);
    inherit_overridden_results(source, functions);
    retain_covariant_inherited_overrides(source, functions, intersection_family);
}

/// Left-to-right depth-first visit order of `root` and its supertypes. The first visit wins in a
/// diamond, matching the order an inherited default expression is chosen.
pub(crate) fn supertype_preorder(
    source: &dyn SymbolSource,
    root: TypeName,
) -> std::collections::HashMap<TypeName, u32> {
    let mut order = std::collections::HashMap::new();
    let mut next = 0u32;
    let mut pending = vec![root];
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        order.insert(current, next);
        next = next.saturating_add(1);
        for supertype in super::direct_supertypes(source, Ty::obj_name(current))
            .into_iter()
            .rev()
        {
            if let Some(name) = supertype.kotlin_class_internal() {
                pending.push(name);
            }
        }
    }
    order
}

/// The inherited declaration whose default expressions a fake override uses.
///
/// Several overridden functions may each declare defaults for the same parameter. That conflict is
/// rejected when the functions are both reached directly, and kept as a compatibility warning when
/// one is reached through an intermediate classifier (`KT-36188`). The expressions that survive are
/// the ones on the leftmost supertype in a depth-first walk, not the nearest declaration: `A2 : A`
/// listed before `B` still uses `A`'s default even though `B` is a shallower rung.
fn leftmost_default_supplier<'a>(
    source: &dyn SymbolSource,
    owner: TypeName,
    inherited: &[&'a FunctionInfo],
) -> Option<&'a FunctionInfo> {
    let order = supertype_preorder(source, owner);
    inherited
        .iter()
        .copied()
        .filter(|candidate| {
            candidate
                .call_sig
                .param_defaults
                .iter()
                .any(|default| *default)
        })
        .min_by_key(|candidate| {
            (
                order
                    .get(&candidate.callable.owner)
                    .copied()
                    .unwrap_or(u32::MAX),
                candidate.receiver_rank,
            )
        })
}

/// Publish inherited default-argument availability on the overriding declaration that remains the
/// semantic call target. Kotlin forbids repeating defaults on an override: a call through the
/// derived receiver selects the override's covariant result and parameter contract, while omitted
/// slots obtain their expressions from an overridden declaration. The provider coordinate is kept
/// as realization data; it must never replace the selected callable during overload resolution.
/// The declarations of one receiver-ranked member family whose slot `implementation` occupies:
/// those it overrides, and an abstract sibling it implements as one fake override.
fn inherited_declarations<'a>(
    source: &dyn SymbolSource,
    implementation: &FunctionInfo,
    declarations: &'a [FunctionInfo],
) -> Vec<&'a FunctionInfo> {
    let implementation_result = implementation.ret.apply(implementation.callable.ret);
    let implementation_parameters = implementation.semantic_params();
    declarations
        .iter()
        .filter(|candidate| {
            let owner_override = implementation.callable.owner != candidate.callable.owner
                && resolution_subtype(
                    source,
                    Ty::obj_name(implementation.callable.owner),
                    Ty::obj_name(candidate.callable.owner),
                );
            // Unrelated inherited declarations can still contribute one fake-override slot.
            // In particular, a concrete/delegated implementation from one interface inherits
            // default availability declared by an abstract sibling interface.  Two unrelated
            // concrete bodies remain a conflict and are deliberately not joined here.
            let same_rank_fake_override = candidate.receiver_rank == implementation.receiver_rank
                && (candidate.flags.is_abstract || implementation.flags.is_abstract);
            (candidate.receiver_rank > implementation.receiver_rank
                || owner_override
                || same_rank_fake_override)
                && candidate.context_count == implementation.context_count
                && candidate.semantic_params() == implementation_parameters
                && resolution_subtype(
                    source,
                    implementation_result,
                    candidate.ret.apply(candidate.callable.ret),
                )
        })
        .collect()
}

/// The normalizer records a non-null classifier or a declared type parameter. Anything else
/// (`Unit`, a function type) has no carrier to publish, and a nullable result is left unrecorded
/// because the box is what crosses the continuation.
fn expects_recorded_suspend_result(candidate: &FunctionInfo) -> bool {
    if candidate.ret.nullable {
        return false;
    }
    matches!(
        candidate.callable.ret.non_null(),
        Ty::Obj(..) | Ty::TyParam(..)
    )
}

/// Publish on each dependency suspend declaration the declared results of the declarations it
/// overrides (a current-module override's are the checked override graph's). A declared result is
/// the overridden declaration's own, before its owner's type arguments apply: `Base<T>.value(): T`
/// is recorded as `T` whatever the receiver binds it to.
fn inherit_overridden_results(source: &dyn SymbolSource, functions: &mut FunctionSet) {
    let declarations = functions.overloads.clone();
    for implementation in &mut functions.overloads {
        if !implementation.flags.suspend || implementation.callable.external_identity.is_none() {
            continue;
        }
        let results = inherited_declarations(source, implementation, &declarations)
            .into_iter()
            .filter(|candidate| candidate.callable.owner != implementation.callable.owner)
            .filter_map(|candidate| match candidate.callable.declared_ret {
                Some(result) => Some(result),
                // Nullable results, and results that are not a classifier or a type parameter,
                // are deliberately unrecorded. A nullable result crosses the continuation as a
                // box. `Unit` is not a classifier (`Ty::Unit`, not `kotlin/Unit`), so neither is
                // an overridden carrier to publish.
                None if !expects_recorded_suspend_result(candidate) => None,
                None => panic!(
                    "a normalized dependency suspend declaration carries its declared result"
                ),
            })
            .collect::<Vec<_>>();
        implementation.callable.overridden_results = results.into_boxed_slice();
    }
}

fn inherit_overridden_default_arguments(source: &dyn SymbolSource, functions: &mut FunctionSet) {
    let declarations = functions.overloads.clone();
    for implementation in &mut functions.overloads {
        let inherited = inherited_declarations(source, implementation, &declarations);
        if inherited.is_empty() {
            continue;
        }
        // Providers compact an all-false default bitmap to an empty vector.  An overriding
        // declaration loaded from metadata therefore commonly has no bitmap at all, even though
        // an overridden declaration supplies a default for the same semantic slot.  Materialize
        // the compact representation before merging the inherited availability; using the stored
        // bitmap length here silently skipped every slot across a compiled-module boundary.
        let parameter_count = implementation.semantic_params().len();
        if implementation.call_sig.param_defaults.is_empty() {
            implementation.call_sig.param_defaults = vec![false; parameter_count];
        }
        let supplier = leftmost_default_supplier(source, implementation.callable.owner, &inherited);
        let mut inherited_any_default = false;
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
                inherited_any_default = true;
                if implementation
                    .default_values
                    .get(parameter)
                    .is_some_and(Option::is_none)
                {
                    let value = match supplier {
                        Some(supplier) => supplier.default_values.get(parameter).cloned().flatten(),
                        None => inherited
                            .iter()
                            .find_map(|candidate| candidate.default_values.get(parameter))
                            .cloned()
                            .flatten(),
                    };
                    if let Some(value) = value {
                        implementation.default_values[parameter] = Some(value);
                    }
                }
            }
        }
        implementation.call_sig.required = crate::libraries::required_arity(
            parameter_count,
            &implementation.call_sig.param_defaults,
        );
        if inherited_any_default {
            if let Some(supplier) = supplier {
                if implementation.callable.external_default_provider.is_none() {
                    implementation.callable.external_default_provider = supplier
                        .callable
                        .external_default_provider
                        .or(supplier.callable.external_identity);
                }
                if implementation.callable.default_realization.is_none() {
                    implementation.callable.default_realization =
                        supplier.callable.default_realization.clone();
                }
            }
        } else if implementation.callable.default_realization.is_none() {
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
                function.callable.singleton_dispatch = Some(Box::new(singleton));
                true
            }
            FnKind::Extension => {
                function.callable.singleton_dispatch = Some(Box::new(singleton));
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
                property.getter.singleton_dispatch = Some(Box::new(singleton));
                if let Some(setter) = &mut property.setter {
                    setter.singleton_dispatch = Some(Box::new(singleton));
                }
                true
            }
            PropKind::MemberExtension => {
                property.kind = PropKind::Extension;
                property.getter.singleton_dispatch = Some(Box::new(singleton));
                if let Some(setter) = &mut property.setter {
                    setter.singleton_dispatch = Some(Box::new(singleton));
                }
                true
            }
            PropKind::Extension => {
                property.getter.singleton_dispatch = Some(Box::new(singleton));
                if let Some(setter) = &mut property.setter {
                    setter.singleton_dispatch = Some(Box::new(singleton));
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
                function.callable.singleton_dispatch = Some(Box::new(singleton));
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

/// Collapse declarations that represent one inherited function slot. A declaration owned by a
/// subtype overrides the matching slot owned by its supertype even when both owners are direct
/// supertypes of the use-site classifier (`class B : A(), T` with `A.foo` overriding `T.foo`). For
/// sibling owners at the same receiver rung, Kotlin's covariant-return fake override keeps the
/// uniquely most-specific result. Incomparable owners/results remain separate so ordinary
/// overload/ambiguity diagnostics can reject an invalid hierarchy.
fn retain_covariant_inherited_overrides(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
    intersection_family: bool,
) {
    let mut retained: Vec<FunctionInfo> = Vec::with_capacity(functions.overloads.len());
    for candidate in functions.overloads.drain(..) {
        let candidate_ret = candidate.ret.apply(candidate.callable.ret);
        let mut candidate_is_shadowed = false;
        let mut shadowed = Vec::new();
        for (index, existing) in retained.iter().enumerate() {
            if existing.context_count != candidate.context_count
                || !same_slot_parameters(&existing.semantic_params(), &candidate.semantic_params())
            {
                continue;
            }
            let existing_ret = existing.ret.apply(existing.callable.ret);
            let candidate_is_subtype = resolution_subtype(source, candidate_ret, existing_ret);
            let existing_is_subtype = resolution_subtype(source, existing_ret, candidate_ret);
            let candidate_owner_overrides = candidate.callable.owner != existing.callable.owner
                && resolution_subtype(
                    source,
                    Ty::obj_name(candidate.callable.owner),
                    Ty::obj_name(existing.callable.owner),
                );
            let existing_owner_overrides = candidate.callable.owner != existing.callable.owner
                && resolution_subtype(
                    source,
                    Ty::obj_name(existing.callable.owner),
                    Ty::obj_name(candidate.callable.owner),
                );
            // A nearer declaration reached by the same breadth-first receiver walk is an override
            // slot even while an active local classifier has no provider-published owner edge yet.
            // Unrelated direct supertypes remain at the same rank and still require the ordinary
            // owner/result comparison below.
            let candidate_rank_overrides = candidate.receiver_rank < existing.receiver_rank;
            let existing_rank_overrides = existing.receiver_rank < candidate.receiver_rank;
            let same_result = candidate_is_subtype && existing_is_subtype;
            let candidate_implements_abstract = same_result
                && candidate.receiver_rank == existing.receiver_rank
                && !candidate.flags.is_abstract
                && existing.flags.is_abstract;
            let existing_implements_abstract = same_result
                && candidate.receiver_rank == existing.receiver_rank
                && !existing.flags.is_abstract
                && candidate.flags.is_abstract;
            // An ordinary class/interface contributes one fake-override slot for unrelated
            // abstract declarations. An inferred intersection has no declaring classifier that
            // owns such a slot: keep unrelated declarations exact so defaults and provenance can
            // participate in selection. Repeated views of the same owner are still one declaration.
            let both_abstract_fake_override = same_result
                && candidate.flags.is_abstract
                && existing.flags.is_abstract
                && existing.receiver_rank == candidate.receiver_rank
                && (!intersection_family || candidate.callable.owner == existing.callable.owner);
            if candidate_implements_abstract
                || (candidate_is_subtype
                    && (candidate_owner_overrides
                        || candidate_rank_overrides
                        || (existing.receiver_rank == candidate.receiver_rank
                            && !existing_is_subtype)))
            {
                shadowed.push(index);
            } else if existing_implements_abstract
                || both_abstract_fake_override
                || (existing_is_subtype
                    && (existing_owner_overrides
                        || existing_rank_overrides
                        || (existing.receiver_rank == candidate.receiver_rank
                            && !candidate_is_subtype)))
            {
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

/// Parameter lists occupying one override slot. A Java platform type is flexible, so an override
/// of `J.from(String!)` declared as `from(String)` or `from(String?)` is the same slot, as in
/// kotlinc's override checker.
fn same_slot_parameters(left: &[Ty], right: &[Ty]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(&left, &right)| crate::assignable::same_flexible_type(left, right))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_input_shape_includes_context_and_suspend() {
        let source = crate::libraries::EmptySymbolSource;
        let shape = |context_count, suspend| OverrideInputShape {
            params: &[],
            receiver: None,
            formals: &[],
            formal_bounds: &[],
            context_count,
            suspend,
        };

        assert!(override_input_shapes_match(
            &source,
            shape(1, true),
            shape(1, true),
        ));
        assert!(!override_input_shapes_match(
            &source,
            shape(1, true),
            shape(0, true),
        ));
        assert!(!override_input_shapes_match(
            &source,
            shape(1, true),
            shape(1, false),
        ));
    }

    #[test]
    fn override_input_shape_includes_type_parameter_bounds() {
        let source = crate::libraries::EmptySymbolSource;
        let formals = ["S".to_string()];
        let param = [Ty::ty_param("S", Ty::nullable(Ty::obj("kotlin/Any")))];
        let wide = [vec![Ty::obj("app/Shape")]];
        let narrow = [vec![Ty::obj("app/Circle")]];
        let implicit = [Vec::new()];
        let explicit_top = [vec![Ty::nullable(Ty::obj("kotlin/Any"))]];
        let shape = |formal_bounds| OverrideInputShape {
            params: &param,
            receiver: None,
            formals: &formals,
            formal_bounds,
            context_count: 0,
            suspend: false,
        };

        assert!(override_input_shapes_match(
            &source,
            shape(&wide),
            shape(&wide),
        ));
        assert!(!override_input_shapes_match(
            &source,
            shape(&wide),
            shape(&narrow),
        ));
        assert!(override_input_shapes_match(
            &source,
            shape(&implicit),
            shape(&explicit_top),
        ));
    }
}

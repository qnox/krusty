//! Declared and inherited member lookup over applied classifier hierarchies.

use super::{
    classifier_bindings, direct_supertypes_from_classifier, resolution_subtype,
    specialize_call_sig, specialize_callable, specialize_member_type, ty_subst_keep_unbound,
    TypePosition,
};
use crate::libraries::{Callables, FnKind, FunctionInfo, FunctionSet, PropKind, PropertySet};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{Ty, TypeName};

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
                let same_rank_fake_override = candidate.receiver_rank
                    == implementation.receiver_rank
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
            .collect::<Vec<_>>();
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
        if inherited_any_default && implementation.callable.external_default_provider.is_none() {
            implementation.callable.external_default_provider = inherited
                .iter()
                .filter(|candidate| {
                    candidate
                        .call_sig
                        .param_defaults
                        .iter()
                        .any(|default| *default)
                })
                .min_by_key(|candidate| candidate.receiver_rank)
                .and_then(|candidate| {
                    candidate
                        .callable
                        .external_default_provider
                        .or(candidate.callable.external_identity)
                });
        }
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
fn retain_covariant_inherited_overrides(source: &dyn SymbolSource, functions: &mut FunctionSet) {
    let mut retained: Vec<FunctionInfo> = Vec::with_capacity(functions.overloads.len());
    for candidate in functions.overloads.drain(..) {
        let candidate_ret = candidate.ret.apply(candidate.callable.ret);
        let mut candidate_is_shadowed = false;
        let mut shadowed = Vec::new();
        for (index, existing) in retained.iter().enumerate() {
            if existing.context_count != candidate.context_count
                || existing.semantic_params() != candidate.semantic_params()
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
            let same_result = candidate_is_subtype && existing_is_subtype;
            let candidate_implements_abstract = same_result
                && candidate.receiver_rank == existing.receiver_rank
                && !candidate.flags.is_abstract
                && existing.flags.is_abstract;
            let existing_implements_abstract = same_result
                && candidate.receiver_rank == existing.receiver_rank
                && !existing.flags.is_abstract
                && candidate.flags.is_abstract;
            let both_abstract_fake_override = same_result
                && candidate.flags.is_abstract
                && existing.flags.is_abstract
                && existing.receiver_rank == candidate.receiver_rank;
            if candidate_implements_abstract
                || (candidate_is_subtype
                    && (candidate_owner_overrides
                        || (existing.receiver_rank == candidate.receiver_rank
                            && !existing_is_subtype)))
            {
                shadowed.push(index);
            } else if existing_implements_abstract
                || both_abstract_fake_override
                || (existing_is_subtype
                    && (existing_owner_overrides
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

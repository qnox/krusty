//! Declared and inherited member lookup over applied classifier hierarchies.

mod bound_inner_aliases;
mod bound_inner_constructors;
mod result_enhancement;

pub(crate) use bound_inner_aliases::{
    apply_inner_alias_outer_in_source, apply_inner_classifier_outer_in_source,
    bound_inner_classifier_in_source, InnerAliasOuterApplication,
};
pub(crate) use bound_inner_constructors::bound_inner_constructor_candidates;
pub(crate) use bound_inner_constructors::BoundInnerConstructorContext;

use super::{
    classifier_bindings, projected_supertypes, resolution_subtype, specialize_call_sig,
    specialize_callable, specialize_member_type, ty_subst_keep_unbound, PropertyAccess,
    TypePosition,
};
use crate::libraries::{Callables, FnKind, FunctionInfo, FunctionSet, PropKind, PropertySet};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{Ty, TyKind, TypeName};

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
                // kotlinc substitutes the implementation's own formals by the inherited ones. A
                // declaration nested in the inherited function's body can name that function's
                // formal itself, which then occupies the inherited slot.
                let implementation = crate::types::ty_canonicalize_params(
                    crate::types::ty_canonicalize_params(implementation, implementation_formals),
                    inherited_formals,
                );
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

/// The override input shape of one function as its provider published it. A provider publishes a
/// generic signature for every callable that declares type parameters (module symbols, Kotlin
/// metadata, and a Java `Signature` attribute alike), so a callable without one declares none.
fn function_override_input_shape<'a>(
    function: &'a FunctionInfo,
    params: &'a [Ty],
) -> OverrideInputShape<'a> {
    let (formals, formal_bounds) = match function.generic_sig.as_ref() {
        Some(signature) => (
            signature.formals.as_slice(),
            signature.formal_bounds.as_slice(),
        ),
        None => (&[][..], &[][..]),
    };
    OverrideInputShape {
        params,
        receiver: function
            .semantic_receiver()
            .filter(|_| function.is_extension()),
        formals,
        formal_bounds,
        context_count: function.context_count,
        suspend: function.flags.suspend,
    }
}

/// Whether `implementation` and `inherited`, both viewed as members of one receiver type, share
/// one override slot: [`override_input_shapes_match`] over each function's published shape.
pub(crate) fn function_input_shapes_match(
    source: &dyn SymbolSource,
    inherited: &FunctionInfo,
    implementation: &FunctionInfo,
) -> bool {
    if inherited.kind != implementation.kind {
        return false;
    }
    let inherited_params = inherited.semantic_params();
    let implementation_params = implementation.semantic_params();
    override_input_shapes_match(
        source,
        function_override_input_shape(inherited, &inherited_params),
        function_override_input_shape(implementation, &implementation_params),
    )
}

/// Whether two declarations occupy the same semantic or provider-normalized override input slot.
///
/// Dependency providers may publish an opaque slot when their source-language view cannot express
/// the declaration relation directly (for example, a Java collection class method whose erased
/// input implements an applied Kotlin collection interface method). Common hierarchy code may
/// compare those identities, but cannot inspect or reconstruct their target representation.
pub(crate) fn function_override_input_slots_match(
    source: &dyn SymbolSource,
    inherited: &FunctionInfo,
    implementation: &FunctionInfo,
) -> bool {
    inherited.kind == implementation.kind
        && (function_input_shapes_match(source, inherited, implementation)
            || inherited
                .provider_override_slot
                .as_ref()
                .zip(implementation.provider_override_slot.as_ref())
                .is_some_and(|(inherited, implementation)| inherited.same_key(implementation)))
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
    // A refined common fragment sees an `actual` class only through its `expect`: the members only
    // the actual declares are not declarations there.
    let (mut functions, mut properties) = callables.clone().into_parts();
    functions
        .overloads
        .retain(|function| source.sees_function(function));
    properties
        .overloads
        .retain(|property| source.sees_property(property));
    specialize_declared_callables(
        source,
        classifier,
        receiver,
        Callables::from_parts(functions, properties),
    )
}

fn specialize_declared_callables(
    source: &dyn SymbolSource,
    classifier: &crate::libraries::LibraryType,
    receiver: Ty,
    callables: Callables,
) -> Callables {
    let base_bindings = classifier_bindings(classifier, receiver);
    let input_bounds =
        crate::libraries::ReceiverInputBounds::from_parameters(&classifier.type_parameters);
    let (mut functions, mut properties) = callables.into_parts();
    for function in &mut functions.overloads {
        specialize_member_function(source, receiver, function, &base_bindings, &input_bounds);
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
        property
            .getter
            .enhanced_result
            .specialize(receiver.kotlin_class_internal(), property.ty);
        property.ty = specialize_member_type(source, property.ty, &bindings, TypePosition::Out);
        specialize_callable(source, &mut property.getter, &bindings);
        // The applied property's slot identity includes its own formal bounds. Those bounds may
        // mention the declaring classifier's formals (`Parent<X>.val <T : X> T.x`), so specialize
        // only the applied copy while the declaration table retains the raw signature for
        // diagnostics. Property-owned formals were removed from `bindings` above and stay open.
        if let Some(signature) = &mut property.getter.generic_sig {
            for bounds in &mut signature.formal_bounds {
                for bound in bounds {
                    *bound = ty_subst_keep_unbound(*bound, &bindings);
                }
            }
        }
        property.getter.ret = property.ty;
        if let Some(setter) = &mut property.setter {
            specialize_callable(source, setter, &bindings);
            if let Some(signature) = &mut setter.generic_sig {
                for bounds in &mut signature.formal_bounds {
                    for bound in bounds {
                        *bound = ty_subst_keep_unbound(*bound, &bindings);
                    }
                }
            }
        }
        if super::raw_member_view::is_raw_receiver(receiver) {
            super::raw_member_view::erase_property(source, property);
        }
        if raw_owner_property {
            let erased = property.getter.physical_ret;
            let erased = match declared_nullability.kind() {
                TyKind::Nullable(_) => Ty::nullable(erased.non_null()),
                TyKind::PlatformNullable(_) => Ty::platform_nullable(erased.non_null()),
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
    input_bounds: &crate::libraries::ReceiverInputBounds,
) {
    if function.declaration_identity.is_none() {
        function.declaration_identity = Some(Box::new(declaration_identity(function)));
    }
    let mut bindings = classifier_bindings.clone();
    if let Some(signature) = &function.generic_sig {
        for formal in &signature.formals {
            // A method formal always owns its name. In `class Box<T> { fun <T> echo(T): T }`, the
            // method's `T` shadows the receiver-bound class `T`; retaining the class binding here
            // would specialize `Box<String>.echo(42)` before overload inference.
            bindings.remove(formal);
        }
    }
    let raw_receiver = super::raw_member_view::is_raw_receiver(receiver);
    if let (true, Some(signature)) = (raw_receiver, &function.generic_sig) {
        super::raw_member_view::bind_method_formals(signature, &mut bindings);
    }
    if !bindings.is_empty() {
        if function.source_inputs.is_none() {
            let mut signature = function.semantic_signature().into_owned();
            if function.generic_sig.is_some() {
                normalize_suspend_member_signature(&mut signature, function.flags.suspend);
            }
            function.source_inputs = Some(Box::new(crate::libraries::SourceInputDeclaration {
                signature,
                receiver_bindings: super::GSigBinds::default(),
                receiver_bounds: crate::libraries::ReceiverInputBounds::default(),
            }));
        }
        function
            .source_inputs
            .as_mut()
            .expect("receiver application retains the declaration input contract")
            .apply_receiver(&bindings, input_bounds);
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
    let declared_result = function
        .generic_sig
        .as_ref()
        .map_or(function.callable.ret, |signature| signature.ret);
    function
        .callable
        .enhanced_result
        .specialize(receiver.kotlin_class_internal(), declared_result);
    if let TyKind::PlatformNullable(rigid) = declared_result.kind() {
        result_enhancement::publish_flexible_result(
            &mut function.call_sig.result_enhancement,
            specialize_member_type(source, rigid, &bindings, TypePosition::Out),
        );
    }
    specialize_callable(source, &mut function.callable, &bindings);
    function.ret.class = function
        .ret
        .class
        .map(|ty| ty_subst_keep_unbound(ty, &bindings));
    specialize_call_sig(source, &mut function.call_sig, &bindings);
    if let Some(signature) = &mut function.generic_sig {
        normalize_suspend_member_signature(signature, function.flags.suspend);
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
    if raw_receiver {
        super::raw_member_view::erase_function(source, function);
    }
}

/// The existing logical suspend-member view, shared with the retained declaration snapshot.
fn normalize_suspend_member_signature(signature: &mut crate::libraries::GenericSig, suspend: bool) {
    let suspend_ret = suspend
        .then(|| {
            let continuation = *signature.params.last()?;
            match continuation.kind() {
                TyKind::Obj(name, args)
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
}

/// All exact declarations in one classifier slot, including HIDDEN-deprecated callables used by
/// override matching. This is a declaration-model query, not a call-candidate query.
pub(crate) fn declared_member_declarations(
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

/// Call-visible declarations in one classifier slot. HIDDEN declarations remain in the common
/// model for override edges but never enter overload selection.
pub(crate) fn declared_member_callables(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
) -> Callables {
    let (mut functions, mut properties) =
        declared_member_declarations(source, receiver, name).into_parts();
    functions
        .overloads
        .retain(|function| !function.flags.deprecated_hidden);
    properties
        .overloads
        .retain(|property| !property.deprecated_hidden);
    Callables::from_parts(functions, properties)
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
    let mut seen = crate::fx_hash::HashSet::default();
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
        if classifier.hidden_deprecated_callables.contains(name)
            || source.api_withheld_callable(internal, name)
        {
            return true;
        }
        queue.extend(projected_supertypes(source, &classifier, current));
    }
    false
}

/// Whether failed property selection encountered an independently HIDDEN accessor in the applied
/// receiver hierarchy. Providers publish getter/setter availability on the property declaration;
/// diagnostic selection consumes that exact fact instead of reopening annotations or deriving an
/// accessor from its spelling.
pub(crate) fn has_hidden_deprecated_property_accessor(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
    access: PropertyAccess,
) -> bool {
    let mut queue = std::collections::VecDeque::from([receiver.non_null()]);
    let mut seen = crate::fx_hash::HashSet::default();
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
        let (_, properties) = declared_callables(source, &classifier, current, name).into_parts();
        if classifier.hidden_deprecated_callables.contains(name)
            || source.api_withheld_callable(internal, name)
            || properties.overloads.iter().any(|property| {
                property.deprecated_hidden
                    || match access {
                        PropertyAccess::Read => !property.accessor_availability.getter,
                        PropertyAccess::Write => {
                            property.setter.is_some() && !property.accessor_availability.setter
                        }
                    }
            })
        {
            return true;
        }
        queue.extend(projected_supertypes(source, &classifier, current));
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

    /// The accessor-specific rejection fact for a failed property read or write.
    pub(crate) fn receiver_has_hidden_deprecated_property_accessor(
        &self,
        receiver: Ty,
        name: &str,
        access: PropertyAccess,
    ) -> bool {
        has_hidden_deprecated_property_accessor(
            &self.src,
            super::hierarchy_projection::member_scope_receiver(receiver),
            name,
            access,
        )
    }
}

pub(crate) fn members_in_hierarchy(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
) -> Callables {
    members_in_hierarchy_for(source, receiver, name, HierarchyMemberView::CallCandidates)
}

/// All declarations in an applied receiver hierarchy, including HIDDEN-deprecated callables.
/// Override validation consumes declaration semantics rather than the call-candidate view.
pub(crate) fn member_declarations_in_hierarchy(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
) -> Callables {
    members_in_hierarchy_for(source, receiver, name, HierarchyMemberView::Declarations)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HierarchyMemberView {
    CallCandidates,
    Declarations,
}

fn members_in_hierarchy_for(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
    view: HierarchyMemberView,
) -> Callables {
    // A function type carries its callable shape directly in `FnSig`; it is not named by deriving a
    // `FunctionN` classifier from the parameter count. For ordinary member lookup its declared
    // classifier is the arity-independent `Function<R>`, whose hierarchy supplies `Any` members.
    // `invoke` remains a member of the `FnSig` itself and is handled by the caller from that signature.
    let intersection_parts =
        super::hierarchy_projection::intersection_components(receiver.non_null());
    let receiver = match receiver.non_null().kind() {
        TyKind::Fun(signature) => Ty::obj_args("kotlin/Function", &[signature.ret]),
        TyKind::Unit => Ty::obj("kotlin/Unit"),
        TyKind::Nothing => Ty::obj("kotlin/Nothing"),
        _ => receiver,
    };
    let mut functions = FunctionSet::default();
    let mut properties = PropertySet::default();
    let mut queue = std::collections::VecDeque::from([(receiver, 0)]);
    let mut seen = crate::fx_hash::HashSet::default();
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
        if view == HierarchyMemberView::Declarations {
            // A member extension is absent from ordinary dispatch lookup: it has an implicit
            // dispatch receiver and a separate extension receiver, so `host.ext()` must not make
            // it look like an ordinary member of `host`. It is nevertheless a declaration of the
            // classifier and participates in override/abstract-member validation. Providers keep
            // that exact declaration in their semantic member table; project it into the
            // declaration-only hierarchy view and specialize the declaring classifier's type
            // parameters through this applied rung.
            let declared_extensions = classifier
                .members
                .iter()
                .filter(|member| member.name == name && member.is_member_extension())
                .cloned()
                .map(|member| {
                    let extension_receiver = member
                        .semantic_extension_receiver()
                        .expect("a member-extension declaration publishes its semantic receiver");
                    let mut function =
                        FunctionInfo::classifier_member(FnKind::Extension, internal, member);
                    function.receiver = Some(extension_receiver);
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
                )
                .into_parts()
                .0;
                for declaration in specialized.overloads {
                    let already_published = current_functions.overloads.iter().any(|published| {
                        published.stable_declaration.is_some()
                            && published.stable_declaration == declaration.stable_declaration
                            || published.callable.external_identity.is_some()
                                && published.callable.external_identity
                                    == declaration.callable.external_identity
                    });
                    if !already_published {
                        current_functions.overloads.push(declaration);
                    }
                }
            }
        }
        if view == HierarchyMemberView::CallCandidates {
            current_functions
                .overloads
                .retain(|function| !function.flags.deprecated_hidden);
            current_properties
                .overloads
                .retain(|property| !property.deprecated_hidden);
        }
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
            projected_supertypes(source, &classifier, current)
                .into_iter()
                .map(|supertype| (supertype, depth + 1)),
        );
    }

    normalize_inherited_member_functions_with_family(source, &mut functions, intersection_parts);
    record_overridden_property_declarations(source, &mut properties, intersection_parts);
    if intersection_parts.is_some() {
        // Component order is not a hierarchy distance. A member inherited by the earliest
        // component and a member declared on a later component are one slot; flattening the
        // walk depth lets that selection see both instead of keeping only the shallower rung.
        for function in &mut functions.overloads {
            function.receiver_rank = 0;
        }
    }
    Callables::from_parts(functions, properties)
}

/// Normalize one complete, receiver-ranked member family after hierarchy traversal. The ordinary
/// provider-backed walk and the checker's active body-local overlay both feed this same operation;
/// providers continue to expose only declarations and direct supertypes.
pub(crate) fn normalize_inherited_member_functions(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
) {
    normalize_inherited_member_functions_with_family(source, functions, None);
}

fn normalize_inherited_member_functions_with_family(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
    intersection_parts: Option<&[Ty]>,
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
    inherit_overridden_semantic_roles(source, functions, intersection_parts);
    record_overridden_declarations(source, functions, intersection_parts);
    result_enhancement::enhance_overriding_flexible_results(source, functions);
    inherit_overridden_default_arguments(source, functions);
    inherit_overridden_results(source, functions);
    retain_covariant_inherited_overrides(source, functions, intersection_parts);
}

/// Carry a language role from the declaration an override implements onto the selected override.
///
/// Providers publish roles only on the exact declarations that own them. The common hierarchy is
/// the authority for override relationships, so this is where a nearer declaration inherits that
/// role. Matching only the callable spelling would misclassify an unrelated same-named member;
/// matching the complete override input shape keeps the role attached to one semantic slot.
fn inherit_overridden_semantic_roles(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
    intersection_parts: Option<&[Ty]>,
) {
    let declarations = functions.overloads.clone();
    for implementation in &mut functions.overloads {
        if implementation.callable.semantic_role.is_some() {
            continue;
        }
        let mut inherited_role = None;
        let mut conflicting = false;
        for inherited in &declarations {
            let Some(role) = inherited.callable.semantic_role else {
                continue;
            };
            if !overrides_in_family(source, inherited, implementation, intersection_parts) {
                continue;
            }
            match inherited_role {
                None => inherited_role = Some(role),
                Some(existing) if existing == role => {}
                Some(_) => conflicting = true,
            }
        }
        if !conflicting {
            implementation.callable.semantic_role = inherited_role;
        }
    }
}

/// Whether `implementation` overrides `inherited` within one normalized member family: a
/// declaration further up the receiver's hierarchy (sharing an intersection component when the
/// receiver is one) whose complete override input shape matches and whose result the
/// implementation's refines.
fn overrides_in_family(
    source: &dyn SymbolSource,
    inherited: &FunctionInfo,
    implementation: &FunctionInfo,
    intersection_parts: Option<&[Ty]>,
) -> bool {
    inherited.receiver_rank > implementation.receiver_rank
        && intersection_parts.is_none_or(|parts| {
            owners_share_intersection_component(
                source,
                parts,
                implementation.callable.owner,
                inherited.callable.owner,
            )
        })
        && function_override_input_slots_match(source, inherited, implementation)
        && function_result_refines(source, implementation, inherited)
}

/// Whether `implementation` has a result that may override `inherited`'s result. Input-slot
/// equivalence and result covariance are separate semantic facts: a provider-normalized input key
/// must never by itself discharge a declaration with a narrower result.
pub(crate) fn function_result_refines(
    source: &dyn SymbolSource,
    implementation: &FunctionInfo,
    inherited: &FunctionInfo,
) -> bool {
    resolution_subtype(
        source,
        normalized_function_result(implementation),
        normalized_function_result(inherited),
    )
}

/// A result under alpha-normalized declaration variables and their complete declared bounds.
/// Override input matching has already established that the declarations have the same generic
/// shape; result covariance must therefore compare their variables by ordinal, not by source or
/// metadata spelling (`fun <T> f(): T` and `fun <U> f(): U` are the same contract).
fn normalized_function_result(function: &FunctionInfo) -> Ty {
    let result = function.ret.apply(function.callable.ret);
    let Some(signature) = function.generic_sig.as_ref() else {
        return result;
    };
    let default_bound = Ty::nullable(Ty::obj("kotlin/Any"));
    let bounds = signature
        .formals
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let declared = signature
                .formal_bounds
                .get(index)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let bound = match declared {
                [] => default_bound,
                [bound] => *bound,
                bounds => Ty::intersection(bounds),
            };
            (formal.clone(), bound)
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();
    crate::types::ty_canonicalize_params(
        crate::types::ty_with_transitive_param_bounds(result, &bounds),
        &signature.formals,
    )
}

/// Freeze on each dependency declaration the exact identities of the declarations it overrides.
///
/// The override edges are the ones this hierarchy proves (see [`overrides_in_family`]); providers
/// still publish only direct declarations. A target that implements a language declaration
/// specially (`kotlin/Any.toString(): String`) asks whether the selected declaration is that one
/// or overrides it, rather than classifying a member by its spelling or shape.
fn record_overridden_declarations(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
    intersection_parts: Option<&[Ty]>,
) {
    let declarations = &functions.overloads;
    let recorded = declarations
        .iter()
        .map(|implementation| {
            let mut overridden = Vec::<crate::types::OverriddenDeclaration>::new();
            if implementation.callable.external_identity.is_none() {
                return overridden.into_boxed_slice();
            }
            for inherited in declarations {
                if !overrides_in_family(source, inherited, implementation, intersection_parts) {
                    continue;
                }
                let identity = inherited
                    .declaration_identity
                    .as_deref()
                    .cloned()
                    .unwrap_or_else(|| declaration_identity(inherited));
                if !overridden.contains(&identity) {
                    overridden.push(identity);
                }
            }
            overridden.into_boxed_slice()
        })
        .collect::<Vec<_>>();
    for (implementation, overridden) in functions.overloads.iter_mut().zip(recorded) {
        implementation.callable.overridden_declarations = overridden;
    }
}

/// The exact semantic identity of one declaration in a member family: its declaring classifier,
/// Kotlin name, and declared receiver, parameter and result types before use-site substitution.
fn declaration_identity(declaration: &FunctionInfo) -> crate::types::OverriddenDeclaration {
    let callable = &declaration.callable;
    let owner = match callable.declaration_owner {
        Some(crate::types::SemanticCallableOwner::Classifier(owner)) => owner,
        _ => callable.owner,
    };
    let signature = declaration.generic_sig.as_ref();
    crate::types::OverriddenDeclaration {
        owner,
        name: callable.name.as_str().into(),
        receiver: signature
            .and_then(|signature| signature.receiver)
            .or(callable.source_receiver),
        params: declaration
            .semantic_params()
            .into_owned()
            .into_boxed_slice(),
        ret: signature
            .map(|signature| signature.ret)
            .or(callable.declared_ret)
            .unwrap_or(callable.ret),
    }
}

/// The exact semantic identity of one property declaration. Ordinary dispatch receivers are not
/// override inputs; a member-extension receiver is. Accessor context parameters remain inputs,
/// while a setter value is part of the property's mutability rather than its getter identity.
pub(crate) fn property_declaration_identity(
    property: &crate::libraries::PropertyInfo,
) -> crate::types::OverriddenDeclaration {
    let getter = &property.getter;
    let owner = match getter.declaration_owner {
        Some(crate::types::SemanticCallableOwner::Classifier(owner)) => owner,
        _ => property.owner,
    };
    let generic = getter.generic_sig.as_ref();
    let declared_params = getter
        .declared_params
        .as_deref()
        .or_else(|| generic.map(|signature| signature.params.as_slice()))
        .unwrap_or(&getter.params);
    crate::types::OverriddenDeclaration {
        owner,
        name: property.name.as_str().into(),
        receiver: (property.kind == PropKind::MemberExtension)
            .then(|| {
                generic
                    .and_then(|signature| signature.receiver)
                    .or(property.receiver)
            })
            .flatten(),
        params: declared_params
            .iter()
            .take(property.context_count)
            .copied()
            .collect(),
        ret: generic
            .map(|signature| signature.ret)
            .or(getter.declared_ret)
            .unwrap_or(property.ty),
    }
}

fn property_override_input_shape<'a>(
    property: &'a crate::libraries::PropertyInfo,
) -> OverrideInputShape<'a> {
    let (formals, formal_bounds) = match property.getter.generic_sig.as_ref() {
        Some(signature) => (
            property.formals.as_slice(),
            signature.formal_bounds.as_slice(),
        ),
        None => (property.formals.as_slice(), &[][..]),
    };
    let context_count = property.context_count.min(property.getter.params.len());
    OverrideInputShape {
        params: &property.getter.params[..context_count],
        receiver: (property.kind == PropKind::MemberExtension)
            .then_some(property.receiver)
            .flatten(),
        formals,
        formal_bounds,
        context_count: property.context_count,
        suspend: false,
    }
}

fn property_overrides_in_family(
    source: &dyn SymbolSource,
    inherited: &crate::libraries::PropertyInfo,
    implementation: &crate::libraries::PropertyInfo,
    intersection_parts: Option<&[Ty]>,
) -> bool {
    inherited.receiver_rank > implementation.receiver_rank
        && intersection_parts.is_none_or(|parts| {
            owners_share_intersection_component(
                source,
                parts,
                implementation.owner,
                inherited.owner,
            )
        })
        && override_input_shapes_match(
            source,
            property_override_input_shape(inherited),
            property_override_input_shape(implementation),
        )
        && resolution_subtype(source, implementation.ty, inherited.ty)
}

/// Freeze exact property override edges while the complete applied hierarchy is available.
/// Provider-normalized realizations may have distinct identities for the same semantic property;
/// downstream diagnostics consume this edge instead of comparing spellings or target shapes.
fn record_overridden_property_declarations(
    source: &dyn SymbolSource,
    properties: &mut PropertySet,
    intersection_parts: Option<&[Ty]>,
) {
    let declarations = &properties.overloads;
    let recorded = declarations
        .iter()
        .map(|implementation| {
            let mut overridden = implementation.getter.overridden_declarations.to_vec();
            if implementation.getter.external_identity.is_none() {
                return overridden.into_boxed_slice();
            }
            for inherited in declarations {
                if !property_overrides_in_family(
                    source,
                    inherited,
                    implementation,
                    intersection_parts,
                ) {
                    continue;
                }
                let identity = property_declaration_identity(inherited);
                if !overridden.contains(&identity) {
                    overridden.push(identity);
                }
            }
            overridden.into_boxed_slice()
        })
        .collect::<Vec<_>>();
    for (implementation, overridden) in properties.overloads.iter_mut().zip(recorded) {
        implementation.getter.overridden_declarations = overridden;
    }
}

/// Left-to-right depth-first visit order of `root` and its supertypes. The first visit wins in a
/// diamond, matching the order an inherited default expression is chosen.
pub(crate) fn supertype_preorder(
    source: &dyn SymbolSource,
    root: TypeName,
) -> crate::fx_hash::HashMap<TypeName, u32> {
    let mut order = crate::fx_hash::HashMap::default();
    let mut next = 0u32;
    let mut pending = vec![root];
    let mut seen = crate::fx_hash::HashSet::default();
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
        candidate.callable.ret.non_null().kind(),
        TyKind::Obj(..) | TyKind::TyParam(..)
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
            implementation.call_sig.param_defaults = vec![false; parameter_count].into();
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
                implementation.call_sig.param_defaults.set(parameter, true);
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
    let mut extension_seen = crate::fx_hash::HashSet::default();
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
                    .semantic_extension_receiver()
                    .expect("an imported member extension publishes its semantic receiver");
                let mut function = crate::libraries::FunctionInfo::classifier_member(
                    FnKind::Extension,
                    current_owner,
                    member,
                );
                function.receiver = Some(receiver);
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
            projected_supertypes(source, &classifier, current)
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
            ..(*declared).clone()
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
    intersection_parts: Option<&[Ty]>,
) {
    let mut retained: Vec<FunctionInfo> = Vec::with_capacity(functions.overloads.len());
    for candidate in functions.overloads.drain(..) {
        let mut candidate_is_shadowed = false;
        let mut shadowed = Vec::new();
        for (index, existing) in retained.iter().enumerate() {
            let same_provider_slot = existing
                .provider_override_slot
                .as_ref()
                .zip(candidate.provider_override_slot.as_ref())
                .is_some_and(|(existing, candidate)| existing.same_key(candidate));
            let owner_is_interface = |function: &FunctionInfo| {
                source
                    .classifier(function.callable.owner)
                    .is_some_and(|owner| owner.is_interface())
            };
            let candidate_result_refines_existing =
                function_result_refines(source, &candidate, existing);
            let existing_result_refines_candidate =
                function_result_refines(source, existing, &candidate);
            // A provider-normalized concrete class slot implements the matching abstract interface
            // slot even when the interface is reached at a shallower receiver depth. Keep the
            // ordinary rank rule for two classes so a nearer abstract class can still re-abstract
            // a concrete superclass member.
            let candidate_class_implements_interface = same_provider_slot
                && candidate_result_refines_existing
                && !candidate.flags.is_abstract
                && existing.flags.is_abstract
                && !owner_is_interface(&candidate)
                && owner_is_interface(existing);
            let existing_class_implements_interface = same_provider_slot
                && existing_result_refines_candidate
                && !existing.flags.is_abstract
                && candidate.flags.is_abstract
                && !owner_is_interface(existing)
                && owner_is_interface(&candidate);
            if candidate_class_implements_interface {
                shadowed.push(index);
                continue;
            }
            if existing_class_implements_interface {
                candidate_is_shadowed = true;
                break;
            }
            if !function_input_shapes_match(source, existing, &candidate) {
                continue;
            }
            let candidate_is_subtype = candidate_result_refines_existing;
            let existing_is_subtype = existing_result_refines_candidate;
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
            // Walk depth is an override only inside one classifier hierarchy. Intersection
            // components are peers: a direct member of a later component is not an override of
            // a member the earlier component inherits.
            let rank_is_override = intersection_parts.is_none_or(|parts| {
                owners_share_intersection_component(
                    source,
                    parts,
                    candidate.callable.owner,
                    existing.callable.owner,
                )
            });
            let candidate_rank_overrides =
                rank_is_override && candidate.receiver_rank < existing.receiver_rank;
            let existing_rank_overrides =
                rank_is_override && existing.receiver_rank < candidate.receiver_rank;
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
                && intersection_parts
                    .is_none_or(|_| candidate.callable.owner == existing.callable.owner);
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

fn owners_share_intersection_component(
    source: &dyn SymbolSource,
    parts: &[Ty],
    left: TypeName,
    right: TypeName,
) -> bool {
    parts.iter().any(|part| {
        let hierarchy = super::hierarchy_projection::applied_hierarchy(source, *part);
        hierarchy.iter().any(|(owner, _, _)| *owner == left)
            && hierarchy.iter().any(|(owner, _, _)| *owner == right)
    })
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

/// Whether a scope inherits nested classifiers through `supertype`. Kotlin inherits them along the
/// superclass chain only: an interface's nested classifiers reach neither an implementing class nor
/// a sub-interface by simple name (kotlinc 2.4.21 reports `unresolved reference`), whether the
/// interface is Kotlin or Java.
pub(crate) fn inherits_nested_classifiers_through(
    source: &dyn SymbolSource,
    supertype: TypeName,
) -> bool {
    source
        .classifier(supertype)
        .is_none_or(|classifier| !classifier.is_interface())
}

/// The direct supertypes through which `owner` inherits nested classifiers.
pub(crate) fn nested_classifier_supertypes(
    source: &dyn SymbolSource,
    owner: TypeName,
) -> Vec<TypeName> {
    super::direct_supertypes(source, Ty::obj_name(owner))
        .into_iter()
        .filter_map(Ty::kotlin_class_internal)
        .filter(|&supertype| inherits_nested_classifiers_through(source, supertype))
        .collect()
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
    let mut seen = crate::fx_hash::HashSet::default();
    while !level.is_empty() {
        let mut matches = crate::fx_hash::HashSet::default();
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

    fn member(
        owner: &str,
        rank: u32,
        params: Vec<Ty>,
        role: Option<crate::types::SemanticCallRole>,
    ) -> crate::libraries::FunctionInfo {
        let mut callable = crate::libraries::LibraryCallable::library(
            crate::types::type_name(owner),
            "operation",
            params,
            Ty::String,
            Ty::String,
            "()Ljava/lang/String;",
        );
        callable.semantic_role = role;
        let mut function = crate::libraries::FunctionInfo::plain(
            crate::libraries::FnKind::Member,
            Some(Ty::obj(owner)),
            callable,
        );
        function.receiver_rank = rank;
        function
    }

    #[test]
    fn an_override_inherits_its_declarations_semantic_role() {
        use crate::types::SemanticCallRole::KotlinAnyToString;

        let mut functions = FunctionSet {
            overloads: vec![
                member("sample/Derived", 0, Vec::new(), None),
                member("kotlin/Any", 1, Vec::new(), Some(KotlinAnyToString)),
            ],
        };
        inherit_overridden_semantic_roles(
            &crate::libraries::EmptySymbolSource,
            &mut functions,
            None,
        );

        assert_eq!(
            functions.overloads[0].callable.semantic_role,
            Some(KotlinAnyToString)
        );
    }

    fn dependency(
        owner: &str,
        rank: u32,
        params: Vec<Ty>,
        identity: u32,
    ) -> crate::libraries::FunctionInfo {
        let mut function = member(owner, rank, params, None);
        function.callable.external_identity =
            Some(crate::fir::ExternalCallableId::from_raw(identity));
        function
    }

    #[test]
    fn an_override_records_the_exact_declaration_it_overrides() {
        let mut functions = FunctionSet {
            overloads: vec![
                dependency("sample/Derived", 0, Vec::new(), 0),
                dependency("sample/Base", 1, Vec::new(), 1),
                dependency("kotlin/Any", 2, Vec::new(), 2),
            ],
        };
        record_overridden_declarations(&crate::libraries::EmptySymbolSource, &mut functions, None);

        let identity = |owner: &str| crate::types::OverriddenDeclaration {
            owner: crate::types::type_name(owner),
            name: "operation".into(),
            receiver: None,
            params: Box::new([]),
            ret: Ty::String,
        };
        assert_eq!(
            &*functions.overloads[0].callable.overridden_declarations,
            &[identity("sample/Base"), identity("kotlin/Any")]
        );
        assert_eq!(
            &*functions.overloads[1].callable.overridden_declarations,
            &[identity("kotlin/Any")]
        );
        assert!(
            functions.overloads[2]
                .callable
                .overridden_declarations
                .is_empty(),
            "the root declaration overrides nothing"
        );
    }

    #[test]
    fn a_same_named_overload_records_no_overridden_declaration() {
        let mut functions = FunctionSet {
            overloads: vec![
                dependency("sample/Derived", 0, vec![Ty::Int], 0),
                dependency("kotlin/Any", 1, Vec::new(), 1),
            ],
        };
        record_overridden_declarations(&crate::libraries::EmptySymbolSource, &mut functions, None);

        assert!(functions.overloads[0]
            .callable
            .overridden_declarations
            .is_empty());
    }

    #[test]
    fn a_different_overload_does_not_borrow_an_inherited_role() {
        use crate::types::SemanticCallRole::KotlinAnyToString;

        let mut functions = FunctionSet {
            overloads: vec![
                member("sample/Derived", 0, vec![Ty::Int], None),
                member("kotlin/Any", 1, Vec::new(), Some(KotlinAnyToString)),
            ],
        };
        inherit_overridden_semantic_roles(
            &crate::libraries::EmptySymbolSource,
            &mut functions,
            None,
        );

        assert_eq!(functions.overloads[0].callable.semantic_role, None);
    }

    #[test]
    fn callable_kind_is_part_of_an_override_input_shape() {
        let source = crate::libraries::EmptySymbolSource;
        let member = member("sample/Owner", 0, Vec::new(), None);
        let mut top_level = member.clone();
        top_level.kind = crate::libraries::FnKind::TopLevel;
        top_level.receiver = None;

        assert!(function_input_shapes_match(&source, &member, &member));
        assert!(!function_input_shapes_match(&source, &member, &top_level));
        assert!(!function_input_shapes_match(&source, &top_level, &member));
    }

    #[test]
    fn provider_override_slot_matches_opaque_inputs_but_not_callable_kind() {
        let source = crate::libraries::EmptySymbolSource;
        let slot = |owner| crate::libraries::ProviderOverrideSlot::new(owner, 17_u8);
        let mut inherited = member("sample/Interface", 1, vec![Ty::String], None);
        inherited.provider_override_slot = Some(slot(inherited.callable.owner));
        let mut implementation = member(
            "sample/Implementation",
            0,
            vec![Ty::obj("kotlin/Any")],
            None,
        );
        implementation.provider_override_slot = Some(slot(implementation.callable.owner));

        assert!(function_override_input_slots_match(
            &source,
            &inherited,
            &implementation,
        ));

        implementation.kind = crate::libraries::FnKind::TopLevel;
        assert!(!function_override_input_slots_match(
            &source,
            &inherited,
            &implementation,
        ));
    }

    #[test]
    fn generic_result_covariance_compares_declaration_variables_by_ordinal() {
        let generic = |formal: &str| {
            let bound = Ty::nullable(Ty::obj("kotlin/Any"));
            let result = Ty::ty_param(formal, bound);
            let mut function = member("sample/Owner", 0, vec![result], None);
            function.callable.ret = result;
            function.generic_sig = Some(crate::libraries::GenericSig {
                formals: vec![formal.to_string()],
                formal_bounds: vec![Vec::new()],
                receiver: None,
                params: vec![result],
                ret: result,
                return_policy: crate::libraries::GenericReturnPolicy::Exact,
            });
            function
        };
        let inherited = generic("T");
        let implementation = generic("U");

        assert!(function_result_refines(
            &crate::libraries::EmptySymbolSource,
            &implementation,
            &inherited,
        ));
        assert!(function_result_refines(
            &crate::libraries::EmptySymbolSource,
            &inherited,
            &implementation,
        ));
    }

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

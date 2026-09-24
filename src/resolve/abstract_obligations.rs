//! Abstract-member obligations of a concrete class.
//!
//! A class that is not `abstract` must implement every abstract member it inherits. An obligation
//! is discharged by a concrete member of the complete applied hierarchy that overrides it, matched
//! the way kotlinc's override checkers match: a Kotlin declaration by its substituted signature,
//! with a Java platform type equal to either of its bounds, and a Java declaration by erasure
//! inside its own class hierarchy.

use super::{erased_type_key, Checker, ErasedTypeKey};
use crate::libraries::FunctionInfo;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

impl Checker<'_> {
    /// Whether the nearest declaration of every callable in `owner`'s normalized hierarchy is
    /// concrete. Providers expose only direct declarations and direct supertypes; core performs the
    /// complete traversal. The class chain decides before interfaces at every depth (a class-side
    /// abstract declaration suppresses an interface default), while equally-near interface
    /// declarations merge so a concrete default satisfies the shared semantic signature.
    pub(super) fn has_no_unimplemented_abstract_members(&self, owner: TypeName) -> bool {
        type MemberKey = (String, Vec<ErasedTypeKey>);

        let resolver = self.resolver();
        let root = resolver.classifier(owner).map_or_else(
            || Ty::obj_name(owner),
            |classifier| {
                let arguments = classifier
                    .type_params
                    .iter()
                    .enumerate()
                    .map(|(index, parameter)| {
                        Ty::ty_param(
                            parameter,
                            classifier
                                .type_param_bounds
                                .get(index)
                                .and_then(|bounds| bounds.first())
                                .copied()
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                        )
                    })
                    .collect::<Vec<_>>();
                Ty::obj_args_name(owner, &arguments)
            },
        );
        let source = self.fed_source();
        let hierarchy = crate::symbol_resolver::applied_hierarchy(&source, root);
        if hierarchy.is_empty() {
            return false;
        }
        let java_classifiers = hierarchy
            .iter()
            .filter(|(owner, _, _)| {
                source
                    .classifier(*owner)
                    .is_some_and(|classifier| !classifier.is_kotlin)
            })
            .map(|(owner, _, _)| *owner)
            .collect::<Vec<_>>();
        let names = hierarchy
            .iter()
            .filter_map(|(owner, _, _)| source.classifier(*owner))
            .flat_map(|classifier| {
                classifier
                    .declared_callables
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect::<std::collections::HashSet<_>>();
        let families = names
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    crate::symbol_resolver::members_in_hierarchy(&source, root, name),
                )
            })
            .collect::<Vec<_>>();

        // A dependency property accessor can also be exposed as its physical Java method. It is
        // one Kotlin declaration and therefore one obligation: validate the selected semantic
        // property below and do not count its duplicate function facet independently.
        let property_accessor_targets = families
            .iter()
            .flat_map(|(_, callables)| callables.properties())
            .flat_map(|property| {
                std::iter::once(property.getter.external_identity).chain(std::iter::once(
                    property
                        .setter
                        .as_ref()
                        .and_then(|setter| setter.external_identity),
                ))
            })
            .flatten()
            .collect::<std::collections::HashSet<_>>();

        // Interface delegation is a concrete source declaration even though it has no handwritten
        // member AST. Pass 1 has already selected every forwarded declaration and published its
        // exact semantic signature; consume those stable plans while checking subclasses.
        let mut delegated_functions = std::collections::HashSet::<MemberKey>::new();
        let mut delegated_properties = std::collections::HashSet::<String>::new();
        if let Some(index) = self.resolved_index {
            for (classifier, _, _) in &hierarchy {
                let Some(declaration) = index.classifier_declaration(*classifier) else {
                    continue;
                };
                let Some(header) = index.classifier_header(declaration) else {
                    continue;
                };
                for delegation in &header.interface_delegations {
                    for member in &delegation.members {
                        match member {
                            crate::fir::ResolvedDelegatedMember::Function(function) => {
                                delegated_functions.insert((
                                    function.name.to_string(),
                                    function
                                        .overridden
                                        .parameters
                                        .iter()
                                        .map(|parameter| erased_type_key(parameter.get()))
                                        .collect(),
                                ));
                            }
                            crate::fir::ResolvedDelegatedMember::Property(property) => {
                                delegated_properties.insert(property.name.to_string());
                            }
                        }
                    }
                }
            }
        }

        let mut unresolved = std::collections::HashSet::<MemberKey>::new();
        for (name, callables) in &families {
            let functions = callables.functions();
            for function in functions.iter().filter(|function| {
                function.flags.is_abstract
                    && function
                        .callable
                        .external_identity
                        .is_none_or(|identity| !property_accessor_targets.contains(&identity))
            }) {
                if functions.iter().any(|implementation| {
                    !implementation.flags.is_abstract
                        && implements_abstract_function(
                            &source,
                            &java_classifiers,
                            implementation,
                            function,
                        )
                }) {
                    continue;
                }
                let key = (
                    name.clone(),
                    function
                        .semantic_params()
                        .iter()
                        .copied()
                        .map(erased_type_key)
                        .collect(),
                );
                if !delegated_functions.contains(&key) {
                    unresolved.insert(key);
                }
            }
            // Abstract-property obligations are accessor capabilities over the complete applied
            // hierarchy. Selecting one property is order-sensitive when an interface contributes
            // an abstract accessor and a superclass contributes its concrete implementation (for
            // example `CoroutineContext.Element.key` via
            // `AbstractCoroutineContextElement`). A concrete getter discharges a getter
            // obligation; a mutable property's setter is tracked independently.
            let mut requires_getter = false;
            let mut requires_setter = false;
            let mut has_concrete_getter = false;
            let mut has_concrete_setter = false;
            for property in callables.properties() {
                requires_getter |= property.getter.is_abstract;
                has_concrete_getter |= !property.getter.is_abstract;
                if let Some(setter) = property.setter.as_ref() {
                    requires_setter |= setter.is_abstract;
                    has_concrete_setter |= !setter.is_abstract;
                }
            }
            let delegated = delegated_properties.contains(name);
            if (requires_getter && !has_concrete_getter && !delegated)
                || (requires_setter && !has_concrete_setter && !delegated)
            {
                unresolved.insert((name.clone(), Vec::new()));
            }
        }
        crate::trace_compiler!(
            "resolve",
            "abstract obligations owner={} unresolved={unresolved:?}",
            owner.render(),
        );
        unresolved.is_empty()
    }
}

/// Whether the concrete `implementation` overrides the same-named abstract `declaration`. Both come
/// from one applied hierarchy, so their parameter types are already substituted for the class
/// being checked.
fn implements_abstract_function(
    source: &dyn SymbolSource,
    java_classifiers: &[TypeName],
    implementation: &FunctionInfo,
    declaration: &FunctionInfo,
) -> bool {
    if implementation.flags.suspend != declaration.flags.suspend
        || implementation.context_count != declaration.context_count
    {
        return false;
    }
    let implementation_params = override_shape(implementation);
    let declaration_params = override_shape(declaration);
    implementation_params.len() == declaration_params.len()
        && (implementation_params
            .iter()
            .zip(&declaration_params)
            .all(|(&left, &right)| crate::assignable::same_flexible_type(left, right))
            || java_scope_implements(source, java_classifiers, implementation, declaration))
}

/// Semantic parameter types with declaration-owned type parameters named by their ordinal, so two
/// independently declared `<T>` occupy the same slot.
fn override_shape(function: &FunctionInfo) -> Vec<Ty> {
    let formals = function
        .generic_sig
        .as_ref()
        .map(|signature| signature.formals.as_slice())
        .unwrap_or_default();
    function
        .semantic_params()
        .iter()
        .copied()
        .map(|parameter| crate::types::ty_canonicalize_params(parameter, formals))
        .collect()
}

/// Java members override by erasure (JLS §8.4.2), and kotlinc's Java class scope merges inherited
/// members with its Java override checker: in `java.util.AbstractList<E>`, the inherited
/// `AbstractCollection.contains(Object)` implements `List<E>.contains(E)`, although the substituted
/// Kotlin signatures differ. The two members meet in such a scope only when one Java classifier of
/// the hierarchy inherits both of their declaring classifiers; elsewhere the Kotlin rule applies.
fn java_scope_implements(
    source: &dyn SymbolSource,
    java_classifiers: &[TypeName],
    implementation: &FunctionInfo,
    declaration: &FunctionInfo,
) -> bool {
    // A member is specialized against the hierarchy rung that declares it; its callable owner is
    // the dispatch owner, which for a mapped builtin is the JVM class rather than the declaration.
    let declaring = |function: &FunctionInfo| {
        function
            .receiver
            .and_then(|receiver| receiver.kotlin_class_internal())
    };
    let (Some(implementation_owner), Some(declaration_owner)) =
        (declaring(implementation), declaring(declaration))
    else {
        return false;
    };
    let inherits = |classifier: TypeName, owner: TypeName| {
        classifier == owner
            || crate::symbol_resolver::resolution_subtype(
                source,
                Ty::obj_name(classifier),
                Ty::obj_name(owner),
            )
    };
    implementation.callable.physical_params.len() == declaration.callable.physical_params.len()
        && implementation
            .callable
            .physical_params
            .iter()
            .zip(&declaration.callable.physical_params)
            .all(|(&left, &right)| java_erasure(left) == java_erasure(right))
        && java_classifiers.iter().any(|&classifier| {
            inherits(classifier, implementation_owner) && inherits(classifier, declaration_owner)
        })
}

/// Erasure of a declared parameter type. A Java platform type erases like its nullable bound, so a
/// wrapper class stays boxed while a primitive stays primitive.
fn java_erasure(ty: Ty) -> ErasedTypeKey {
    match ty {
        Ty::PlatformNullable(inner) => erased_type_key(Ty::nullable(*inner)),
        _ => erased_type_key(ty),
    }
}

//! Abstract-member obligations of a concrete class.
//!
//! A class that is not `abstract` must implement every abstract member it inherits. An obligation
//! is discharged by a concrete member of the complete applied hierarchy that overrides it, matched
//! the way kotlinc's override checkers match: ordinary declarations by their substituted semantic
//! signatures, with platform-specific override equivalence supplied as a normalized provider slot.

use super::Checker;
use crate::libraries::{FunctionInfo, LibraryType, PropertyInfo};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// An abstract member a concrete class inherits and does not implement, as its supertype declares
/// it: unsubstituted, with the supertype's own type parameters.
pub(super) struct UnimplementedMember {
    pub(super) declaration: UnimplementedDeclaration,
    /// Whether an interface declares it, rather than a class.
    pub(super) in_interface: bool,
    /// kotlinc's listing order: the declaring supertype's place in the hierarchy, then functions
    /// before properties, each in declaration order.
    order: (usize, bool, usize, usize),
}

pub(super) enum UnimplementedDeclaration {
    Function(Box<FunctionInfo>),
    Property(Box<PropertyInfo>),
}

/// A declaration-owned upper-bound set. Bound order is source presentation, not override-slot
/// identity; duplicates likewise do not create a different semantic constraint set.
#[derive(Clone, Debug, Eq)]
struct SemanticBoundSet(Vec<Ty>);

impl SemanticBoundSet {
    fn new(bounds: impl IntoIterator<Item = Ty>) -> Self {
        let mut unique = Vec::new();
        for bound in bounds {
            if !unique.contains(&bound) {
                unique.push(bound);
            }
        }
        Self(unique)
    }
}

/// Normalize a declaration-owned bound slot for override identity. Providers publish an absent
/// Kotlin bound either as an empty slot or as the explicit semantic default `Any?`; those are the
/// same constraint and must therefore occupy the same override slot.
fn semantic_bound_set(
    bounds: impl IntoIterator<Item = Ty>,
    normalize: &impl Fn(Ty) -> Ty,
) -> SemanticBoundSet {
    let mut bounds = bounds.into_iter().peekable();
    if bounds.peek().is_none() {
        SemanticBoundSet::new([Ty::nullable(Ty::obj("kotlin/Any"))])
    } else {
        SemanticBoundSet::new(bounds.map(normalize))
    }
}

impl PartialEq for SemanticBoundSet {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.0.iter().all(|bound| other.0.contains(bound))
    }
}

impl Hash for SemanticBoundSet {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let mut hashes = self
            .0
            .iter()
            .map(|bound| {
                let mut hasher = crate::fx_hash::FxHasher::default();
                bound.hash(&mut hasher);
                hasher.finish()
            })
            .collect::<Vec<_>>();
        hashes.sort_unstable();
        hashes.hash(state);
    }
}

fn neutral_formal_bounds(formals: &[String]) -> crate::fx_hash::HashMap<String, Ty> {
    formals
        .iter()
        .cloned()
        .map(|formal| (formal, Ty::nullable(Ty::obj("kotlin/Any"))))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FunctionKey {
    name: String,
    suspend: bool,
    context_count: usize,
    formal_count: usize,
    formal_bounds: Vec<SemanticBoundSet>,
    receiver: Option<Ty>,
    parameters: Vec<Ty>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PropertyKey {
    name: String,
    formal_count: usize,
    formal_bounds: Vec<SemanticBoundSet>,
    receiver: Option<Ty>,
    context_parameters: Vec<Ty>,
}

fn function_key(function: &FunctionInfo) -> FunctionKey {
    let signature = function.generic_sig.as_ref();
    let formals = signature
        .map(|signature| signature.formals.as_slice())
        .unwrap_or_default();
    let neutral_bounds = neutral_formal_bounds(formals);
    let normalize = |ty| {
        crate::types::ty_canonicalize_params(
            crate::types::ty_with_param_bounds(ty, &neutral_bounds),
            formals,
        )
    };
    FunctionKey {
        name: function.callable.name.clone(),
        suspend: function.flags.suspend,
        context_count: function.context_count,
        formal_count: formals.len(),
        formal_bounds: signature
            .map(|signature| {
                (0..formals.len())
                    .map(|index| {
                        semantic_bound_set(
                            signature
                                .formal_bounds
                                .get(index)
                                .into_iter()
                                .flatten()
                                .copied(),
                            &normalize,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        receiver: function
            .is_extension()
            .then(|| function.semantic_receiver())
            .flatten()
            .map(normalize),
        parameters: function
            .semantic_params()
            .iter()
            .copied()
            .map(normalize)
            .collect(),
    }
}

fn delegated_function_key(
    function: &crate::fir::ResolvedDelegatedFunction,
    declaration: &crate::fir::ResolvedDelegatedFunctionDeclaration,
) -> FunctionKey {
    let formals = function
        .type_parameters
        .iter()
        .map(|parameter| parameter.semantic_name.to_string())
        .collect::<Vec<_>>();
    let neutral_bounds = neutral_formal_bounds(&formals);
    let normalize = |ty| {
        crate::types::ty_canonicalize_params(
            crate::types::ty_with_param_bounds(ty, &neutral_bounds),
            &formals,
        )
    };
    let mut parameters = declaration
        .applied_parameters
        .iter()
        .map(|parameter| normalize(parameter.get()))
        .collect::<Vec<_>>();
    let receiver = function
        .call
        .extension_receiver_parameter
        .map(|index| index as usize)
        .map(|index| parameters.remove(index));
    FunctionKey {
        name: function.name.to_string(),
        suspend: function.call.suspend,
        context_count: declaration.context_count,
        formal_count: formals.len(),
        formal_bounds: function
            .type_parameters
            .iter()
            .map(|parameter| {
                semantic_bound_set(parameter.bounds.iter().map(|bound| bound.get()), &normalize)
            })
            .collect(),
        receiver,
        parameters,
    }
}

fn property_key(property: &PropertyInfo) -> PropertyKey {
    let normalize = |ty| normalized_property_slot_type(property, ty);
    PropertyKey {
        name: property.name.clone(),
        formal_count: property.formals.len(),
        formal_bounds: property
            .getter
            .generic_sig
            .as_ref()
            .map(|signature| {
                (0..property.formals.len())
                    .map(|index| {
                        semantic_bound_set(
                            signature
                                .formal_bounds
                                .get(index)
                                .into_iter()
                                .flatten()
                                .copied(),
                            &normalize,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        receiver: (property.kind == crate::libraries::PropKind::MemberExtension)
            .then_some(property.receiver)
            .flatten()
            .map(normalize),
        context_parameters: property
            .getter
            .params
            .iter()
            .take(property.context_count)
            .copied()
            .map(normalize)
            .collect(),
    }
}

/// One property input under alpha-normalized declaration variables. A slot key records declared
/// bounds separately, so inline occurrence bounds are neutralized before comparing the input shape.
fn normalized_property_slot_type(property: &PropertyInfo, ty: Ty) -> Ty {
    let neutral_bounds = neutral_formal_bounds(&property.formals);
    crate::types::ty_canonicalize_params(
        crate::types::ty_with_param_bounds(ty, &neutral_bounds),
        &property.formals,
    )
}

/// One property result under alpha-normalized declaration variables. Unlike a slot input, its
/// complete declaration-owned bound remains part of the semantic type: both the primary bound and
/// additional `where` bounds may supply the subtype proof that selects the covariant result.
fn normalized_property_result_type(property: &PropertyInfo) -> Ty {
    let default_bound = Ty::nullable(Ty::obj("kotlin/Any"));
    let signature = property.getter.generic_sig.as_deref();
    let bounds = property
        .formals
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let declared = signature
                .expect("generic property has an accessor generic signature")
                .formal_bounds
                .get(index)
                .expect("generic property bounds are parallel to its formals")
                .as_slice();
            let bound = match declared {
                [] => default_bound,
                [bound] => *bound,
                bounds => Ty::intersection(bounds),
            };
            (formal.clone(), bound)
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();
    crate::types::ty_canonicalize_params(
        crate::types::ty_with_transitive_param_bounds(property.ty, &bounds),
        &property.formals,
    )
}

fn delegated_property_key(property: &crate::fir::ResolvedDelegatedProperty) -> PropertyKey {
    let formals = property
        .type_parameters
        .iter()
        .map(|parameter| parameter.semantic_name.to_string())
        .collect::<Vec<_>>();
    let neutral_bounds = neutral_formal_bounds(&formals);
    let normalize = |ty| {
        crate::types::ty_canonicalize_params(
            crate::types::ty_with_param_bounds(ty, &neutral_bounds),
            &formals,
        )
    };
    let receiver = property
        .getter
        .extension_receiver_parameter
        .map(|index| normalize(property.getter.parameters[index as usize].get()));
    PropertyKey {
        name: property.name.to_string(),
        formal_count: formals.len(),
        formal_bounds: property
            .type_parameters
            .iter()
            .map(|parameter| {
                semantic_bound_set(parameter.bounds.iter().map(|bound| bound.get()), &normalize)
            })
            .collect(),
        receiver,
        context_parameters: property
            .context_parameters
            .iter()
            .map(|parameter| normalize(parameter.ty.get()))
            .collect(),
    }
}

impl Checker<'_> {
    /// The abstract members of `owner`'s normalized hierarchy whose nearest declaration is not
    /// implemented, in hierarchy and declaration order.
    /// Providers expose only direct declarations and direct supertypes; core performs the complete
    /// traversal. The class chain decides before interfaces at every depth (a class-side abstract
    /// declaration suppresses an interface default), while equally-near interface declarations
    /// merge so a concrete default satisfies the shared semantic signature.
    pub(super) fn unimplemented_abstract_members(
        &self,
        owner: TypeName,
    ) -> Vec<UnimplementedMember> {
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
        let hierarchy_classifiers = hierarchy
            .iter()
            .map(|(owner, _, _)| *owner)
            .collect::<Vec<_>>();
        // Nearest first, and at each depth the class chain before interfaces: the classifier
        // that declares an unimplemented member is the first of these that declares it.
        let mut classifiers = hierarchy
            .iter()
            .filter_map(|(owner, _, depth)| Some((*depth, source.classifier(*owner)?)))
            .collect::<Vec<_>>();
        classifiers.sort_by_key(|(depth, classifier)| (*depth, classifier.is_interface()));
        let classifiers = classifiers
            .into_iter()
            .map(|(_, classifier)| classifier)
            .collect::<Vec<_>>();
        let names = classifiers
            .iter()
            .flat_map(|classifier| {
                classifier.declared_callables.keys().chain(
                    classifier
                        .members
                        .iter()
                        .filter(|member| member.is_member_extension())
                        .map(|member| &member.name),
                )
            })
            .collect::<crate::fx_hash::HashSet<_>>();
        let families = names
            .iter()
            .map(|name| {
                (
                    (*name).clone(),
                    crate::symbol_resolver::member_declarations_in_hierarchy(&source, root, name),
                )
            })
            .collect::<Vec<_>>();
        // Preserve normalized declaration views across callable spellings. A provider may expose
        // one declaration as both a function and a property; its recorded override edge, not either
        // spelling, joins those views.
        let normalized_functions = families
            .iter()
            .flat_map(|(_, callables)| callables.functions())
            .collect::<Vec<_>>();
        let normalized_properties = families
            .iter()
            .flat_map(|(_, callables)| callables.properties())
            .collect::<Vec<_>>();

        // A dependency property accessor can also have a function facet. It is one semantic
        // declaration and therefore one obligation: validate the selected property below and do
        // not count its duplicate function facet independently.
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
            .collect::<crate::fx_hash::HashSet<_>>();

        // Interface delegation is a concrete source declaration even though it has no handwritten
        // member AST. Pass 1 has already selected every forwarded declaration and published its
        // exact semantic signature; consume those stable plans while checking subclasses.
        let mut delegated_functions = crate::fx_hash::HashSet::<FunctionKey>::default();
        let mut delegated_properties = crate::fx_hash::HashSet::<PropertyKey>::default();
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
                                for overridden in &function.overridden {
                                    delegated_functions
                                        .insert(delegated_function_key(function, overridden));
                                }
                            }
                            crate::fir::ResolvedDelegatedMember::Property(property) => {
                                delegated_properties.insert(delegated_property_key(property));
                            }
                        }
                    }
                }
            }
        }

        // A body-local class's methods are an overlay its walk publishes, not part of the stable
        // providers: union its own methods as implementations. A method whose result its body
        // infers implements an obligation with the result its checked body recorded; the walk
        // checks every method before it asks.
        let local_implementations = if self.checked_local_methods.contains_key(&owner) {
            families
                .iter()
                .map(|(name, _)| {
                    let (candidates, _) = self.body_local_declared_member_candidates_at(root, name);
                    let candidates = candidates
                        .into_iter()
                        .filter_map(|mut candidate| {
                            let generic_pending = candidate
                                .generic_sig
                                .as_ref()
                                .is_some_and(|generic| generic.ret == Ty::Pending);
                            if candidate.callable.ret == Ty::Pending || generic_pending {
                                let result = self.checked_source_member_result(
                                    candidate.source_member,
                                    candidate.stable_declaration,
                                )?;
                                if candidate.callable.ret == Ty::Pending {
                                    candidate.callable.ret = result;
                                }
                                if let Some(generic) = candidate
                                    .generic_sig
                                    .as_mut()
                                    .filter(|generic| generic.ret == Ty::Pending)
                                {
                                    generic.ret = result;
                                }
                            }
                            Some(candidate)
                        })
                        .collect::<Vec<_>>();
                    (name.clone(), candidates)
                })
                .collect::<crate::fx_hash::HashMap<_, _>>()
        } else {
            crate::fx_hash::HashMap::default()
        };

        let mut unresolved = Vec::<UnimplementedMember>::new();
        let mut unresolved_functions = Vec::<(&FunctionInfo, FunctionKey)>::new();
        for (name, callables) in &families {
            let functions = callables.functions();
            let local = local_implementations
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for function in functions.iter().filter(|function| {
                function.flags.is_abstract
                    && function
                        .callable
                        .external_identity
                        .is_none_or(|identity| !property_accessor_targets.contains(&identity))
            }) {
                if functions.iter().chain(local).any(|implementation| {
                    !implementation.flags.is_abstract
                        && implements_abstract_function(
                            &source,
                            self.libraries,
                            &hierarchy_classifiers,
                            implementation,
                            function,
                        )
                }) {
                    continue;
                }
                let key = function_key(function);
                if delegated_functions.contains(&key)
                    || unresolved_functions.iter().any(|(existing, existing_key)| {
                        existing_key == &key
                            || (existing.callable.name == function.callable.name
                                && existing.flags.suspend == function.flags.suspend
                                && existing.context_count == function.context_count
                                && existing.is_extension() == function.is_extension()
                                && provider_slot_implements(
                                    &source,
                                    self.libraries,
                                    &hierarchy_classifiers,
                                    existing,
                                    function,
                                ))
                    })
                {
                    continue;
                }
                unresolved_functions.push((function, key));
                let member = declared_function(&classifiers, function);
                unresolved.push(member);
            }
            // Abstract-property obligations are accessor capabilities over the complete applied
            // hierarchy. Selecting one property is order-sensitive when an interface contributes
            // an abstract accessor and a superclass contributes its concrete implementation (for
            // example `CoroutineContext.Element.key` via
            // `AbstractCoroutineContextElement`). A concrete getter discharges a getter
            // obligation; a mutable property's setter is tracked independently.
            let mut property_slots = Vec::<(PropertyKey, Vec<&PropertyInfo>)>::new();
            for property in callables.properties() {
                let key = property_key(property);
                if let Some((_, declarations)) = property_slots
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == key)
                {
                    declarations.push(property);
                } else {
                    property_slots.push((key, vec![property]));
                }
            }
            for (key, declarations) in property_slots {
                let requires_getter = declarations
                    .iter()
                    .any(|property| property.getter.is_abstract);
                let has_concrete_getter = declarations
                    .iter()
                    .any(|property| !property.getter.is_abstract);
                let requires_setter = declarations.iter().any(|property| {
                    property
                        .setter
                        .as_ref()
                        .is_some_and(|setter| setter.is_abstract)
                });
                let has_concrete_setter = declarations.iter().any(|property| {
                    property
                        .setter
                        .as_ref()
                        .is_some_and(|setter| !setter.is_abstract)
                });
                let missing_getter = requires_getter && !has_concrete_getter;
                let missing_setter = requires_setter && !has_concrete_setter;
                if (!missing_getter && !missing_setter) || delegated_properties.contains(&key) {
                    continue;
                }
                let obligation = if !missing_getter {
                    declarations.iter().copied().find(|property| {
                        property
                            .setter
                            .as_ref()
                            .is_some_and(|setter| setter.is_abstract)
                    })
                } else {
                    let abstract_getters = declarations
                        .iter()
                        .copied()
                        .filter(|property| property.getter.is_abstract)
                        .collect::<Vec<_>>();
                    // A covariant fake override denotes the uniquely most-specific result, not
                    // whichever supertype happened to be traversed first. Equal results remain one
                    // slot and retain hierarchy order; incomparable results are diagnosed as an
                    // inheritance conflict before a concrete-class obligation is rendered.
                    let most_specific = abstract_getters
                        .iter()
                        .copied()
                        .filter(|candidate| {
                            abstract_getters.iter().all(|other| {
                                crate::symbol_resolver::resolution_subtype(
                                    &source,
                                    normalized_property_result_type(candidate),
                                    normalized_property_result_type(other),
                                )
                            })
                        })
                        .collect::<Vec<_>>();
                    // When equally specific declarations contribute `val` and `var`, the
                    // combined fake override needs both accessors. Render the declaration that
                    // owns the missing setter; hierarchy order decides only after accessor shape.
                    most_specific
                        .iter()
                        .copied()
                        .find(|property| {
                            missing_setter
                                && property
                                    .setter
                                    .as_ref()
                                    .is_some_and(|setter| setter.is_abstract)
                        })
                        .or_else(|| most_specific.first().copied())
                };
                if let Some(obligation) = obligation {
                    unresolved.push(declared_property(
                        &classifiers,
                        &normalized_functions,
                        &normalized_properties,
                        obligation,
                        missing_setter,
                    ));
                }
            }
        }
        crate::trace_compiler!(
            "resolve",
            "abstract obligations owner={} unresolved={:?}",
            owner.render(),
            unresolved.len(),
        );
        let mut members = unresolved;
        members.sort_by_key(|member| member.order);
        members
    }
}

/// The declaration an unimplemented, hierarchy-specialized `function` was specialized from: the
/// nearest classifier's own abstract function with the same stable declaration identity.
fn declared_function(
    classifiers: &[Arc<LibraryType>],
    function: &FunctionInfo,
) -> UnimplementedMember {
    classifiers
        .iter()
        .enumerate()
        .find_map(|(place, classifier)| {
            let (overload, declaration) = classifier
                .declared_callables
                .get(&function.callable.name)?
                .functions()
                .iter()
                .enumerate()
                .find(|(_, declaration)| {
                    declaration.flags.is_abstract
                        && super::declaration_rendering::same_function_declaration(
                            declaration,
                            function,
                        )
                })?;
            Some(UnimplementedMember {
                declaration: UnimplementedDeclaration::Function(Box::new(declaration.clone())),
                in_interface: classifier.is_interface(),
                order: (
                    place,
                    false,
                    declaration_order(classifier, &function.callable.name),
                    overload,
                ),
            })
        })
        // A declaration no classifier lists directly (a provider-synthesized member) is shown as
        // the hierarchy presents it, after every directly declared one.
        .unwrap_or_else(|| UnimplementedMember {
            declaration: UnimplementedDeclaration::Function(Box::new(function.clone())),
            in_interface: false,
            order: (classifiers.len(), false, 0, 0),
        })
}

/// Where `name` stands among `classifier`'s declarations: its recorded source order, or after them
/// in sorted order for a provider that records none.
fn declaration_order(classifier: &LibraryType, name: &str) -> usize {
    classifier
        .declared_callable_order
        .iter()
        .position(|declared| declared == name)
        .unwrap_or_else(|| {
            classifier.declared_callable_order.len()
                + classifier
                    .declared_callables
                    .keys()
                    .filter(|declared| declared.as_str() < name)
                    .count()
        })
}

/// The nearest declaration of the abstract property `obligation` (for a missing setter, of its
/// setter). A provider may declare a normalized property through an accessor method alone, so that
/// method places the obligation on its class while the property renders from the nearest
/// classifier that declares it.
fn declared_property(
    classifiers: &[Arc<LibraryType>],
    normalized_functions: &[&FunctionInfo],
    normalized_properties: &[&PropertyInfo],
    obligation: &PropertyInfo,
    missing_setter: bool,
) -> UnimplementedMember {
    let name = obligation.name.as_str();
    let abstract_property = |declaration: &PropertyInfo| {
        if missing_setter {
            declaration
                .setter
                .as_ref()
                .is_some_and(|setter| setter.is_abstract)
        } else {
            declaration.getter.is_abstract
        }
    };
    let exact_obligation = classifiers
        .iter()
        .filter_map(|classifier| classifier.declared_callables.get(name))
        .flat_map(|callables| callables.properties())
        .find(|declaration| {
            abstract_property(declaration)
                && super::declaration_rendering::same_property_declaration(declaration, obligation)
        })
        // A provider-synthesized property has no distinct direct declaration; in that case its
        // normalized view is itself the declaration boundary.
        .unwrap_or(obligation);
    let accessor = if missing_setter {
        obligation
            .setter
            .as_ref()
            .and_then(|setter| setter.external_identity)
    } else {
        obligation.getter.external_identity
    };
    let obligation_identity =
        crate::symbol_resolver::property_declaration_identity(exact_obligation);
    let obligation_key = property_key(exact_obligation);
    let obligation_result = normalized_property_result_type(exact_obligation);
    let same_slot_and_result = |property: &PropertyInfo| {
        property_key(property) == obligation_key
            && crate::assignable::same_flexible_type(
                normalized_property_result_type(property),
                obligation_result,
            )
    };
    let normalized_property_accessors = normalized_properties
        .iter()
        .copied()
        .filter(|property| {
            same_slot_and_result(property)
                || super::declaration_rendering::property_declares_obligation(
                    property,
                    exact_obligation,
                )
        })
        .filter_map(|property| {
            if missing_setter {
                property
                    .setter
                    .as_ref()
                    .and_then(|setter| setter.external_identity)
            } else {
                property.getter.external_identity
            }
        })
        .collect::<crate::fx_hash::HashSet<_>>();
    let normalized_accessors = normalized_functions
        .iter()
        .copied()
        .filter(|function| {
            function.flags.is_abstract
                && (accessor
                    .is_some_and(|accessor| function.callable.external_identity == Some(accessor))
                    || function
                        .callable
                        .overridden_declarations
                        .contains(&obligation_identity))
        })
        .collect::<Vec<_>>();
    let declared = |classifier: &LibraryType| {
        classifier
            .declared_callables
            .get(name)
            .and_then(|callables| {
                callables
                    .properties()
                    .iter()
                    .enumerate()
                    .find(|(_, declaration)| {
                        abstract_property(declaration)
                            && (same_slot_and_result(declaration)
                                || normalized_properties.iter().any(|normalized| {
                                    super::declaration_rendering::same_property_declaration(
                                        declaration,
                                        normalized,
                                    ) && super::declaration_rendering::property_declares_obligation(
                                        normalized,
                                        exact_obligation,
                                    )
                                }))
                    })
            })
            .map(|(overload, declaration)| (overload, declaration.clone()))
    };
    let declares_accessor =
        |classifier: &LibraryType| {
            classifier.declared_callables.values().any(|callables| {
                callables.functions().iter().any(|function| {
                    function.flags.is_abstract
                        && (function.callable.external_identity.is_some_and(|identity| {
                            normalized_property_accessors.contains(&identity)
                        }) || normalized_accessors.iter().any(|normalized| {
                            super::declaration_rendering::same_function_declaration(
                                function, normalized,
                            )
                        }))
                })
            })
        };
    let nearest = classifiers
        .iter()
        .enumerate()
        .find(|(_, classifier)| declared(classifier).is_some() || declares_accessor(classifier));
    let declaration = classifiers
        .iter()
        .find_map(|classifier| declared(classifier))
        .map_or_else(|| obligation.clone(), |(_, declaration)| declaration);
    let (place, declaring_interface, position, overload) = match nearest {
        Some((place, classifier)) => (
            place,
            classifier.is_interface(),
            declaration_order(classifier, name),
            declared(classifier).map_or(0, |(overload, _)| overload),
        ),
        None => (classifiers.len(), false, 0, 0),
    };
    UnimplementedMember {
        declaration: UnimplementedDeclaration::Property(Box::new(declaration)),
        // A fake override does not redeclare its interface accessor in the abstract class that
        // merely inherits it. The nearest direct declaration/accessor remains the diagnostic owner.
        in_interface: declaring_interface,
        order: (place, true, position, overload),
    }
}

/// Whether the concrete `implementation` overrides the same-named abstract `declaration`. Both come
/// from one applied hierarchy, so their parameter types are already substituted for the class
/// being checked.
fn implements_abstract_function(
    source: &dyn SymbolSource,
    platform: &dyn crate::libraries::SemanticPlatform,
    hierarchy_classifiers: &[TypeName],
    implementation: &FunctionInfo,
    declaration: &FunctionInfo,
) -> bool {
    if implementation.flags.suspend != declaration.flags.suspend
        || implementation.context_count != declaration.context_count
    {
        return false;
    }
    let ordinary_implementation = implementation.kind == declaration.kind
        && (crate::symbol_resolver::function_input_shapes_match(
            source,
            declaration,
            implementation,
        ) || flexible_function_keys_match(implementation, declaration));
    (ordinary_implementation
        && crate::symbol_resolver::function_result_refines(source, implementation, declaration))
        || provider_slot_implements(
            source,
            platform,
            hierarchy_classifiers,
            implementation,
            declaration,
        )
}

/// Flexible semantic types admit either bound in an override slot. Keep declaration-owned generic
/// shape and member-extension receivers exact while applying that flexibility to occurrence types.
fn flexible_function_keys_match(left: &FunctionInfo, right: &FunctionInfo) -> bool {
    let left = function_key(left);
    let right = function_key(right);
    let same_optional_type = |left: Option<Ty>, right: Option<Ty>| match (left, right) {
        (Some(left), Some(right)) => crate::assignable::same_flexible_type(left, right),
        (None, None) => true,
        _ => false,
    };
    left.name == right.name
        && left.suspend == right.suspend
        && left.context_count == right.context_count
        && left.formal_count == right.formal_count
        && left.formal_bounds == right.formal_bounds
        && same_optional_type(left.receiver, right.receiver)
        && left.parameters.len() == right.parameters.len()
        && left
            .parameters
            .iter()
            .zip(&right.parameters)
            .all(|(&left, &right)| crate::assignable::same_flexible_type(left, right))
}

/// A platform provider may publish an override slot whose normalized inputs differ from the
/// declarations' source-language signatures. The common hierarchy compares that explicit fact and
/// ordinary owner relationships; it does not reconstruct target erasure or inspect declaration
/// origin.
fn provider_slot_implements(
    source: &dyn SymbolSource,
    platform: &dyn crate::libraries::SemanticPlatform,
    hierarchy_classifiers: &[TypeName],
    implementation: &FunctionInfo,
    declaration: &FunctionInfo,
) -> bool {
    // Provider slots normalize a declaration's input representation; they do not change its
    // Kotlin callable kind. In particular, a member-extension receiver must never satisfy an
    // ordinary member parameter merely because both happen to occupy one provider ABI slot.
    if implementation.kind != declaration.kind {
        return false;
    }
    let Some(declaration_scope) = declaration.provider_override_slot.as_ref() else {
        return false;
    };
    let declaration_owner = declaration_scope.owner();
    let inherits = |classifier: TypeName, owner: TypeName| {
        classifier == owner
            || crate::symbol_resolver::resolution_subtype(
                source,
                Ty::obj_name(classifier),
                Ty::obj_name(owner),
            )
    };
    // Provider slots normalize dependency declarations at their provider boundary. A source
    // declaration has already been resolved in Kotlin's semantic model and must never be
    // reinterpreted here through a target representation (for example, by treating a member
    // extension receiver as an ordinary JVM parameter).
    let Some(implementation_scope) = implementation.provider_override_slot.as_ref() else {
        return false;
    };
    let implementation_owner = implementation_scope.owner();
    // One provider scope can itself be the join. A mapped semantic hierarchy can expose a
    // concrete declaration whose provider owner already inherits the abstract slot owner; no
    // origin-specific parallel hierarchy is needed to rediscover that relation. Conversely, a
    // nearer abstract declaration can re-abstract a concrete superclass member.
    if implementation_owner != declaration_owner
        && inherits(declaration_owner, implementation_owner)
    {
        return false;
    }
    let shares_provider_scope = inherits(implementation_owner, declaration_owner)
        || hierarchy_classifiers.iter().any(|&classifier| {
            inherits(classifier, implementation_owner) && inherits(classifier, declaration_owner)
        });
    if !implementation_scope.same_key(declaration_scope) || !shares_provider_scope {
        return false;
    }
    let implementation_result = implementation.ret.apply(implementation.callable.ret);
    let declaration_result = declaration.ret.apply(declaration.callable.ret);
    crate::symbol_resolver::function_result_refines(source, implementation, declaration)
        || platform.provider_override_result_refines(
            source,
            implementation_result,
            declaration_result,
        )
}

//! The dispatch shape of every named class of the module, read from its declaration headers.
//!
//! A target that numbers vtable slots itself has to give a class the same numbers in every file
//! that dispatches through it, and only the declaring file has the class's IR. So the numbering is
//! computed from what every file can see alike: the stable headers of the module, frozen here
//! before Pass 2 starts. Each class records its superclass, its instance members in source order
//! with the carriers common lowering gives them ([`ResolvedModuleIndex::callable_carriers`]), and
//! the override edges the frontend selected for them. Every named interface records the same, plus
//! the interfaces it extends, so its members can be numbered once for the module. Nothing here is a
//! target decision.

use crate::fx_hash::{HashMap, HashSet};

use crate::fir::{
    CallableId, DeclarationFlags, DeclarationId, DeclarationKind, PropertyId,
    ResolvedDelegatedMember, ResolvedFunctionOverrideTarget, ResolvedInterfaceDelegation,
    ResolvedModuleIndex, ResolvedPropertyOverrideTarget,
};
use crate::types::{SemanticCallRole, Ty, TypeName};

#[cfg(test)]
#[path = "module_classes/forwarder_tests.rs"]
mod forwarder_tests;

/// The named classes of one module, by identity.
#[derive(Clone, Debug, Default)]
pub struct ModuleClasses {
    classes: HashMap<TypeName, ModuleClass>,
    /// In declaration order, so numbering them visits them the same way in every process.
    interfaces: Vec<(TypeName, ModuleInterface)>,
}

/// One named class: what its dispatch table is built from.
#[derive(Clone, Debug)]
pub struct ModuleClass {
    /// The declared superclass, or `None` for `kotlin.Any`.
    pub(crate) superclass: Option<TypeName>,
    /// A dependency class a module class extends, whose members are its provider's
    /// ([`crate::fir::ResolvedDependencySuperclass`]).
    pub(crate) dependency: bool,
    /// Function types among the declared supertypes, by arity.
    pub(crate) function_type_arities: Box<[usize]>,
    /// Instance members in source order.
    pub(crate) members: Box<[ModuleMember]>,
    pub(crate) function_overrides: Box<[ModuleFunctionOverride]>,
    pub(crate) property_overrides: Box<[ModulePropertyOverride]>,
    /// The forwarders its interface delegations (`I by d`) generate, in the order the declaring
    /// file creates them: delegation by delegation, each member's function, or getter then
    /// setter.
    pub(crate) forwarders: Box<[ModuleForwarder]>,
}

/// One forwarder an interface delegation generates.
#[derive(Debug)]
pub(crate) struct ModuleForwarder {
    /// What it takes after the dispatch receiver and answers.
    pub(crate) parameters: Box<[Ty]>,
    pub(crate) result: Ty,
    /// A `suspend` function, which takes its caller's continuation after `parameters`.
    pub(crate) suspend: bool,
    /// The declarations it realizes, the delegated interface member first. A class member among
    /// them gives up its slot to the forwarder.
    pub(crate) realizes: Box<[ForwardedMember]>,
}

/// A declaration a forwarder realizes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ForwardedMember {
    Function(ResolvedFunctionOverrideTarget),
    Getter(ResolvedPropertyOverrideTarget),
    Setter(ResolvedPropertyOverrideTarget),
}

/// One named interface: what its member numbers are computed from.
#[derive(Clone, Debug)]
pub(crate) struct ModuleInterface {
    /// The interfaces it extends, in source order.
    pub(crate) interfaces: Box<[TypeName]>,
    /// Its members in source order.
    pub(crate) members: Box<[ModuleInterfaceMember]>,
    pub(crate) function_overrides: Box<[ModuleFunctionOverride]>,
    pub(crate) property_overrides: Box<[ModulePropertyOverride]>,
}

/// A member an interface declares or redeclares.
#[derive(Clone, Debug)]
pub(crate) enum ModuleInterfaceMember {
    Function {
        callable: CallableId,
        parameters: Box<[Ty]>,
        result: Ty,
    },
    Property {
        property: PropertyId,
        mutable: bool,
    },
}

/// An instance member a table can hold: a declaration of the module, or of a dependency class a
/// module class extends ([`crate::fir::ResolvedDependencySuperclass`]).
#[derive(Clone, Debug)]
pub(crate) enum ModuleMember {
    Function {
        callable: ResolvedFunctionOverrideTarget,
        parameters: Box<[Ty]>,
        result: Ty,
        /// A `suspend` function, which takes its caller's continuation after `parameters`.
        suspend: bool,
    },
    Property {
        property: ResolvedPropertyOverrideTarget,
        /// What its accessors take after the dispatch receiver: its context parameters, then a
        /// member extension's receiver. A setter takes `value` after them.
        accessor_parameters: Box<[Ty]>,
        value: Ty,
        /// What its getter answers: `value`, or the field's type when the getter reads an
        /// explicit backing field.
        getter_value: Ty,
        /// Whether the member is reached through its getter (and setter) rather than its field
        /// alone: it is open, abstract or an override, an interface member is implemented by it,
        /// or the declaration has an accessor body of its own.
        getter: bool,
        setter: bool,
    },
}

/// A function override edge of a class's own member.
#[derive(Clone, Debug)]
pub(crate) struct ModuleFunctionOverride {
    pub(crate) implementation: ResolvedFunctionOverrideTarget,
    pub(crate) overridden_owner: TypeName,
    pub(crate) overridden: ResolvedFunctionOverrideTarget,
    pub(crate) overridden_is_interface: bool,
    pub(crate) overridden_role: Option<SemanticCallRole>,
}

/// A property override edge of a class's own member.
#[derive(Clone, Debug)]
pub(crate) struct ModulePropertyOverride {
    pub(crate) implementation: ResolvedPropertyOverrideTarget,
    pub(crate) overridden: ResolvedPropertyOverrideTarget,
    pub(crate) overridden_is_interface: bool,
    pub(crate) declared_type: Ty,
    pub(crate) implementation_type: Ty,
    /// A member extension's receiver on each side (`val T.v` over `val Int.v`), `None` for a
    /// member property. It crosses an accessor like the value does, so it decides a bridge too.
    pub(crate) declared_receiver: Option<Ty>,
    pub(crate) implementation_receiver: Option<Ty>,
    pub(crate) depth: u32,
}

impl ModulePropertyOverride {
    /// The value and receiver types either side of the edge carries.
    fn carried_types(&self) -> impl Iterator<Item = Ty> {
        [self.declared_type, self.implementation_type]
            .into_iter()
            .chain(self.declared_receiver)
            .chain(self.implementation_receiver)
    }
}

impl ModuleClass {
    /// Every type a member of this class carries, as a target asks how it represents them.
    pub(crate) fn carried_types(&self) -> impl Iterator<Item = Ty> + '_ {
        self.members
            .iter()
            .flat_map(|member| match member {
                ModuleMember::Function {
                    parameters, result, ..
                } => parameters
                    .iter()
                    .copied()
                    .chain([*result])
                    .collect::<Vec<_>>(),
                ModuleMember::Property { .. } => Vec::new(),
            })
            .chain(
                self.property_overrides
                    .iter()
                    .flat_map(ModulePropertyOverride::carried_types),
            )
    }
}

impl ModuleInterface {
    /// Every type a member of this interface carries, as a target asks how it represents them.
    pub(crate) fn carried_types(&self) -> impl Iterator<Item = Ty> + '_ {
        self.members
            .iter()
            .flat_map(|member| match member {
                ModuleInterfaceMember::Function {
                    parameters, result, ..
                } => parameters
                    .iter()
                    .copied()
                    .chain([*result])
                    .collect::<Vec<_>>(),
                ModuleInterfaceMember::Property { .. } => Vec::new(),
            })
            .chain(
                self.property_overrides
                    .iter()
                    .flat_map(ModulePropertyOverride::carried_types),
            )
    }
}

impl ModuleClasses {
    /// Build a provider-free dispatch snapshot from class records already authenticated by the
    /// compiler. This boundary intentionally accepts no libraries, providers, or semantic lookups.
    pub(crate) fn from_support_classes(
        support: impl IntoIterator<Item = (TypeName, ModuleClass)>,
    ) -> Result<Self, TypeName> {
        let mut classes = HashMap::default();
        for (classifier, class) in support {
            if classes.insert(classifier, class).is_some() {
                return Err(classifier);
            }
        }
        Ok(Self {
            classes,
            interfaces: Vec::new(),
        })
    }

    /// Merge compiler-owned additions for one checked support unit before slot planning.
    /// Existing current-module declaration facts always win; duplicate dependency shapes are
    /// stable provider facts and need only one module-wide copy.
    pub(crate) fn merge_support_classes(&mut self, support: Self) -> Result<(), TypeName> {
        for (classifier, class) in support.classes {
            match self.classes.get(&classifier) {
                Some(existing) if existing.dependency && class.dependency => {}
                Some(_) => return Err(classifier),
                None => {
                    self.classes.insert(classifier, class);
                }
            }
        }
        for (classifier, interface) in support.interfaces {
            if !self
                .interfaces
                .iter()
                .any(|(known, _)| *known == classifier)
            {
                self.interfaces.push((classifier, interface));
            }
        }
        Ok(())
    }

    pub(crate) fn class(&self, classifier: TypeName) -> Option<&ModuleClass> {
        self.classes.get(&classifier)
    }

    /// Whether `classifier` is a dependency class a module class extends, which the support unit
    /// materializing it lays out for that subclass ([`crate::fir::ResolvedDependencySuperclass`]).
    pub fn is_dependency_class(&self, classifier: TypeName) -> bool {
        self.classes
            .get(&classifier)
            .is_some_and(|class| class.dependency)
    }

    pub fn iter(&self) -> impl Iterator<Item = (TypeName, &ModuleClass)> {
        self.classes.iter().map(|(name, class)| (*name, class))
    }

    /// The named classes of the module whose declared superclass is `superclass`.
    pub fn subclasses_of(&self, superclass: TypeName) -> impl Iterator<Item = TypeName> + '_ {
        self.classes
            .iter()
            .filter(move |(_, class)| class.superclass == Some(superclass))
            .map(|(name, _)| *name)
    }

    /// The named interfaces, in declaration order.
    pub(crate) fn interfaces(&self) -> impl Iterator<Item = (TypeName, &ModuleInterface)> {
        self.interfaces
            .iter()
            .map(|(name, interface)| (*name, interface))
    }

    /// Every type a class or interface member carries.
    pub fn carried_types(&self) -> impl Iterator<Item = Ty> + '_ {
        self.classes
            .values()
            .flat_map(ModuleClass::carried_types)
            .chain(
                self.interfaces
                    .iter()
                    .flat_map(|(_, interface)| interface.carried_types()),
            )
    }

    /// Every named, non-local class of `index` that has an instance table (not an annotation or
    /// an `object` expression), and every named interface.
    pub(crate) fn from_resolved_index(index: &ResolvedModuleIndex) -> Self {
        let implements_an_interface = interface_implementations(index);
        let mut classes = HashMap::default();
        let mut interfaces = Vec::new();
        for raw in 0..index.declaration_count() {
            let declaration = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            let Some(classifier) = index.classifier_header(declaration) else {
                continue;
            };
            let Some(header) = index.declaration_header(declaration) else {
                continue;
            };
            let flags = header.flags;
            if index.is_body_local_declaration(declaration)
                || flags.has(DeclarationFlags::ANNOTATION_CLASS)
                || flags.has(DeclarationFlags::ANONYMOUS_OBJECT)
                || flags.has(DeclarationFlags::LOCAL_CLASS)
            {
                continue;
            }
            let declared_interfaces = classifier
                .interfaces
                .iter()
                .filter_map(|supertype| supertype.get().non_null().obj_internal())
                .collect::<Box<[_]>>();
            let function_overrides = || {
                index
                    .function_overrides(declaration)
                    .iter()
                    .filter(|edge| {
                        matches!(
                            edge.implementation,
                            ResolvedFunctionOverrideTarget::Module(_)
                        )
                    })
                    .map(function_override)
                    .collect::<Box<[_]>>()
            };
            let property_overrides = || {
                index
                    .property_overrides(declaration)
                    .iter()
                    .filter(|edge| {
                        matches!(
                            edge.implementation,
                            ResolvedPropertyOverrideTarget::Module(_)
                        )
                    })
                    .map(property_override)
                    .collect::<Box<[_]>>()
            };
            if flags.has(DeclarationFlags::INTERFACE) {
                let Some(members) = interface_members(index, declaration) else {
                    continue;
                };
                interfaces.push((
                    classifier.classifier,
                    ModuleInterface {
                        interfaces: declared_interfaces,
                        members,
                        function_overrides: function_overrides(),
                        property_overrides: property_overrides(),
                    },
                ));
                continue;
            }
            let Some(members) = members(index, declaration, &implements_an_interface) else {
                continue;
            };
            let function_type_arities = classifier
                .declared_supertypes()
                .filter_map(|supertype| match supertype.get().non_null().kind() {
                    crate::types::TyKind::Fun(signature) => Some(signature.params.len()),
                    _ => None,
                })
                .collect();
            classes.insert(
                classifier.classifier,
                ModuleClass {
                    dependency: false,
                    superclass: classifier
                        .superclass
                        .and_then(|superclass| superclass.get().non_null().obj_internal())
                        .filter(|&superclass| superclass != crate::types::wk::any()),
                    function_type_arities,
                    members,
                    function_overrides: function_overrides(),
                    property_overrides: property_overrides(),
                    forwarders: forwarders(&classifier.interface_delegations),
                },
            );
        }
        for superclass in index.dependency_superclasses() {
            classes.insert(superclass.classifier, dependency_class(superclass));
        }
        Self {
            classes,
            interfaces,
        }
    }
}

/// A dependency class a module class extends, as Pass 1 froze its dispatch shape.
fn dependency_class(superclass: &crate::fir::ResolvedDependencySuperclass) -> ModuleClass {
    let members = superclass
        .members
        .iter()
        .map(|member| match member {
            crate::fir::ResolvedDependencyMember::Function {
                callable,
                parameters,
                result,
                suspend,
            } => ModuleMember::Function {
                callable: ResolvedFunctionOverrideTarget::External(*callable),
                parameters: parameters.iter().map(|parameter| parameter.get()).collect(),
                result: result.get(),
                suspend: *suspend,
            },
            crate::fir::ResolvedDependencyMember::Property {
                getter,
                setter,
                accessor_parameters,
                value,
            } => {
                ModuleMember::Property {
                    property: ResolvedPropertyOverrideTarget::External(*getter),
                    accessor_parameters: accessor_parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect(),
                    value: value.get(),
                    // Its accessors dispatch, so its getter answers the declared value.
                    getter_value: value.get(),
                    getter: true,
                    setter: *setter,
                }
            }
        })
        .collect();
    ModuleClass {
        dependency: true,
        superclass: superclass.superclass,
        function_type_arities: Box::default(),
        members,
        function_overrides: superclass
            .function_overrides
            .iter()
            .map(function_override)
            .collect(),
        property_overrides: superclass
            .property_overrides
            .iter()
            .map(property_override)
            .collect(),
        // A dependency class's delegations are realized where it is declared.
        forwarders: Box::default(),
    }
}

fn function_override(edge: &crate::fir::ResolvedFunctionOverride) -> ModuleFunctionOverride {
    ModuleFunctionOverride {
        implementation: edge.implementation,
        overridden_owner: edge.overridden_owner,
        overridden: edge.overridden,
        overridden_is_interface: edge.overridden_is_interface,
        overridden_role: edge.overridden_semantic_role,
    }
}

fn property_override(edge: &crate::fir::ResolvedPropertyOverride) -> ModulePropertyOverride {
    ModulePropertyOverride {
        implementation: edge.implementation,
        overridden: edge.overridden,
        overridden_is_interface: edge.overridden_is_interface,
        declared_type: edge.declared_type.get(),
        implementation_type: edge.implementation_type.get(),
        declared_receiver: edge.declared_receiver.map(|ty| ty.get()),
        implementation_receiver: edge.implementation_receiver.map(|ty| ty.get()),
        depth: edge.depth,
    }
}

/// The members `interface` declares, in source order, or `None` when one of them has no complete
/// header.
fn interface_members(
    index: &ResolvedModuleIndex,
    interface: DeclarationId,
) -> Option<Box<[ModuleInterfaceMember]>> {
    let mut owned = index
        .owned_declarations(interface)
        .iter()
        .copied()
        .filter_map(|declaration| {
            let header = index.declaration_header(declaration)?;
            matches!(
                header.kind,
                DeclarationKind::Function | DeclarationKind::Property
            )
            .then_some((declaration, header.kind, header.flags))
        })
        .collect::<Vec<_>>();
    owned.sort_by_key(|(declaration, ..)| index.source_order(*declaration));
    let mut members = Vec::with_capacity(owned.len());
    for (declaration, kind, flags) in owned {
        // A `companion { … }` member is static, and a `const` has no instance storage.
        if flags.has(DeclarationFlags::COMPANION) || flags.has(DeclarationFlags::CONST) {
            continue;
        }
        match kind {
            DeclarationKind::Function => {
                if index.is_suppressed_generated_callable(declaration) {
                    continue;
                }
                let callable = index.callable_for_declaration(declaration)?;
                let (parameters, result) = index.callable_carriers(callable.id)?;
                members.push(ModuleInterfaceMember::Function {
                    callable: callable.id,
                    parameters: parameters.into_boxed_slice(),
                    result,
                });
            }
            DeclarationKind::Property => {
                let property = index.property_for_declaration(declaration)?;
                members.push(ModuleInterfaceMember::Property {
                    property,
                    mutable: flags.has(DeclarationFlags::MUTABLE),
                });
            }
            _ => unreachable!("filtered to functions and properties above"),
        }
    }
    Some(members.into_boxed_slice())
}

/// The instance members `class` declares, in source order, or `None` when one of them has no
/// complete header (a module that recovered from a frontend error).
fn members(
    index: &ResolvedModuleIndex,
    class: DeclarationId,
    implements_an_interface: &HashSet<PropertyId>,
) -> Option<Box<[ModuleMember]>> {
    let mut owned = index
        .owned_declarations(class)
        .iter()
        .copied()
        .filter_map(|declaration| {
            let header = index.declaration_header(declaration)?;
            matches!(
                header.kind,
                DeclarationKind::Function | DeclarationKind::Property
            )
            .then_some((declaration, header.kind, header.flags))
        })
        .collect::<Vec<_>>();
    owned.sort_by_key(|(declaration, ..)| index.source_order(*declaration));
    let mut members = Vec::with_capacity(owned.len());
    for (declaration, kind, flags) in owned {
        // A `companion { … }` member is static, and a `const` has no instance storage.
        if flags.has(DeclarationFlags::COMPANION) || flags.has(DeclarationFlags::CONST) {
            continue;
        }
        match kind {
            DeclarationKind::Function => {
                if index.is_suppressed_generated_callable(declaration) {
                    continue;
                }
                let callable = index.callable_for_declaration(declaration)?;
                let (parameters, result) = index.callable_carriers(callable.id)?;
                members.push(ModuleMember::Function {
                    callable: ResolvedFunctionOverrideTarget::Module(callable.id),
                    parameters: parameters.into_boxed_slice(),
                    result,
                    suspend: flags.has(DeclarationFlags::SUSPEND),
                });
            }
            DeclarationKind::Property => {
                let property = index.property_for_declaration(declaration)?;
                let shape = index.property(property)?;
                let dispatched = flags.has(DeclarationFlags::OPEN)
                    || flags.has(DeclarationFlags::ABSTRACT)
                    || flags.has(DeclarationFlags::OVERRIDE)
                    || implements_an_interface.contains(&property);
                // Delegated and member-extension properties are always reached through accessor
                // functions, as is a declaration that writes an accessor body.
                let accessors =
                    flags.has(DeclarationFlags::DELEGATED) || shape.extension_receiver.is_some();
                let mutable = flags.has(DeclarationFlags::MUTABLE);
                let signature = index.signature(declaration)?;
                let value = signature.result.get();
                // An accessor takes the property's context parameters, then a member extension's
                // receiver, as a callable does.
                let mut parameters = signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                if let Some(receiver) = shape.extension_receiver {
                    let position = shape.context_parameter_count as usize;
                    if position > parameters.len() {
                        return None;
                    }
                    parameters.insert(position, receiver.get());
                }
                // A getter with no function of its own reads the field, so it answers what an
                // explicit backing field stores.
                let reads_field = !accessors && !flags.has(DeclarationFlags::CUSTOM_GETTER);
                let getter_value = match shape.storage_type {
                    Some(storage) if reads_field => storage.get(),
                    _ => value,
                };
                members.push(ModuleMember::Property {
                    property: ResolvedPropertyOverrideTarget::Module(property),
                    accessor_parameters: parameters.into_boxed_slice(),
                    value,
                    getter_value,
                    getter: dispatched || accessors || flags.has(DeclarationFlags::CUSTOM_GETTER),
                    setter: mutable
                        && (dispatched
                            || accessors
                            || flags.has(DeclarationFlags::SETTER_HAS_BODY)),
                });
            }
            _ => unreachable!("filtered to functions and properties above"),
        }
    }
    Some(members.into_boxed_slice())
}

/// Every module property some class of the module hands to an interface it implements: such a
/// property is reached through the interface's number, so it dispatches even where its own
/// declaration is neither open nor an override.
fn interface_implementations(index: &ResolvedModuleIndex) -> HashSet<PropertyId> {
    let mut properties = HashSet::default();
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(raw as u32);
        if index.classifier_header(declaration).is_none() {
            continue;
        }
        for edge in index.property_overrides(declaration) {
            if let (true, ResolvedPropertyOverrideTarget::Module(property)) =
                (edge.overridden_is_interface, edge.implementation)
            {
                properties.insert(property);
            }
        }
    }
    properties
}

/// The forwarders `delegations` generate, in the order the declaring file creates them.
fn forwarders(delegations: &[ResolvedInterfaceDelegation]) -> Box<[ModuleForwarder]> {
    let mut forwarders = Vec::new();
    for delegation in delegations {
        for member in &delegation.members {
            match member {
                ResolvedDelegatedMember::Function(function) => {
                    forwarders.push(ModuleForwarder {
                        parameters: function.call.parameters.iter().map(|ty| ty.get()).collect(),
                        result: function.call.result.get(),
                        suspend: function.call.suspend,
                        realizes: function
                            .overridden
                            .iter()
                            .map(|declaration| ForwardedMember::Function(declaration.target))
                            .collect(),
                    });
                }
                ResolvedDelegatedMember::Property(property) => {
                    // An accessor takes the context parameters, then a member extension's
                    // receiver; the setter takes the value after them.
                    let mut parameters = property
                        .context_parameters
                        .iter()
                        .map(|parameter| parameter.ty.get())
                        .collect::<Vec<_>>();
                    if let Some(position) = property.getter.extension_receiver_parameter {
                        if let Some(receiver) = property.getter.parameters.get(position as usize) {
                            parameters.push(receiver.get());
                        }
                    }
                    let realized = property
                        .overridden
                        .iter()
                        .map(|declaration| declaration.target)
                        .collect::<Vec<_>>();
                    let ty = property.ty.get();
                    forwarders.push(ModuleForwarder {
                        parameters: parameters.clone().into_boxed_slice(),
                        result: ty,
                        suspend: false,
                        realizes: realized
                            .iter()
                            .map(|&p| ForwardedMember::Getter(p))
                            .collect(),
                    });
                    if property.setter.is_some() {
                        parameters.push(ty);
                        forwarders.push(ModuleForwarder {
                            parameters: parameters.into_boxed_slice(),
                            result: Ty::Unit,
                            suspend: false,
                            realizes: realized
                                .iter()
                                .map(|&p| ForwardedMember::Setter(p))
                                .collect(),
                        });
                    }
                }
            }
        }
    }
    forwarders.into_boxed_slice()
}

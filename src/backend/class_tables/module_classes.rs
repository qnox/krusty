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
    ResolvedFunctionOverrideTarget, ResolvedModuleIndex, ResolvedPropertyOverrideTarget,
};
use crate::types::{SemanticCallRole, Ty, TypeName};

/// The named classes of one module, by identity.
#[derive(Debug, Default)]
pub struct ModuleClasses {
    classes: HashMap<TypeName, ModuleClass>,
    /// In declaration order, so numbering them visits them the same way in every process.
    interfaces: Vec<(TypeName, ModuleInterface)>,
}

/// One named class: what its dispatch table is built from.
#[derive(Debug)]
pub(crate) struct ModuleClass {
    /// The declared superclass, or `None` for `kotlin.Any`.
    pub(crate) superclass: Option<TypeName>,
    /// Function types among the declared supertypes, by arity.
    pub(crate) function_type_arities: Box<[usize]>,
    /// Instance members in source order.
    pub(crate) members: Box<[ModuleMember]>,
    pub(crate) function_overrides: Box<[ModuleFunctionOverride]>,
    pub(crate) property_overrides: Box<[ModulePropertyOverride]>,
}

/// One named interface: what its member numbers are computed from.
#[derive(Debug)]
pub(crate) struct ModuleInterface {
    /// The interfaces it extends, in source order.
    pub(crate) interfaces: Box<[TypeName]>,
    /// Its members in source order.
    pub(crate) members: Box<[ModuleInterfaceMember]>,
    pub(crate) function_overrides: Box<[ModuleFunctionOverride]>,
    pub(crate) property_overrides: Box<[ModulePropertyOverride]>,
}

/// A member an interface declares or redeclares.
#[derive(Debug)]
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

/// An instance member a table can hold.
#[derive(Debug)]
pub(crate) enum ModuleMember {
    Function {
        callable: CallableId,
        parameters: Box<[Ty]>,
        result: Ty,
    },
    Property {
        property: PropertyId,
        /// Whether the member is reached through its getter (and setter) rather than its field
        /// alone: it is open, abstract or an override, an interface member is implemented by it,
        /// or the declaration has an accessor body of its own.
        getter: bool,
        setter: bool,
    },
}

/// A function override edge of a class's own member.
#[derive(Debug)]
pub(crate) struct ModuleFunctionOverride {
    pub(crate) implementation: CallableId,
    pub(crate) overridden_owner: TypeName,
    pub(crate) overridden: ResolvedFunctionOverrideTarget,
    pub(crate) overridden_is_interface: bool,
    pub(crate) overridden_role: Option<SemanticCallRole>,
}

/// A property override edge of a class's own member.
#[derive(Debug)]
pub(crate) struct ModulePropertyOverride {
    pub(crate) implementation: PropertyId,
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
    pub(crate) fn class(&self, classifier: TypeName) -> Option<&ModuleClass> {
        self.classes.get(&classifier)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (TypeName, &ModuleClass)> {
        self.classes.iter().map(|(name, class)| (*name, class))
    }

    /// The named interfaces, in declaration order.
    pub(crate) fn interfaces(&self) -> impl Iterator<Item = (TypeName, &ModuleInterface)> {
        self.interfaces
            .iter()
            .map(|(name, interface)| (*name, interface))
    }

    /// Every type a class or interface member carries.
    pub(crate) fn carried_types(&self) -> impl Iterator<Item = Ty> + '_ {
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
                    .filter_map(|edge| {
                        let ResolvedFunctionOverrideTarget::Module(implementation) =
                            edge.implementation
                        else {
                            return None;
                        };
                        Some(ModuleFunctionOverride {
                            implementation,
                            overridden_owner: edge.overridden_owner,
                            overridden: edge.overridden,
                            overridden_is_interface: edge.overridden_is_interface,
                            overridden_role: edge.overridden_semantic_role,
                        })
                    })
                    .collect::<Box<[_]>>()
            };
            let property_overrides = || {
                index
                    .property_overrides(declaration)
                    .iter()
                    .filter_map(|edge| {
                        let ResolvedPropertyOverrideTarget::Module(implementation) =
                            edge.implementation
                        else {
                            return None;
                        };
                        Some(ModulePropertyOverride {
                            implementation,
                            overridden: edge.overridden,
                            overridden_is_interface: edge.overridden_is_interface,
                            declared_type: edge.declared_type.get(),
                            implementation_type: edge.implementation_type.get(),
                            declared_receiver: edge.declared_receiver.map(|ty| ty.get()),
                            implementation_receiver: edge
                                .implementation_receiver
                                .map(|ty| ty.get()),
                            depth: edge.depth,
                        })
                    })
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
                    superclass: classifier
                        .superclass
                        .and_then(|superclass| superclass.get().non_null().obj_internal())
                        .filter(|&superclass| superclass != crate::types::wk::any()),
                    function_type_arities,
                    members,
                    function_overrides: function_overrides(),
                    property_overrides: property_overrides(),
                },
            );
        }
        Self {
            classes,
            interfaces,
        }
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
                    callable: callable.id,
                    parameters: parameters.into_boxed_slice(),
                    result,
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
                members.push(ModuleMember::Property {
                    property,
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

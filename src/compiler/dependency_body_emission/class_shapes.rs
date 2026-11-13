//! Exact KLIB class shapes retained across dependency support units.

use crate::backend::class_tables::module_classes::{
    ModuleClass, ModuleFunctionOverride, ModuleMember, ModulePropertyOverride,
};
use crate::fir::{ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::types::{Ty, TypeName};

/// Provider-independent identity facts published when a dependency class is emitted. Callable
/// types are deliberately re-read from the current exact provider before planning, because each
/// source module has a fresh provider-local type-parameter identity space.
#[derive(Clone)]
pub(crate) struct PublishedDependencyClassShape {
    pub(crate) classifier: TypeName,
    constructor: crate::libraries::KlibDependencyCallable,
    superclass: Option<TypeName>,
    function_type_arities: Box<[usize]>,
    members: Vec<PublishedDependencyClassMember>,
}

#[derive(Clone)]
enum PublishedDependencyClassMember {
    Function {
        target: crate::libraries::KlibDependencyCallable,
        overrides: Vec<crate::klib_lowering::MaterializedClassFunctionOverride>,
    },
    Property {
        getter: crate::libraries::KlibDependencyCallable,
        setter: Option<crate::libraries::KlibDependencyCallable>,
        getter_reached: bool,
        setter_reached: bool,
        function_overrides: Vec<crate::klib_lowering::MaterializedClassFunctionOverride>,
        overrides: Vec<crate::klib_lowering::MaterializedClassPropertyOverride>,
    },
}

struct PublishedPropertyGroup {
    getter: crate::libraries::KlibDependencyCallable,
    slot: usize,
    setter: Option<crate::libraries::KlibDependencyCallable>,
    getter_reached: bool,
    setter_reached: bool,
    function_overrides: Vec<crate::klib_lowering::MaterializedClassFunctionOverride>,
    property_overrides: Vec<crate::klib_lowering::MaterializedClassPropertyOverride>,
}

impl PublishedDependencyClassShape {
    pub(crate) fn constructor_identity(&self) -> &crate::libraries::KlibDependencyCallable {
        &self.constructor
    }

    pub(crate) fn from_materialized_ir(
        ir: &crate::ir::IrFile,
        materialized: &[(
            TypeName,
            crate::ir::ClassId,
            crate::libraries::KlibDependencyCallable,
        )],
        members: &[crate::klib_lowering::MaterializedClassMember],
    ) -> Result<Vec<Self>, TypeName> {
        let mut shapes = Vec::with_capacity(materialized.len());
        for &(classifier, class_id, ref constructor) in materialized {
            let class = &ir.classes[class_id as usize];
            let mut published_members = Vec::new();
            let mut properties: Vec<PublishedPropertyGroup> = Vec::new();
            for member in members.iter().filter(|member| member.owner == classifier) {
                let Some(role) = member.accessor else {
                    published_members.push(Some(PublishedDependencyClassMember::Function {
                        target: member.target.clone(),
                        overrides: member.function_overrides.clone(),
                    }));
                    continue;
                };
                let getter = member.property_getter.clone().ok_or(classifier)?;
                let slot = properties
                    .iter()
                    .position(|known| known.getter == getter)
                    .unwrap_or_else(|| {
                        let position = published_members.len();
                        published_members.push(None);
                        properties.push(PublishedPropertyGroup {
                            getter: getter.clone(),
                            slot: position,
                            setter: member.property_setter.clone(),
                            getter_reached: false,
                            setter_reached: false,
                            function_overrides: Vec::new(),
                            property_overrides: Vec::new(),
                        });
                        properties.len() - 1
                    });
                let property = &mut properties[slot];
                if property.getter != getter || property.setter != member.property_setter {
                    return Err(classifier);
                }
                match role {
                    crate::libraries::KlibAccessorRole::Getter => property.getter_reached = true,
                    crate::libraries::KlibAccessorRole::Setter => property.setter_reached = true,
                }
                for edge in &member.function_overrides {
                    if !property.function_overrides.iter().any(|known| {
                        known.implementation == edge.implementation
                            && known.overridden == edge.overridden
                    }) {
                        property.function_overrides.push(edge.clone());
                    }
                }
                for edge in &member.property_overrides {
                    if !property.property_overrides.iter().any(|known| {
                        known.overridden == edge.overridden
                            && known.overridden_owner == edge.overridden_owner
                            && known.overridden_is_interface == edge.overridden_is_interface
                    }) {
                        property.property_overrides.push(edge.clone());
                    }
                }
            }
            for property in properties {
                published_members[property.slot] = Some(PublishedDependencyClassMember::Property {
                    getter: property.getter,
                    setter: property.setter,
                    getter_reached: property.getter_reached,
                    setter_reached: property.setter_reached,
                    function_overrides: property.function_overrides,
                    overrides: property.property_overrides,
                });
            }
            let published_members = published_members
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .ok_or(classifier)?;
            let function_type_arities = class
                .supertypes
                .iter()
                .filter_map(|supertype| match supertype.non_null().kind() {
                    crate::types::TyKind::Fun(signature) => Some(signature.params.len()),
                    _ => None,
                })
                .collect::<Box<[_]>>();
            shapes.push(Self {
                classifier,
                constructor: constructor.clone(),
                superclass: (class.superclass != crate::types::wk::any())
                    .then_some(class.superclass),
                function_type_arities,
                members: published_members,
            });
        }
        Ok(shapes)
    }

    pub(crate) fn materialize(
        &self,
        declarations: &dyn crate::symbol_source::SymbolSource,
    ) -> Result<ModuleClass, TypeName> {
        let (_constructor_id, constructor) =
            resolve_dependency_callable(declarations, &self.constructor, self.classifier)?;
        let Some(constructor_callable) = constructor
            .klib_body_callable()
            .filter(|callable| callable.identity() == &self.constructor)
        else {
            return Err(self.classifier);
        };
        let Some(constructor_contract) = constructor_callable.class_contract() else {
            return Err(self.classifier);
        };
        if constructor.kind != crate::libraries::ExternalCallableKind::Constructor
            || constructor_callable.declaration_owner()
                != Some(crate::types::SemanticCallableOwner::Classifier(
                    self.classifier,
                ))
            || constructor_contract.classifier != self.classifier
            || constructor_contract.module != self.constructor.module
        {
            return Err(self.classifier);
        }

        let mut members = Vec::new();
        let mut function_overrides = Vec::new();
        let mut property_overrides = Vec::new();
        for member in &self.members {
            match member {
                PublishedDependencyClassMember::Function { target, overrides } => {
                    let (implementation_id, implementation) =
                        resolve_dependency_callable(declarations, target, self.classifier)?;
                    let Some(implementation_callable) = implementation
                        .klib_body_callable()
                        .filter(|callable| callable.identity() == target)
                    else {
                        return Err(self.classifier);
                    };
                    check_materialized_member_owner(implementation_callable, self.classifier)?;
                    if implementation_callable.accessor().is_some()
                        || implementation.kind != crate::libraries::ExternalCallableKind::Member
                    {
                        return Err(self.classifier);
                    }
                    let Some(modifiers) = implementation_callable.declaration_modifiers() else {
                        return Err(self.classifier);
                    };
                    members.push(ModuleMember::Function {
                        callable: ResolvedFunctionOverrideTarget::External(implementation_id),
                        parameters: implementation_callable.params().to_vec().into_boxed_slice(),
                        result: implementation_callable.ret(),
                        suspend: modifiers.is_suspend,
                    });
                    for edge in overrides {
                        if edge.implementation != *target {
                            return Err(self.classifier);
                        }
                        let (overridden_id, overridden) = resolve_dependency_callable(
                            declarations,
                            &edge.overridden,
                            edge.overridden_owner,
                        )?;
                        let Some(overridden_callable) = overridden
                            .klib_body_callable()
                            .filter(|callable| callable.identity() == &edge.overridden)
                        else {
                            return Err(self.classifier);
                        };
                        check_materialized_member_owner(
                            overridden_callable,
                            edge.overridden_owner,
                        )?;
                        if overridden_callable.class_contract().is_none_or(|contract| {
                            (contract.kind == crate::libraries::TypeKind::Interface)
                                != edge.overridden_is_interface
                        }) {
                            return Err(self.classifier);
                        }
                        function_overrides.push(ModuleFunctionOverride {
                            implementation: ResolvedFunctionOverrideTarget::External(
                                implementation_id,
                            ),
                            overridden_owner: edge.overridden_owner,
                            overridden: ResolvedFunctionOverrideTarget::External(overridden_id),
                            overridden_is_interface: edge.overridden_is_interface,
                            overridden_role: edge.overridden_role,
                        });
                    }
                }
                PublishedDependencyClassMember::Property {
                    getter,
                    setter,
                    getter_reached,
                    setter_reached,
                    function_overrides: member_function_overrides,
                    overrides,
                } => {
                    let (getter_id, getter_realization) =
                        resolve_dependency_callable(declarations, getter, self.classifier)?;
                    let Some(getter_callable) = getter_realization
                        .klib_body_callable()
                        .filter(|callable| callable.identity() == getter)
                    else {
                        return Err(self.classifier);
                    };
                    check_materialized_member_owner(getter_callable, self.classifier)?;
                    if getter_callable.accessor().is_none_or(|accessor| {
                        accessor.role != crate::libraries::KlibAccessorRole::Getter
                    }) {
                        return Err(self.classifier);
                    }
                    let accessor_parameters = getter_callable.params().to_vec();
                    let value = getter_callable.ret();
                    let getter_accessor = getter_callable.accessor().ok_or(self.classifier)?;
                    let property = declarations
                        .external_property(getter_accessor.property)
                        .ok_or(self.classifier)?;
                    if property.getter != getter_id
                        || property.setter.is_some() != setter.is_some()
                        || getter_realization.kind != crate::libraries::ExternalCallableKind::Member
                    {
                        return Err(self.classifier);
                    }
                    let resolved_setter_id = if let Some(setter) = setter {
                        let (resolved_setter_id, setter_realization) =
                            resolve_dependency_callable(declarations, setter, self.classifier)?;
                        let Some(setter_callable) = setter_realization
                            .klib_body_callable()
                            .filter(|callable| callable.identity() == setter)
                        else {
                            return Err(self.classifier);
                        };
                        check_materialized_member_owner(setter_callable, self.classifier)?;
                        if setter_callable.accessor().is_none_or(|accessor| {
                            accessor.role != crate::libraries::KlibAccessorRole::Setter
                        }) || setter_callable.params().len() != accessor_parameters.len() + 1
                            || setter_callable.params()[..accessor_parameters.len()]
                                != accessor_parameters
                            || setter_callable.params().last().copied() != Some(value)
                            || setter_callable.accessor().is_none_or(|accessor| {
                                accessor.property != getter_accessor.property
                            })
                            || setter_realization.kind
                                != crate::libraries::ExternalCallableKind::Member
                            || property.setter != Some(resolved_setter_id)
                        {
                            return Err(self.classifier);
                        }
                        Some(resolved_setter_id)
                    } else {
                        None
                    };
                    members.push(ModuleMember::Property {
                        property: ResolvedPropertyOverrideTarget::External(getter_id),
                        accessor_parameters: accessor_parameters.into_boxed_slice(),
                        value,
                        getter_value: value,
                        getter: *getter_reached,
                        setter: *setter_reached,
                    });
                    for edge in member_function_overrides {
                        if edge.implementation != *getter
                            && setter.as_ref() != Some(&edge.implementation)
                        {
                            return Err(self.classifier);
                        }
                        let (implementation_id, implementation_realization) =
                            resolve_dependency_callable(
                                declarations,
                                &edge.implementation,
                                self.classifier,
                            )?;
                        let Some(implementation_callable) = implementation_realization
                            .klib_body_callable()
                            .filter(|callable| callable.identity() == &edge.implementation)
                        else {
                            return Err(self.classifier);
                        };
                        check_materialized_member_owner(implementation_callable, self.classifier)?;
                        let expected_role = if edge.implementation == *getter {
                            crate::libraries::KlibAccessorRole::Getter
                        } else {
                            crate::libraries::KlibAccessorRole::Setter
                        };
                        if implementation_callable
                            .accessor()
                            .is_none_or(|accessor| accessor.role != expected_role)
                        {
                            return Err(self.classifier);
                        }
                        if implementation_id != getter_id
                            && Some(implementation_id) != resolved_setter_id
                        {
                            return Err(self.classifier);
                        }
                        let (overridden_id, overridden_realization) = resolve_dependency_callable(
                            declarations,
                            &edge.overridden,
                            edge.overridden_owner,
                        )?;
                        let Some(overridden_callable) = overridden_realization
                            .klib_body_callable()
                            .filter(|callable| callable.identity() == &edge.overridden)
                        else {
                            return Err(self.classifier);
                        };
                        check_materialized_member_owner(
                            overridden_callable,
                            edge.overridden_owner,
                        )?;
                        if (overridden_callable.class_contract().is_none_or(|contract| {
                            contract.kind == crate::libraries::TypeKind::Interface
                        })) != edge.overridden_is_interface
                        {
                            return Err(self.classifier);
                        }
                        function_overrides.push(ModuleFunctionOverride {
                            implementation: ResolvedFunctionOverrideTarget::External(
                                implementation_id,
                            ),
                            overridden_owner: edge.overridden_owner,
                            overridden: ResolvedFunctionOverrideTarget::External(overridden_id),
                            overridden_is_interface: edge.overridden_is_interface,
                            overridden_role: edge.overridden_role,
                        });
                    }
                    for edge in overrides {
                        let (overridden_id, overridden_realization) = resolve_dependency_callable(
                            declarations,
                            &edge.overridden,
                            edge.overridden_owner,
                        )?;
                        let Some(overridden_callable) = overridden_realization
                            .klib_body_callable()
                            .filter(|callable| callable.identity() == &edge.overridden)
                        else {
                            return Err(self.classifier);
                        };
                        check_materialized_member_owner(
                            overridden_callable,
                            edge.overridden_owner,
                        )?;
                        if overridden_callable.accessor().is_none_or(|accessor| {
                            accessor.role != crate::libraries::KlibAccessorRole::Getter
                        }) {
                            return Err(self.classifier);
                        }
                        let overridden_accessor =
                            overridden_callable.accessor().ok_or(self.classifier)?;
                        let overridden_property = declarations
                            .external_property(overridden_accessor.property)
                            .ok_or(self.classifier)?;
                        if overridden_property.getter != overridden_id {
                            return Err(self.classifier);
                        }
                        if overridden_callable.class_contract().is_none_or(|contract| {
                            (contract.kind == crate::libraries::TypeKind::Interface)
                                != edge.overridden_is_interface
                        }) {
                            return Err(self.classifier);
                        }
                        let declared_receiver = extension_receiver(overridden_callable);
                        let implementation_receiver = extension_receiver(getter_callable);
                        property_overrides.push(ModulePropertyOverride {
                            implementation: ResolvedPropertyOverrideTarget::External(getter_id),
                            overridden: ResolvedPropertyOverrideTarget::External(overridden_id),
                            overridden_is_interface: edge.overridden_is_interface,
                            declared_type: overridden_callable.ret(),
                            implementation_type: value,
                            declared_receiver,
                            implementation_receiver,
                            depth: 1,
                        });
                    }
                }
            }
        }
        Ok(ModuleClass {
            dependency: true,
            superclass: self.superclass,
            function_type_arities: self.function_type_arities.clone(),
            members: members.into_boxed_slice(),
            function_overrides: function_overrides.into_boxed_slice(),
            property_overrides: property_overrides.into_boxed_slice(),
            delegates_interfaces: false,
        })
    }
}

fn resolve_dependency_callable(
    declarations: &dyn crate::symbol_source::SymbolSource,
    identity: &crate::libraries::KlibDependencyCallable,
    classifier: TypeName,
) -> Result<
    (
        crate::fir::ExternalCallableId,
        crate::libraries::ExternalCallableRealization,
    ),
    TypeName,
> {
    let (external, realization) = declarations
        .external_callable_by_dependency(identity)
        .ok_or(classifier)?;
    if realization.dependency_callable.as_ref() != Some(identity) {
        return Err(classifier);
    }
    Ok((external, realization))
}

fn check_materialized_member_owner(
    callable: crate::libraries::KlibBodyCallable<'_>,
    owner: TypeName,
) -> Result<(), TypeName> {
    let Some(contract) = callable.class_contract() else {
        return Err(owner);
    };
    if callable.kind() != crate::libraries::ExternalCallableKind::Member
        || callable.identity().module != contract.module
        || callable.declaration_owner()
            != Some(crate::types::SemanticCallableOwner::Classifier(owner))
        || contract.classifier != owner
    {
        return Err(owner);
    }
    Ok(())
}

fn extension_receiver(callable: crate::libraries::KlibBodyCallable<'_>) -> Option<Ty> {
    callable
        .parameter_identities()
        .iter()
        .position(|identity| *identity == crate::fir::ResolvedParameterIdentity::ExtensionReceiver)
        .and_then(|index| callable.params().get(index).copied())
}

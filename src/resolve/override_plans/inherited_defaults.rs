//! Selection of the interface defaults a classifier inherits without overriding.
//!
//! The classifier's interface closure is walked once while the declaration providers are live. In
//! that closure every interface precedes its ancestors, so the first declaration of an override slot
//! is the nearest one and wins even when it is abstract: an abstract redeclaration suppresses a
//! farther ancestor's body. A selected member is published when it has a body, is not the
//! classifier's own declaration, and is not supplied through the superclass. The published record
//! names the direct superinterface the member is inherited through; no target realization is
//! chosen here.

use std::collections::HashSet;

use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, ExternalCallableId, InheritedDefaultBody,
    InheritedMemberName, ResolvedFunctionOverride, ResolvedInheritedDefault, ResolvedModuleIndex,
    ResolvedParameterIdentity, ResolvedPropertyOverride,
};
use crate::libraries::{FnKind, FunctionInfo, LibraryCallable, PropKind, PropertyInfo};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName, Visibility};

/// The override plan of one classifier, as its inherited defaults are selected from it.
pub(super) struct ClassifierPlan<'a> {
    pub(super) classifier: DeclarationId,
    pub(super) hierarchy: &'a [crate::fir::ResolvedAppliedClassifier],
    pub(super) superclass_interfaces: &'a [TypeName],
    pub(super) functions: &'a [ResolvedFunctionOverride],
    pub(super) properties: &'a [ResolvedPropertyOverride],
}

/// The interface defaults `plan.classifier` inherits without overriding, in closure order.
pub(super) fn inherited_defaults(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    plan: &ClassifierPlan<'_>,
) -> Vec<ResolvedInheritedDefault> {
    let Some(header) = index.classifier_header(plan.classifier) else {
        return Vec::new();
    };
    let owner = header.classifier;
    let direct = header
        .interfaces
        .iter()
        .filter_map(|interface| interface.get().non_null().obj_internal())
        .collect::<Vec<_>>();
    let closure = sorted_closure(source, &direct);
    if closure.is_empty() {
        return Vec::new();
    }
    let applied = |classifier: TypeName| {
        plan.hierarchy
            .iter()
            .find(|entry| entry.classifier == classifier)
            .map_or_else(|| Ty::obj_name(classifier), |entry| entry.applied.get())
    };
    // The classifier's own declarations occupy their slots first.
    let mut selected = Vec::new();
    if let Some(own) = source.classifier(owner) {
        let receiver = applied(owner);
        for name in &own.declared_callable_order {
            let (functions, properties) =
                crate::symbol_resolver::declared_member_callables(source, receiver, name)
                    .into_parts();
            selected.extend(
                functions
                    .overloads
                    .into_iter()
                    .filter(member_function)
                    .map(|function| Slot::Function(Box::new(function))),
            );
            selected.extend(
                properties
                    .overloads
                    .into_iter()
                    .filter(member_property)
                    .map(|property| Slot::Property(Box::new(property))),
            );
        }
    }
    // `interface I by delegate` implements every member of `I` and its ancestors by delegation.
    let delegated = sorted_closure(
        source,
        &header
            .interface_delegations
            .iter()
            .filter_map(|delegation| delegation.interface.get().non_null().obj_internal())
            .collect::<Vec<_>>(),
    );
    // A delegation also implements every slot of those interfaces that a sibling interface
    // redeclares with a body (`object : Base2, Base by delegate` where `Base2` overrides
    // `Base.test`): the delegate answers it.
    for &interface in &delegated {
        if let Some(shape) = source.classifier(interface) {
            selected.extend(
                interface_surface(index, source, &shape, interface, applied(interface))
                    .into_iter()
                    .map(|member| member.slot),
            );
        }
    }
    let ancestors = direct
        .iter()
        .map(|&interface| {
            (
                interface,
                sorted_closure(source, &[interface])
                    .into_iter()
                    .collect::<HashSet<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let mut defaults = Vec::new();
    for interface in closure {
        let Some(shape) = source.classifier(interface) else {
            continue;
        };
        let receiver = applied(interface);
        for member in interface_surface(index, source, &shape, interface, receiver) {
            if selected
                .iter()
                .any(|slot| slot.same_slot(source, &member.slot))
            {
                continue;
            }
            let SurfaceMember {
                slot,
                accessors,
                is_kotlin,
            } = member;
            let implemented = match &slot {
                Slot::Function(function) => {
                    super::function_target(index, function).is_some_and(|target| {
                        plan.functions.iter().any(|edge| {
                            edge.overridden == target
                                && !is_interface(source, edge.implementation_owner)
                        })
                    })
                }
                Slot::Property(property) => super::target(index, property).is_some_and(|target| {
                    plan.properties.iter().any(|edge| {
                        edge.overridden == target
                            && !is_interface(source, edge.implementation_owner)
                    })
                }),
            };
            let applied_shapes = slot.applied_accessor_shapes();
            assert_eq!(
                applied_shapes.len(),
                accessors.len(),
                "an applied member keeps every accessor of its declaration"
            );
            selected.push(slot);
            // The direct superclass already realizes every member of an interface it reaches, and a
            // delegation every member of the interface it delegates.
            if plan.superclass_interfaces.contains(&interface)
                || delegated.contains(&interface)
                || implemented
            {
                continue;
            }
            let dispatch_interface = ancestors
                .iter()
                .find(|(_, closure)| closure.contains(&interface))
                .map(|(direct, _)| *direct)
                .expect("a closure interface is reached through a direct superinterface");
            for (accessor, (applied_parameters, applied_result)) in
                accessors.into_iter().zip(applied_shapes)
            {
                if accessor.is_abstract || accessor.visibility == Visibility::Private {
                    continue;
                }
                let Some(body) = accessor.body(is_kotlin) else {
                    continue;
                };
                defaults.push(ResolvedInheritedDefault {
                    name: accessor.name,
                    function: accessor.function,
                    declaring_interface: interface,
                    dispatch_interface,
                    parameters: accessor.parameters,
                    parameter_identities: accessor.parameter_identities,
                    result: accessor.result,
                    applied_parameters,
                    applied_result,
                    suspend: accessor.suspend,
                    vararg: accessor.vararg,
                    body,
                });
            }
        }
    }
    defaults
}

fn is_interface(source: &dyn SymbolSource, classifier: TypeName) -> bool {
    source
        .classifier(classifier)
        .is_some_and(|shape| shape.is_interface())
}

fn member_function(function: &FunctionInfo) -> bool {
    matches!(function.kind, FnKind::Member | FnKind::Extension)
}

fn member_property(property: &PropertyInfo) -> bool {
    matches!(property.kind, PropKind::Member | PropKind::MemberExtension)
}

/// The transitive interface closure of `direct` in topological order: every interface precedes
/// all of its ancestors, while incomparable interfaces keep declaration order.
fn sorted_closure(source: &dyn SymbolSource, direct: &[TypeName]) -> Vec<TypeName> {
    fn visit(
        source: &dyn SymbolSource,
        interface: TypeName,
        seen: &mut HashSet<TypeName>,
        out: &mut Vec<TypeName>,
    ) {
        if !seen.insert(interface) {
            return;
        }
        let Some(shape) = source
            .classifier(interface)
            .filter(|shape| shape.is_interface())
        else {
            return;
        };
        let parents = shape.supertypes.iter_ids().collect::<Vec<_>>();
        for parent in parents.into_iter().rev() {
            visit(source, parent, seen, out);
        }
        out.push(interface);
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for interface in direct.iter().rev() {
        visit(source, *interface, &mut seen, &mut out);
    }
    out.reverse();
    out
}

/// One override slot, as the classifier sees it through its applied supertypes.
enum Slot {
    Function(Box<FunctionInfo>),
    Property(Box<PropertyInfo>),
}

impl Slot {
    /// The parameters and result of each member this slot contributes (a function, or a
    /// property's getter then setter), as the classifier's applied supertype substitutes them.
    fn applied_accessor_shapes(&self) -> Vec<(Box<[Ty]>, Ty)> {
        match self {
            Slot::Function(function) => vec![(
                super::declaration_parameters_with_receiver(function).into_boxed_slice(),
                function.ret.apply(function.callable.ret),
            )],
            Slot::Property(property) => std::iter::once(&property.getter)
                .chain(property.setter.as_ref())
                .map(|accessor| (accessor.params.clone().into_boxed_slice(), accessor.ret))
                .collect(),
        }
    }

    fn same_slot(&self, source: &dyn SymbolSource, other: &Slot) -> bool {
        match (self, other) {
            (Slot::Function(left), Slot::Function(right)) => {
                left.callable.name == right.callable.name
                    && crate::symbol_resolver::override_input_shapes_match(
                        source,
                        input_shape(left).as_shape(),
                        input_shape(right).as_shape(),
                    )
            }
            (Slot::Property(left), Slot::Property(right)) => {
                left.name == right.name
                    && left.kind == right.kind
                    && left.context_count == right.context_count
                    && match (
                        left.receiver
                            .filter(|_| left.kind == PropKind::MemberExtension),
                        right
                            .receiver
                            .filter(|_| right.kind == PropKind::MemberExtension),
                    ) {
                        (None, None) => true,
                        (Some(left_receiver), Some(right_receiver)) => {
                            crate::symbol_resolver::override_parameter_types_match(
                                source,
                                &[left_receiver],
                                &left.formals,
                                &[right_receiver],
                                &right.formals,
                            )
                        }
                        (None, Some(_)) | (Some(_), None) => false,
                    }
            }
            (Slot::Function(_), Slot::Property(_)) | (Slot::Property(_), Slot::Function(_)) => {
                false
            }
        }
    }
}

struct InputShape<'a> {
    params: Vec<Ty>,
    function: &'a FunctionInfo,
}

impl<'a> InputShape<'a> {
    fn as_shape(&self) -> crate::symbol_resolver::OverrideInputShape<'_> {
        let signature = self.function.generic_sig.as_ref();
        crate::symbol_resolver::OverrideInputShape {
            params: &self.params,
            receiver: self
                .function
                .semantic_receiver()
                .filter(|_| self.function.is_extension()),
            formals: signature
                .map(|signature| signature.formals.as_slice())
                .unwrap_or_default(),
            formal_bounds: signature
                .map(|signature| signature.formal_bounds.as_slice())
                .unwrap_or_default(),
            context_count: self.function.context_count,
            suspend: self.function.flags.suspend,
        }
    }
}

fn input_shape(function: &FunctionInfo) -> InputShape<'_> {
    InputShape {
        params: function.semantic_params().into_owned(),
        function,
    }
}

/// One declaration of an interface's surface: its slot, and the members it contributes (a
/// function, or a property's getter and setter).
struct SurfaceMember {
    slot: Slot,
    accessors: Vec<SurfaceAccessor>,
    is_kotlin: bool,
}

struct SurfaceAccessor {
    name: InheritedMemberName,
    function: Option<crate::fir::ResolvedFunctionOverrideTarget>,
    parameters: Box<[Ty]>,
    parameter_identities: Box<[ResolvedParameterIdentity]>,
    result: Ty,
    suspend: bool,
    /// The declaration's last physical parameter is its declared `vararg`.
    vararg: bool,
    is_abstract: bool,
    visibility: Visibility,
    /// The provider's realization of a dependency declaration; `None` for this module's.
    dependency: Option<DependencyRealization>,
}

struct DependencyRealization {
    identity: ExternalCallableId,
    realization: crate::libraries::MemberRealization,
    has_nonvirtual_realization: bool,
}

impl SurfaceAccessor {
    fn body(&self, is_kotlin: bool) -> Option<InheritedDefaultBody> {
        let Some(dependency) = &self.dependency else {
            return Some(InheritedDefaultBody::Module);
        };
        match (
            dependency.has_nonvirtual_realization,
            dependency.realization,
        ) {
            (true, _) => Some(InheritedDefaultBody::DependencyHolder(dependency.identity)),
            (false, crate::libraries::MemberRealization::Dispatch) if is_kotlin => Some(
                InheritedDefaultBody::DependencyInterfaceMethod(dependency.identity),
            ),
            (false, crate::libraries::MemberRealization::Dispatch) => {
                Some(InheritedDefaultBody::JavaDefaultMethod)
            }
            (false, _) => None,
        }
    }
}

/// The members `interface` declares, in its semantic declaration order, each paired with its slot
/// as seen through `receiver` (the classifier's applied view of the interface).
fn interface_surface(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    shape: &crate::libraries::LibraryType,
    interface: TypeName,
    receiver: Ty,
) -> Vec<SurfaceMember> {
    match index.classifier_declaration(interface) {
        Some(declaration) => module_surface(index, source, declaration, receiver),
        None => dependency_surface(source, shape, receiver),
    }
}

/// A current-module interface's members, read from their checked declarations in source order.
fn module_surface(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    declaration: DeclarationId,
    receiver: Ty,
) -> Vec<SurfaceMember> {
    let mut children = index
        .owned_declarations(declaration)
        .iter()
        .copied()
        .filter(|child| {
            index.declaration_header(*child).is_some_and(|header| {
                matches!(
                    header.kind,
                    DeclarationKind::Function | DeclarationKind::Property
                )
            })
        })
        .collect::<Vec<_>>();
    children.sort_by_key(|child| index.source_order(*child).unwrap_or(u32::MAX));
    let mut surface = Vec::new();
    for child in children {
        let header = index
            .declaration_header(child)
            .expect("a filtered member keeps its header");
        if header.kind == DeclarationKind::Function && index.is_suppressed_generated_callable(child)
        {
            continue;
        }
        let name = index
            .declaration_name(child)
            .expect("a module interface member has a name");
        let signature = index
            .signature(child)
            .expect("a module interface member has a checked signature");
        let (functions, properties) =
            crate::symbol_resolver::declared_member_callables(source, receiver, name).into_parts();
        let is_abstract = header.flags.has(DeclarationFlags::ABSTRACT);
        match header.kind {
            DeclarationKind::Function => {
                let callable = index
                    .callable_for_declaration(child)
                    .expect("a module function has a callable identity");
                let applied = functions
                    .overloads
                    .into_iter()
                    .find(|function| function.stable_declaration == Some(child))
                    .expect("a module interface function is visible through its provider");
                let mut parameters = signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                if let Some(receiver) = callable.shape.extension_receiver {
                    parameters.insert(
                        (callable.shape.context_parameter_count as usize).min(parameters.len()),
                        receiver.get(),
                    );
                }
                let parameter_identities = index
                    .callable_parameter_identities(callable.id, parameters.len())
                    .expect("a module function publishes every parameter identity");
                // The declared `vararg` is the last physical parameter (an extension receiver
                // leads and does not move it).
                let vararg = (0..signature.parameters.len())
                    .find(|ordinal| {
                        index
                            .callable_parameter(callable.id, *ordinal as u32)
                            .is_some_and(|parameter| parameter.flags().is_vararg())
                    })
                    .is_some_and(|ordinal| ordinal + 1 == signature.parameters.len());
                surface.push(SurfaceMember {
                    slot: Slot::Function(Box::new(applied)),
                    accessors: vec![SurfaceAccessor {
                        name: InheritedMemberName::Function(name.into()),
                        function: Some(crate::fir::ResolvedFunctionOverrideTarget::Module(
                            callable.id,
                        )),
                        parameters: parameters.into_boxed_slice(),
                        parameter_identities,
                        result: signature.result.get(),
                        suspend: header.flags.has(DeclarationFlags::SUSPEND),
                        vararg,
                        is_abstract,
                        visibility: header.visibility,
                        dependency: None,
                    }],
                    is_kotlin: true,
                });
            }
            DeclarationKind::Property => {
                let property_id = index
                    .property_for_declaration(child)
                    .expect("a module property has a property identity");
                let property = index
                    .property(property_id)
                    .expect("a module property publishes its shape");
                let applied = properties
                    .overloads
                    .into_iter()
                    .find(|property| property.stable_declaration == Some(child))
                    .expect("a module interface property is visible through its provider");
                let mut parameters = signature
                    .parameters
                    .iter()
                    .take(property.context_parameter_count as usize)
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                if let Some(receiver) = property.extension_receiver {
                    parameters.push(receiver.get());
                }
                let mut getter_identities = index
                    .property_context_parameter_identities(property_id)
                    .expect("a module property publishes its context identities")
                    .into_vec();
                if property.extension_receiver.is_some() {
                    getter_identities.push(ResolvedParameterIdentity::ExtensionReceiver);
                }
                let mut accessors = vec![SurfaceAccessor {
                    name: InheritedMemberName::PropertyGetter(name.into()),
                    function: None,
                    parameters: parameters.clone().into_boxed_slice(),
                    parameter_identities: getter_identities.clone().into_boxed_slice(),
                    result: signature.result.get(),
                    suspend: false,
                    vararg: false,
                    is_abstract,
                    visibility: header.visibility,
                    dependency: None,
                }];
                if property.mutable {
                    parameters.push(signature.result.get());
                    let mut setter_identities = getter_identities;
                    setter_identities.push(
                        index
                            .property_setter_parameter_identity(property_id)
                            .expect("a module setter publishes its value identity")
                            .clone(),
                    );
                    let visibility = index
                        .owned_declaration(child, DeclarationKind::Accessor, 1)
                        .and_then(|setter| index.declaration_header(setter))
                        .map_or(header.visibility, |setter| setter.visibility);
                    accessors.push(SurfaceAccessor {
                        name: InheritedMemberName::PropertySetter(name.into()),
                        function: None,
                        parameters: parameters.into_boxed_slice(),
                        parameter_identities: setter_identities.into_boxed_slice(),
                        result: Ty::Unit,
                        suspend: false,
                        vararg: false,
                        is_abstract,
                        visibility,
                        dependency: None,
                    });
                }
                surface.push(SurfaceMember {
                    slot: Slot::Property(Box::new(applied)),
                    accessors,
                    is_kotlin: true,
                });
            }
            _ => unreachable!("only functions and properties are kept"),
        }
    }
    surface
}

/// A dependency interface's members in the order its provider declares them, each with the
/// provider's own declaration shape and realization.
fn dependency_surface(
    source: &dyn SymbolSource,
    shape: &crate::libraries::LibraryType,
    receiver: Ty,
) -> Vec<SurfaceMember> {
    // A provider may also expose a function as a property view whose getter IS that function
    // (`fun isEmpty()` read as `isEmpty`); the view is not a declaration of its own.
    let functions = shape
        .declared_callables
        .values()
        .flat_map(|declared| declared.functions())
        .filter_map(|function| function.callable.external_identity)
        .collect::<HashSet<_>>();
    let mut surface = Vec::new();
    for name in &shape.declared_callable_order {
        let Some(declared) = shape.declared_callables.get(name) else {
            continue;
        };
        let (applied_functions, applied_properties) =
            crate::symbol_resolver::declared_member_callables(source, receiver, name).into_parts();
        assert_eq!(
            applied_functions.overloads.len(),
            declared.functions().len(),
            "an applied declaration list keeps its declarations"
        );
        assert_eq!(
            applied_properties.overloads.len(),
            declared.properties().len(),
            "an applied declaration list keeps its declarations"
        );
        for (function, applied) in declared
            .functions()
            .iter()
            .zip(applied_functions.overloads)
            .filter(|(function, _)| member_function(function))
        {
            surface.push(SurfaceMember {
                slot: Slot::Function(Box::new(applied)),
                accessors: vec![dependency_function(function)],
                is_kotlin: shape.is_kotlin,
            });
        }
        for (property, applied) in declared
            .properties()
            .iter()
            .zip(applied_properties.overloads)
            .filter(|(property, _)| {
                member_property(property)
                    && !property
                        .getter
                        .external_identity
                        .is_some_and(|getter| functions.contains(&getter))
            })
        {
            let mut accessors = vec![dependency_accessor(
                &property.getter,
                property.visibility,
                &property.context_parameter_identities,
                None,
                property.receiver.is_some(),
                InheritedMemberName::PropertyGetter(property.name.as_str().into()),
            )];
            if let Some(setter) = &property.setter {
                accessors.push(dependency_accessor(
                    setter,
                    property.setter_visibility,
                    &property.context_parameter_identities,
                    property.setter_parameter_name.as_deref(),
                    property.receiver.is_some(),
                    InheritedMemberName::PropertySetter(property.name.as_str().into()),
                ));
            }
            surface.push(SurfaceMember {
                slot: Slot::Property(Box::new(applied)),
                accessors,
                is_kotlin: shape.is_kotlin,
            });
        }
    }
    surface
}

fn dependency_realization(callable: &LibraryCallable) -> DependencyRealization {
    DependencyRealization {
        identity: callable
            .external_identity
            .expect("a dependency member has a stable external identity"),
        realization: callable.member_realization,
        has_nonvirtual_realization: callable.nonvirtual_realization.is_some(),
    }
}

fn dependency_function(function: &FunctionInfo) -> SurfaceAccessor {
    let callable = &function.callable;
    // An extension receiver leads the physical parameters; the call shape's `vararg_index` counts
    // only the logical (context and value) parameters.
    let receiver_param = function.is_extension()
        && callable.params.len() == function.call_sig.parameter_identities.len() + 1;
    SurfaceAccessor {
        name: InheritedMemberName::Function(callable.name.as_str().into()),
        function: callable
            .external_identity
            .map(crate::fir::ResolvedFunctionOverrideTarget::External),
        parameters: callable.params.clone().into_boxed_slice(),
        parameter_identities: function
            .call_sig
            .physical_parameter_identities(
                callable.params.len(),
                function.context_count,
                receiver_param.then_some(function.context_count),
            )
            .expect("a normalized function publishes every typed parameter identity"),
        result: callable.ret,
        suspend: function.flags.suspend,
        vararg: function.call_sig.vararg_index.is_some_and(|index| {
            index + usize::from(receiver_param) + 1 == callable.physical_params.len()
        }),
        is_abstract: function.flags.is_abstract,
        visibility: function.visibility,
        dependency: Some(dependency_realization(callable)),
    }
}

fn dependency_accessor(
    callable: &LibraryCallable,
    visibility: Visibility,
    context_parameter_identities: &[ResolvedParameterIdentity],
    setter_parameter_name: Option<&str>,
    extension_receiver: bool,
    name: InheritedMemberName,
) -> SurfaceAccessor {
    let mut identities = context_parameter_identities.to_vec();
    if matches!(name, InheritedMemberName::PropertySetter(_)) {
        identities.push(
            setter_parameter_name.map_or(ResolvedParameterIdentity::PropertySetterValue, |name| {
                ResolvedParameterIdentity::Source(name.into())
            }),
        );
    }
    // An associated/companion extension participates in source lookup through a receiver but its
    // accessor has no physical receiver parameter. Keep only a receiver its parameters carry.
    if extension_receiver && callable.params.len() == identities.len() + 1 {
        identities.insert(
            callable.context_count,
            ResolvedParameterIdentity::ExtensionReceiver,
        );
    }
    assert_eq!(
        callable.params.len(),
        identities.len(),
        "a normalized property accessor publishes every typed parameter identity"
    );
    SurfaceAccessor {
        name,
        function: None,
        parameters: callable.params.clone().into_boxed_slice(),
        parameter_identities: identities.into_boxed_slice(),
        result: callable.ret,
        suspend: callable.suspend,
        // A property accessor never declares a `vararg`.
        vararg: false,
        is_abstract: callable.is_abstract,
        visibility,
        dependency: Some(dependency_realization(callable)),
    }
}

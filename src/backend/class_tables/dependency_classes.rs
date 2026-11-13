//! The dispatch shape of the dependency classes a module class extends, read from the
//! declarations their providers publish.
//!
//! A class of the module whose superclass is a dependency class inherits that class's table, so
//! the module plan numbers the dependency class as well. Its shape comes from its provider, the
//! same for every module: its superclass, the members a subclass can override or reach by
//! dispatch (open, abstract and overriding ones) in declaration order, and their override edges.
//! The support unit that materializes the class fills those numbers from its methods
//! ([`crate::ir::IrFile::dependency_methods`]).

use crate::fir::{
    ExternalCallableId, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget,
};
use crate::libraries::{FnKind, FunctionInfo, LibraryType, PropKind};
use crate::symbol_source::SymbolSource;
use crate::types::{OverriddenDeclaration, SemanticCallRole, Ty, TypeName};

use super::module_classes::{
    ModuleClass, ModuleClasses, ModuleFunctionOverride, ModuleMember, ModulePropertyOverride,
};

impl ModuleClasses {
    /// Add each dependency class among `classes` and their superclasses, transitively, that
    /// `source` publishes. A class the module declares is already present and is skipped, as is a
    /// dependency class whose shape its provider does not publish completely: the plan then has
    /// no table for it, and a class extending it declines.
    pub(crate) fn add_dependency_classes(
        &mut self,
        classes: impl IntoIterator<Item = TypeName>,
        source: &dyn SymbolSource,
    ) {
        let mut pending = classes.into_iter().collect::<Vec<_>>();
        while let Some(name) = pending.pop() {
            if name == crate::types::wk::any() || self.class(name).is_some() {
                continue;
            }
            let Some(declaration) = source.classifier(name) else {
                continue;
            };
            if declaration.is_interface() || declaration.is_object() {
                continue;
            }
            let Some(class) = dependency_class(&declaration, source) else {
                continue;
            };
            pending.extend(class.superclass);
            self.insert_dependency_class(name, class);
        }
    }

    /// The dependency superclasses of the module's own classes, transitively.
    pub(crate) fn with_dependency_superclasses(mut self, source: &dyn SymbolSource) -> Self {
        let superclasses = self
            .iter()
            .filter_map(|(_, class)| class.superclass)
            .collect::<Vec<_>>();
        self.add_dependency_classes(superclasses, source);
        self
    }
}

/// The class shape `declaration` publishes, or `None` when a member it would number has no
/// provider identity.
fn dependency_class(declaration: &LibraryType, source: &dyn SymbolSource) -> Option<ModuleClass> {
    let superclass = declaration.supertypes.iter().find(|&supertype| {
        source
            .classifier(supertype)
            .is_some_and(|record| !record.is_interface())
    });
    let mut members = Vec::new();
    let mut function_overrides = Vec::new();
    let mut property_overrides = Vec::new();
    for name in &declaration.declared_callable_order {
        let Some(callables) = declaration.declared_callables.get(name) else {
            continue;
        };
        for function in callables.functions() {
            if function.kind != FnKind::Member
                || function.receiver.is_some()
                || function.classifier_static
                || !dispatched(
                    function.flags.is_final,
                    &function.callable.overridden_declarations,
                )
            {
                continue;
            }
            let callable =
                ResolvedFunctionOverrideTarget::External(function.callable.external_identity?);
            let (parameters, result) = carriers(function);
            members.push(ModuleMember::Function {
                callable,
                parameters: parameters.into(),
                result,
            });
            for overridden in function.callable.overridden_declarations.iter() {
                let (target, role, is_interface) = overridden_function(overridden, source)?;
                function_overrides.push(ModuleFunctionOverride {
                    implementation: callable,
                    overridden_owner: overridden.owner,
                    overridden: ResolvedFunctionOverrideTarget::External(target),
                    overridden_is_interface: is_interface,
                    overridden_role: role,
                });
            }
        }
        for property in callables.properties() {
            if property.kind != PropKind::Member
                || !dispatched(property.is_final, &property.getter.overridden_declarations)
            {
                continue;
            }
            let identity = property.getter.external_identity?;
            let implementation = ResolvedPropertyOverrideTarget::External(identity);
            members.push(ModuleMember::Property {
                property: implementation,
                getter: true,
                setter: property.setter.is_some(),
            });
            for overridden in property.getter.overridden_declarations.iter() {
                let (target, is_interface) = overridden_property(overridden, source)?;
                property_overrides.push(ModulePropertyOverride {
                    implementation,
                    overridden: ResolvedPropertyOverrideTarget::External(target),
                    overridden_is_interface: is_interface,
                    declared_type: overridden.ret,
                    implementation_type: property.ty,
                    declared_receiver: None,
                    implementation_receiver: None,
                    depth: 1,
                });
            }
        }
    }
    Some(ModuleClass {
        dependency: true,
        superclass: superclass.filter(|&superclass| superclass != crate::types::wk::any()),
        function_type_arities: Box::default(),
        members: members.into_boxed_slice(),
        function_overrides: function_overrides.into_boxed_slice(),
        property_overrides: property_overrides.into_boxed_slice(),
    })
}

/// Whether a member is reached by dispatch: it can be overridden, or it overrides another.
fn dispatched(is_final: bool, overridden: &[OverriddenDeclaration]) -> bool {
    !is_final || !overridden.is_empty()
}

/// The parameter and result types a function's declaration states.
fn carriers(function: &FunctionInfo) -> (Vec<Ty>, Ty) {
    let callable = &function.callable;
    (
        callable
            .declared_params
            .as_deref()
            .unwrap_or(&callable.params)
            .to_vec(),
        callable.declared_ret.unwrap_or(callable.ret),
    )
}

/// The provider identity and language role of the member function `overridden` names, and
/// whether its owner is an interface.
fn overridden_function(
    overridden: &OverriddenDeclaration,
    source: &dyn SymbolSource,
) -> Option<(ExternalCallableId, Option<SemanticCallRole>, bool)> {
    let owner = source.classifier(overridden.owner)?;
    let callables = owner.declared_callables.get(&*overridden.name)?;
    let mut matching = callables.functions().iter().filter(|function| {
        function.kind == FnKind::Member
            && function.receiver == overridden.receiver
            && carriers(function).0[..] == overridden.params[..]
    });
    let function = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some((
        function.callable.external_identity?,
        function.callable.semantic_role,
        owner.is_interface(),
    ))
}

/// The provider identity of the member property `overridden` names, and whether its owner is an
/// interface.
fn overridden_property(
    overridden: &OverriddenDeclaration,
    source: &dyn SymbolSource,
) -> Option<(ExternalCallableId, bool)> {
    let owner = source.classifier(overridden.owner)?;
    let callables = owner.declared_callables.get(&*overridden.name)?;
    let mut matching = callables
        .properties()
        .iter()
        .filter(|property| property.kind == PropKind::Member && property.receiver.is_none());
    let property = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some((property.getter.external_identity?, owner.is_interface()))
}

//! How the JVM realizes one inherited interface default a class does not override.
//!
//! Override resolution publishes the member ([`crate::fir::ResolvedInheritedDefault`]); this module
//! decides, for the active `-jvm-default` mode, whether the class writes a compatibility forwarder
//! for it and where that forwarder sends its call. When the class's applied supertype specializes
//! the member (`f(value: String)` for `I<String>`), the forwarder takes the substituted types and
//! an erased bridge carries the declaration's own descriptor; [`inherited_default_bridges`] derives
//! that bridge for the ordinary bridge pass.

use crate::ir::{Bridge, BridgeKind, BridgeParameter, IrFile};
use crate::jvm::ir_emit::JvmDefaultMode;

/// The member's name before value-class mangling: its declared name, or its accessor's.
pub(crate) fn inherited_member_declared_name(
    default: &crate::fir::ResolvedInheritedDefault,
) -> String {
    match &default.name {
        crate::fir::InheritedMemberName::Function(name) => name.to_string(),
        crate::fir::InheritedMemberName::PropertyGetter(name) => {
            crate::jvm::names::property_getter_name(name)
        }
        crate::fir::InheritedMemberName::PropertySetter(name) => {
            crate::jvm::names::property_setter_name(name)
        }
    }
}

/// The member's JVM name: the one its dependency's provider published, else its declared name with
/// the value-class hash its declaring file gives it.
pub(crate) fn inherited_member_jvm_name(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    default: &crate::fir::ResolvedInheritedDefault,
) -> String {
    let physical_name = match &default.body {
        crate::fir::InheritedDefaultBody::DependencyInterfaceMethod(declaration)
        | crate::fir::InheritedDefaultBody::DependencyHolder(declaration) => callables
            .callable(*declaration)
            .expect("an inherited dependency default has frozen backend facts")
            .physical_name
            .as_deref(),
        crate::fir::InheritedDefaultBody::Module
        | crate::fir::InheritedDefaultBody::JavaDefaultMethod => None,
    };
    if let Some(name) = physical_name {
        return name.to_string();
    }
    crate::jvm::value_classes::module_member_jvm_name(
        ir,
        &inherited_member_declared_name(default),
        &default.parameters,
        &default.result,
        default.suspend,
    )
}

/// Where a class's compatibility forwarder sends its call.
pub(crate) enum ForwarderRealization<'a> {
    /// The dependency's receiver-first holder static, its only body.
    DependencyHolder(&'a crate::libraries::NonvirtualCallRealization),
    /// This module's body under `disable`: every interface between the class and the declaration
    /// republishes it on its own holder, so the forwarder calls the holder of the direct
    /// superinterface it is inherited through.
    DispatchHolder,
    /// A Kotlin interface default method, reached with a Java-style interface `super` call. The
    /// JVM requires the named interface to be a DIRECT superinterface of this class; the body then
    /// resolves to the maximally-specific default.
    InterfaceSpecial,
}

/// The forwarder a class writes for `default` under `jvm_default`, or `None`: `no-compatibility`
/// writes none, and a Java default method never gets one.
pub(crate) fn forwarder_realization<'a>(
    default: &crate::fir::ResolvedInheritedDefault,
    callables: &'a crate::backend::CheckedBackendCallables,
    jvm_default: JvmDefaultMode,
) -> Option<ForwarderRealization<'a>> {
    use crate::fir::InheritedDefaultBody;
    match (&default.body, jvm_default) {
        (InheritedDefaultBody::DependencyHolder(declaration), _) => {
            Some(ForwarderRealization::DependencyHolder(
                callables
                    .callable(*declaration)
                    .and_then(|callable| callable.nonvirtual_realization.as_deref())
                    .expect("a dependency holder default has frozen nonvirtual facts"),
            ))
        }
        (InheritedDefaultBody::Module, JvmDefaultMode::Disable) => {
            Some(ForwarderRealization::DispatchHolder)
        }
        (InheritedDefaultBody::Module, JvmDefaultMode::Enable)
        | (
            InheritedDefaultBody::DependencyInterfaceMethod(_),
            JvmDefaultMode::Enable | JvmDefaultMode::Disable,
        ) => Some(ForwarderRealization::InterfaceSpecial),
        (
            InheritedDefaultBody::Module | InheritedDefaultBody::DependencyInterfaceMethod(_),
            JvmDefaultMode::NoCompatibility,
        )
        | (InheritedDefaultBody::JavaDefaultMethod, _) => None,
    }
}

/// The erased bridges class `cid` needs for the forwarders a specializing supertype gives it: each
/// carries the inherited declaration's descriptor and delegates to the typed forwarder, as kotlinc
/// writes `Object f(Object)` beside `String f(String)` for `I<String>`. A suspend member keeps its
/// declared CPS shape and needs none.
pub(crate) fn inherited_default_bridges(
    ir: &IrFile,
    cid: usize,
    callables: &crate::backend::CheckedBackendCallables,
    jvm_default: JvmDefaultMode,
) -> Vec<Bridge> {
    let class = &ir.classes[cid];
    if class.is_interface {
        return Vec::new();
    }
    let Some(defaults) = ir.inherited_defaults.get(&class.fq_name) else {
        return Vec::new();
    };
    let mut bridges = Vec::new();
    for default in defaults {
        if default.suspend || forwarder_realization(default, callables, jvm_default).is_none() {
            continue;
        }
        let declared = crate::jvm::value_classes::forwarded_member_types(ir, callables, default);
        let specialized =
            crate::jvm::value_classes::specialized_member_types(ir, callables, default);
        let erased = crate::jvm::method_descriptors::jvm_tys(&declared.physical_params);
        let concrete = crate::jvm::method_descriptors::jvm_tys(&specialized.physical_params);
        let erased_ret = crate::jvm::method_descriptors::jvm_declared_ty(&declared.physical_ret);
        let concrete_ret =
            crate::jvm::method_descriptors::jvm_declared_ty(&specialized.physical_ret);
        if erased == concrete && erased_ret == concrete_ret {
            continue;
        }
        let kind = match default.name {
            crate::fir::InheritedMemberName::Function(_) => BridgeKind::Function,
            crate::fir::InheritedMemberName::PropertyGetter(_) => BridgeKind::PropertyGetter,
            crate::fir::InheritedMemberName::PropertySetter(_) => BridgeKind::PropertySetter,
        };
        bridges.push(Bridge {
            kind,
            target_function: None,
            overridden_owner: Some(default.declaring_interface),
            collection_barrier: None,
            parameters: default
                .parameter_identities
                .iter()
                .zip(default.parameters.iter())
                .map(|(identity, semantic)| BridgeParameter {
                    identity: identity.clone(),
                    semantic: *semantic,
                })
                .collect(),
            name: inherited_member_jvm_name(ir, callables, default),
            erased_params: declared.physical_params,
            erased_ret: declared.physical_ret,
            concrete_params: specialized.physical_params,
            concrete_ret: specialized.physical_ret,
            target_ret: None,
            barrier_plan: None,
            special: false,
            target_name: None,
            property_implementation: None,
        });
    }
    bridges
}

/// The descriptor a `super` call names when it selects `declaration` through class `owner` and a
/// class at or above `owner` inherits that declaration as a specialized default: the typed
/// forwarder's (`echo(String)String` for `Echo<String>`), since the erased entry is a bridge that
/// dispatches virtually back to the typed one. `None` when no such forwarder exists.
pub(crate) fn specialized_super_descriptor(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    owner: crate::types::TypeName,
    declaration: crate::fir::ResolvedFunctionOverrideTarget,
    jvm_default: JvmDefaultMode,
) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    let mut current = owner;
    while seen.insert(current) {
        let class = ir.classes.iter().find(|class| class.fq_name == current)?;
        if class.is_interface {
            return None;
        }
        if let Some(default) = ir
            .inherited_defaults
            .get(&current)
            .into_iter()
            .flatten()
            .find(|default| default.function == Some(declaration))
        {
            if default.suspend || forwarder_realization(default, callables, jvm_default).is_none() {
                return None;
            }
            let declared =
                crate::jvm::value_classes::forwarded_member_types(ir, callables, default);
            let specialized =
                crate::jvm::value_classes::specialized_member_types(ir, callables, default);
            let params = crate::jvm::method_descriptors::jvm_tys(&specialized.physical_params);
            let ret = crate::jvm::method_descriptors::jvm_declared_ty(&specialized.physical_ret);
            let unchanged = params
                == crate::jvm::method_descriptors::jvm_tys(&declared.physical_params)
                && ret == crate::jvm::method_descriptors::jvm_declared_ty(&declared.physical_ret);
            return (!unchanged).then(|| crate::jvm::names::method_descriptor(&params, ret));
        }
        current = class.superclass;
    }
    None
}

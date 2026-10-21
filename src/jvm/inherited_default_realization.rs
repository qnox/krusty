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

/// The JVM name of the classifier's own realization of the member: the member's JVM name, or,
/// when the applied supertype substitutes a value class into its signature, the declared name
/// mangled over that substituted signature, as kotlinc names the fake override
/// (`foo-IQRRRT4(I)` for `foo(x: T)` through `I<Z>`). The member's own name then stays the erased
/// bridge's.
pub(crate) fn inherited_realization_jvm_name(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    default: &crate::fir::ResolvedInheritedDefault,
) -> String {
    if !crate::jvm::value_classes::substitutes_value_class(ir, default) {
        return inherited_member_jvm_name(ir, callables, default);
    }
    crate::jvm::value_classes::module_member_jvm_name(
        ir,
        &inherited_member_declared_name(default),
        &default.applied_parameters,
        &default.applied_result,
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
        let name = inherited_member_jvm_name(ir, callables, default);
        let realization = inherited_realization_jvm_name(ir, callables, default);
        if name == realization && erased == concrete && erased_ret == concrete_ret {
            continue;
        }
        // A value class the substitution carries unboxed stays the value class here: the
        // value-class pass erases it to its carrier and plans the bridge's box and unbox.
        let concrete_params = specialized
            .physical_params
            .iter()
            .zip(&specialized.value_class_params)
            .zip(default.applied_parameters.iter())
            .map(|((physical, carried), applied)| match carried {
                Some(_) => *applied,
                None => *physical,
            })
            .collect();
        let concrete_ret = match specialized.value_class_ret {
            Some(_) => default.applied_result,
            None => specialized.physical_ret,
        };
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
            target_name: (realization != name).then_some(realization),
            name,
            erased_params: declared.physical_params,
            erased_ret: declared.physical_ret,
            concrete_params,
            concrete_ret,
            target_ret: None,
            barrier_plan: None,
            special: false,
            module_name_bridge: false,
            property_implementation: None,
        });
    }
    bridges
}

/// The typed forwarder a `super` call names in place of the declaration it selected.
pub(crate) struct SpecializedSuperEntry {
    pub(crate) name: String,
    pub(crate) descriptor: String,
    /// The member as the applied supertype substitutes it, when that substitutes a value class:
    /// the forwarder is then no realization of the selected declaration's own signature (it takes
    /// and returns carriers under its own mangled name), so the call's arguments and result are
    /// represented for this signature instead: its parameters, then its result.
    pub(crate) value_class_signature: Option<(Box<[crate::types::Ty]>, crate::types::Ty)>,
}

/// The entry a `super` call names when it selects `declaration` through class `owner` and a class
/// at or above `owner` inherits that declaration as a specialized default: the typed forwarder
/// (`echo(String)String` for `Echo<String>`, `foo-IQRRRT4(I)` for `I<Z>`), since the erased entry
/// is a bridge that dispatches virtually back to the typed one. `None` when no such forwarder
/// exists.
pub(crate) fn specialized_super_entry(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    owner: crate::types::TypeName,
    declaration: crate::fir::ResolvedFunctionOverrideTarget,
    jvm_default: JvmDefaultMode,
) -> Option<SpecializedSuperEntry> {
    let mut seen = crate::fx_hash::HashSet::default();
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
            let name = inherited_realization_jvm_name(ir, callables, default);
            let unchanged = name == inherited_member_jvm_name(ir, callables, default)
                && params == crate::jvm::method_descriptors::jvm_tys(&declared.physical_params)
                && ret == crate::jvm::method_descriptors::jvm_declared_ty(&declared.physical_ret);
            return (!unchanged).then(|| SpecializedSuperEntry {
                name,
                descriptor: crate::jvm::names::method_descriptor(&params, ret),
                value_class_signature: crate::jvm::value_classes::substitutes_value_class(
                    ir, default,
                )
                .then(|| (default.applied_parameters.clone(), default.applied_result)),
            });
        }
        current = class.superclass;
    }
    None
}

/// The static a value class writes for an inherited default beside its forwarder:
/// `name-impl(carrier, parameters…)`, which boxes the carrier and calls the forwarder.
pub(crate) struct ValueClassInheritedStatic {
    pub(crate) name: String,
    /// The physical parameters after the carrier, as the classifier's applied supertype gives them.
    pub(crate) parameters: Vec<crate::types::Ty>,
    /// The checked parameters after the carrier: the member as the applied supertype substitutes
    /// it, which a call's arguments are represented for.
    pub(crate) declared_parameters: Box<[crate::types::Ty]>,
    pub(crate) result: crate::types::Ty,
}

/// The static a value class realizes `default` with under `jvm_default`, or `None` when the class
/// writes no forwarder for it. A suspend member keeps its CPS shape and is left to interface
/// dispatch.
pub(crate) fn value_class_inherited_static(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    default: &crate::fir::ResolvedInheritedDefault,
    jvm_default: JvmDefaultMode,
) -> Option<ValueClassInheritedStatic> {
    if default.suspend {
        return None;
    }
    forwarder_realization(default, callables, jvm_default)?;
    let types = crate::jvm::value_classes::specialized_member_types(ir, callables, default);
    Some(ValueClassInheritedStatic {
        name: crate::jvm::value_classes::inherited_member_impl_name(
            &inherited_member_declared_name(default),
            &inherited_realization_jvm_name(ir, callables, default),
        ),
        parameters: crate::jvm::method_descriptors::jvm_tys(&types.physical_params),
        declared_parameters: default.applied_parameters.clone(),
        result: crate::jvm::method_descriptors::jvm_declared_ty(&types.physical_ret),
    })
}

//! Bridge-method derivation.
//!
//! A bridge is not a Kotlin declaration: it exists because the JVM dispatches on an ERASED descriptor,
//! so an override whose descriptor differs from the supertype's (generic, covariant, mangled, or
//! physically renamed by a mapped interface) is invisible through a supertype reference without a
//! synthetic `ACC_BRIDGE` method carrying the supertype's descriptor. Descriptors, erasure, accessor
//! names and `@JvmName` mangling are all JVM realizations of a declaration, so the derivation belongs
//! here rather than in common lowering, which only records what the source declares.
//!
//! The pass consumes exact override edges selected while Pass 1's declaration providers were live. It
//! runs before the value-class pass so an existing bridge's target is retargeted/renamed with the
//! mangled name once mangling is known.

use crate::ir::{
    Bridge, BridgeAccessorRole, BridgeKind, BridgeParameter, BridgePropertyImplementation, IrFile,
};
use crate::jvm::backend::SkipReason;
use crate::jvm::names::method_descriptor;
use crate::names::{property_getter_name, property_setter_name};
use crate::types::{stored_value_ty, Ty};

/// Every bridge family this class needs, appended to `IrClass::bridges`.
pub(super) fn derive_bridges(
    ir: &mut IrFile,
    classpath: &crate::jvm::classpath::Classpath,
    callables: &crate::backend::CheckedBackendCallables,
    override_results: &crate::jvm::override_results::OverrideResults,
    argument_arrays: &mut crate::jvm::function_argument_arrays::FunctionArgumentArrays,
) -> Result<(), SkipReason> {
    for cid in 0..ir.classes.len() {
        // Source-declared classes, declaration-owned enum-entry subclasses, and anonymous classes
        // copied at a reified inline call. Lambdas and callable-reference classes have
        // emitter-chosen supertypes and no Kotlin override edges. An enum-entry body contains
        // source override declarations. A reified copy carries those edges retargeted at its
        // specialized methods.
        if !crate::jvm::override_results::realizes_overrides(ir, cid) {
            continue;
        }
        let first = ir.classes[cid].bridges.len();
        let mut order = Vec::new();
        superclass_method_bridges(
            ir,
            cid,
            classpath,
            callables,
            override_results,
            argument_arrays,
            &mut order,
        )?;
        property_bridges(ir, cid, classpath, callables, override_results, &mut order)?;
        declaration_order(&mut ir.classes[cid].bridges[first..], order);
    }
    Ok(())
}

/// kotlinc's bridge lowering walks the class's declarations in order and adds each one's bridges
/// as it goes, so a property's bridges and a function's interleave by where the overriding members
/// are declared. `order` holds each bridge's member position; an inherited implementation has none
/// and keeps its bridges after the class's own, in derivation order.
fn declaration_order(bridges: &mut [Bridge], order: Vec<u32>) {
    assert_eq!(
        bridges.len(),
        order.len(),
        "one position per derived bridge"
    );
    let mut keyed = order
        .into_iter()
        .zip(bridges.iter().cloned())
        .collect::<Vec<_>>();
    keyed.sort_by_key(|(position, _)| *position);
    for (slot, (_, bridge)) in bridges.iter_mut().zip(keyed) {
        *slot = bridge;
    }
}

/// kotlinc's `BridgeLowering` blacklists the special bridge of every overridden declaration in a
/// Kotlin superclass: a Kotlin class that first takes `size` from a collection already carries the
/// final `size()` bridge to its `getSize()`, including through an inherited fake override, so a
/// subclass must not declare it again (`IncompatibleClassChangeError: overrides final method`). The
/// override edges name only the declarations that spell the member, so walk the superclass chain
/// for a Kotlin class whose classfile already has that final bridge.
fn inherits_final_special_bridge(
    ir: &IrFile,
    cid: usize,
    classpath: &crate::jvm::classpath::Classpath,
    name: &str,
    descriptor: &str,
) -> bool {
    let mut next = ir.classes[cid].superclass;
    loop {
        if let Some(module_class) = ir.classes.iter().find(|class| class.fq_name == next) {
            next = module_class.superclass;
            continue;
        }
        let Some(info) = classpath.find_name(next) else {
            return false;
        };
        if info.meta.class_kind.is_some()
            && info.methods.iter().any(|method| {
                method.name == name
                    && method.descriptor == descriptor
                    && is_inherited_special_bridge(method)
            })
        {
            return true;
        }
        match info.super_class {
            Some(superclass) => next = superclass,
            None => return false,
        }
    }
}

/// A superclass method that IS the special bridge a subclass inherits: a final `ACC_BRIDGE` instance
/// method. kotlinc publishes that bridge `public`, so a `protected` one is inherited too, but a
/// private or static method with the same signature is not inherited and owns no bridge, and a
/// package-private one is not treated as reachable across the packages a classpath superclass
/// can sit in.
fn is_inherited_special_bridge(method: &crate::jvm::classreader::MethodSig) -> bool {
    method.is_bridge()
        && method.is_final()
        && !method.is_static()
        && !method.is_private()
        && (method.is_public() || method.is_protected())
}

fn external_method_name(
    callables: &crate::backend::CheckedBackendCallables,
    target: crate::fir::ExternalCallableId,
) -> Result<String, SkipReason> {
    let callable = callables.callable(target).ok_or(SkipReason::Bridges)?;
    Ok(crate::jvm::names::mapped_builtin_virtual_name(
        callable.physical_owner,
        callable.physical_name(),
        &callable.descriptor,
    )
    .to_owned())
}

/// A same-module override can see the member. A dependency's `internal` member is overridable
/// only from a friend module; otherwise the Kotlin name in this class is a different slot.
fn sees_internal_member(
    classpath: &crate::jvm::classpath::Classpath,
    owner: crate::types::TypeName,
    external: bool,
) -> bool {
    !external || classpath.grants_internal_access(owner)
}

fn function_is_internal(ir: &IrFile, function: crate::ir::FunId) -> bool {
    ir.method_visibility(function) == crate::types::Visibility::Internal
}

fn module_function_name(ir: &IrFile, callable: crate::fir::CallableId) -> Option<String> {
    ir.checked_callable_functions
        .get(&callable)
        .map(|function| ir.functions[*function as usize].name.clone())
}

fn function_target_is_internal(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    target: crate::fir::ResolvedFunctionOverrideTarget,
) -> bool {
    match target {
        crate::fir::ResolvedFunctionOverrideTarget::Module(callable) => {
            ir.checked_callable_functions
                .get(&callable)
                .is_some_and(|function| function_is_internal(ir, *function))
                || ir
                    .referenced_module_callables
                    .get(&callable)
                    .is_some_and(super::internal_names::module_callable_is_mangled)
        }
        crate::fir::ResolvedFunctionOverrideTarget::External(callable) => callables
            .callable(callable)
            .is_some_and(|fact| fact.visibility == crate::types::Visibility::Internal),
    }
}

fn property_accessor(
    ir: &IrFile,
    property: crate::fir::PropertyId,
    setter: bool,
) -> Option<crate::ir::FunId> {
    let checked = ir.checked_properties.get(&property)?;
    let class = ir.classes.get(checked.class? as usize)?;
    let member = class
        .properties
        .iter()
        .find(|member| member.name == checked.name)?;
    if setter {
        member.setter
    } else {
        member.getter
    }
}

fn property_member<'a>(
    ir: &'a IrFile,
    property: crate::fir::PropertyId,
) -> Option<&'a crate::ir::IrProperty> {
    let checked = ir.checked_properties.get(&property)?;
    let class = ir.classes.get(checked.class? as usize)?;
    class
        .properties
        .iter()
        .find(|member| member.name == checked.name)
}

fn property_target_is_internal(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    target: crate::fir::ResolvedPropertyOverrideTarget,
    setter: bool,
) -> bool {
    match target {
        crate::fir::ResolvedPropertyOverrideTarget::Module(property) => {
            // A plain backing-field property has no accessor function. Its getter's visibility is
            // the property's; a setter may narrow that on its own declaration.
            if let Some(member) = property_member(ir, property) {
                return if setter {
                    member.setter_visibility == crate::types::Visibility::Internal
                } else {
                    member.visibility == crate::types::Visibility::Internal
                };
            }
            property_accessor(ir, property, setter)
                .is_some_and(|function| function_is_internal(ir, function))
                || ir
                    .referenced_module_properties
                    .get(&property)
                    .is_some_and(|property| {
                        if setter {
                            super::internal_names::module_setter_is_mangled(property)
                        } else {
                            super::internal_names::module_getter_is_mangled(property)
                        }
                    })
        }
        crate::fir::ResolvedPropertyOverrideTarget::External(callable) => callables
            .callable(callable)
            .is_some_and(|fact| fact.visibility == crate::types::Visibility::Internal),
    }
}

/// The common-IR function implementing `edge`: a compiler-generated forwarder's own function, or the
/// function lowered from the selected source declaration.
pub(super) fn implementation_function(
    ir: &IrFile,
    edge: &crate::ir::IrFunctionOverride,
) -> Option<crate::ir::FunId> {
    edge.implementation_function
        .or_else(|| match edge.implementation {
            crate::fir::ResolvedFunctionOverrideTarget::Module(declaration) => {
                ir.checked_callable_functions.get(&declaration).copied()
            }
            crate::fir::ResolvedFunctionOverrideTarget::External(_) => None,
        })
}

/// A method overriding a superclass method with a different erased signature (a generic or covariant
/// override) needs an `ACC_BRIDGE` method carrying the SUPERCLASS's descriptor that delegates to the
/// concrete override — without it a call through a base reference resolves to a method that is not there.
fn superclass_method_bridges(
    ir: &mut IrFile,
    cid: usize,
    classpath: &crate::jvm::classpath::Classpath,
    callables: &crate::backend::CheckedBackendCallables,
    override_results: &crate::jvm::override_results::OverrideResults,
    argument_arrays: &mut crate::jvm::function_argument_arrays::FunctionArgumentArrays,
    order: &mut Vec<u32>,
) -> Result<(), SkipReason> {
    let internal_name = ir.classes[cid].fq_name;
    let edges = ir
        .function_overrides
        .get(&internal_name)
        .cloned()
        .unwrap_or_default();
    for edge in edges {
        let own_fid = implementation_function(ir, &edge);
        if edge.implementation_owner == internal_name
            && own_fid.is_none_or(|function| !ir.classes[cid].methods.contains(&function))
        {
            continue;
        }
        // The overridden declaration's own shape, unapplied, as the frontend recorded it on the
        // edge: bridge erasure applies to that, not to the call-site view through this class.
        let mut declared_parameters = edge.declared_parameters.clone();
        let mut declared_result = edge.declared_result;
        // An overridden declaration whose primitive result is realized as its wrapper is reached
        // through that wrapper. Dependency representation comes only from the exact frozen
        // callable fact; a missing fact fails this pass instead of falling back to the semantic
        // result.
        match edge.overridden {
            crate::fir::ResolvedFunctionOverrideTarget::Module(callable) => {
                if override_results
                    .boxed_callable_result(ir, callable)
                    .is_some()
                {
                    declared_result = Ty::nullable(declared_result);
                }
            }
            crate::fir::ResolvedFunctionOverrideTarget::External(target) => {
                if crate::jvm::override_results::external_boxed_result(
                    callables,
                    target,
                    edge.declared_result,
                )?
                .is_some()
                {
                    declared_result = Ty::nullable(declared_result);
                }
            }
        }
        let mut base_params = declared_parameters
            .iter()
            .copied()
            .map(bridge_erasure)
            .collect::<Vec<_>>();
        let mut base_ret = bridge_erasure(declared_result);
        let mut concrete_params = own_fid
            .map(|function| ir.functions[function as usize].params.clone())
            .unwrap_or_else(|| edge.implementation_parameters.clone());
        // The implementation's JVM result: the wrapper where its primitive result is boxed, as it
        // is for an implementation inherited from another file's declaration.
        let mut concrete_ret = match (own_fid, edge.implementation) {
            (Some(function), _) => override_results.physical_result(ir, function),
            (None, crate::fir::ResolvedFunctionOverrideTarget::Module(callable))
                if override_results
                    .boxed_callable_result(ir, callable)
                    .is_some() =>
            {
                Ty::nullable(edge.implementation_result)
            }
            (None, _) => edge.implementation_result,
        };
        // A bridge carries the overridden declaration's signature, so kotlinc names its parameters
        // after that declaration's, not the override's.
        let mut parameter_identities = edge.overridden_parameter_identities.clone();
        let suspend_function_supertype =
            crate::libraries::function_classifiers::classifier(edge.overridden_owner)
                .is_some_and(|function| function.is_suspend() && !function.is_reflective());
        if suspend_function_supertype {
            // `SuspendFunctionN.invoke` is realized as `Function{N+1}.invoke`, whose continuation is
            // the ordinary type parameter `P{N+1}`: its erased descriptor takes `Object` where the
            // CPS override takes `Continuation`, so the bridge always exists and is built here in
            // its final CPS form (kotlinc's `invoke(Object)Object` casting to `Continuation`).
            base_params.push(Ty::nullable(Ty::obj("kotlin/Any")));
            base_ret = Ty::nullable(Ty::obj("kotlin/Any"));
            concrete_params.push(Ty::obj("kotlin/coroutines/Continuation"));
            concrete_ret = base_ret;
            declared_parameters.push(Ty::obj("kotlin/coroutines/Continuation"));
            parameter_identities.push(crate::fir::ResolvedParameterIdentity::SuspendCompletion);
        }
        let packed_arguments =
            crate::libraries::function_classifiers::classifier(edge.overridden_owner)
                .is_some_and(|function| !function.is_reflective())
                && crate::jvm::names::uses_function_n(base_params.len());
        if packed_arguments {
            base_params = vec![Ty::array(Ty::nullable(Ty::obj("kotlin/Any")))];
        }
        let own_params = concrete_params
            .iter()
            .copied()
            .map(bridge_erasure)
            .collect::<Vec<_>>();
        let own_ret = bridge_erasure(concrete_ret);
        let bridge_name = match edge.overridden {
            crate::fir::ResolvedFunctionOverrideTarget::Module(callable) => {
                module_function_name(ir, callable).unwrap_or_else(|| edge.name.clone())
            }
            crate::fir::ResolvedFunctionOverrideTarget::External(target) => {
                external_method_name(callables, target)?
            }
        };
        let target_name = if let Some(function) = own_fid {
            ir.functions[function as usize].name.clone()
        } else {
            match edge.implementation {
                crate::fir::ResolvedFunctionOverrideTarget::Module(callable) => {
                    module_function_name(ir, callable).unwrap_or_else(|| edge.name.clone())
                }
                crate::fir::ResolvedFunctionOverrideTarget::External(target) => {
                    external_method_name(callables, target)?
                }
            }
        };
        // A public override of an internal member keeps the Kotlin name. The internal declaration's
        // JVM name gains `$<module>` after this pass, so the bridge has to exist even while the two
        // names still match. Whether that name difference is the internal slot, rather than a mapped
        // builtin (`size` / `getSize`), is the declaration's visibility. A module that cannot see an
        // internal member does not publish its slot, even when `override` is required by another
        // visible declaration of the same Kotlin name. A public JVM name is never that slot.
        let sees_internal = sees_internal_member(
            classpath,
            edge.overridden_owner,
            matches!(
                edge.overridden,
                crate::fir::ResolvedFunctionOverrideTarget::External(_)
            ),
        );
        let overridden_internal = function_target_is_internal(ir, callables, edge.overridden);
        if !sees_internal && overridden_internal {
            continue;
        }
        let internal_name_bridge = sees_internal
            && overridden_internal
            && !function_target_is_internal(ir, callables, edge.implementation);
        let special = matches!(
            edge.overridden,
            crate::fir::ResolvedFunctionOverrideTarget::External(_)
        ) && bridge_name != edge.name
            && !overridden_internal;
        if special && edge.has_kotlin_superclass_override {
            continue;
        }
        if bridge_name != target_name
            && inherits_final_special_bridge(
                ir,
                cid,
                classpath,
                &bridge_name,
                &method_descriptor(&base_params, base_ret),
            )
        {
            continue;
        }
        crate::trace_compiler!(
            "bridges",
            "stable override class={internal_name} implementation={:?} owner={} overridden={:?} owner={} source={} bridge={} target={} declared={base_params:?}->{base_ret:?} concrete={own_params:?}->{own_ret:?}",
            edge.implementation,
            edge.implementation_owner,
            edge.overridden,
            edge.overridden_owner,
            edge.name,
            bridge_name,
            target_name,
        );
        // Bridge necessity is a JVM-descriptor question. Semantic types may still differ only in
        // arguments owned by different declarations (`Iterator<T-super>` vs `Iterator<T-class>`),
        // while erasure gives both methods the exact same descriptor. Comparing `Ty` structurally
        // in that case manufactured a same-name/same-descriptor bridge which could only call itself.
        let value_class_return_difference = base_ret != own_ret
            && [base_ret, own_ret].into_iter().any(|result| {
                result
                    .non_null()
                    .obj_internal()
                    .is_some_and(|name| crate::jvm::value_classes::is_boxed_value_class(ir, name))
            });
        if method_descriptor(&base_params, base_ret) == method_descriptor(&own_params, own_ret)
            && bridge_name == target_name
            && !value_class_return_difference
            && !internal_name_bridge
        {
            continue;
        }
        // A suspend override: the CPS rewrite gives BOTH the base declaration and the override the
        // same trailing `Continuation` parameter and an `Object` return, so a RETURN-only erasure
        // difference vanishes — no bridge exists to build (probed: kotlinc emits a single
        // `byId(int, Continuation)` for `Repo<T>.byId(Int): T?`). A VALUE-parameter difference or a
        // value-class-mangled target still needs a bridge. Record it in declared form here so the
        // value-class pass can apply its one canonical mangle/box/unbox realization; the suspend pass
        // later converts both bridge sides to the CPS descriptor.
        if !suspend_function_supertype
            && (edge.suspend || own_fid.is_some_and(|function| ir.suspend_funs.contains(&function)))
        {
            let vc_ret = own_ret
                .non_null()
                .obj_internal()
                .is_some_and(|n| crate::jvm::value_classes::is_boxed_value_class(ir, n));
            if base_params == own_params && !vc_ret && !internal_name_bridge {
                continue;
            }
        }
        if let Some(existing) = ir.classes[cid].bridges.iter_mut().find(|bridge| {
            bridge.name == bridge_name
                && bridge.erased_params == base_params
                && bridge.erased_ret == base_ret
        }) {
            // Several resolved override edges erase to one physical bridge. The nearest
            // declaration (`AbstractCollection.contains`) need not be the collection member that
            // carries the type-safe role; a farther edge (`Collection.contains`) does. The role
            // is already on that edge, so the bridge keeps it.
            if existing.collection_barrier.is_none() {
                existing.collection_barrier = edge.collection_barrier;
            }
            continue;
        }
        let target_name =
            (internal_name_bridge || bridge_name != target_name).then_some(target_name);
        if parameter_identities.len() != concrete_params.len()
            || declared_parameters.len() != concrete_params.len()
        {
            return Err(SkipReason::Bridges);
        }
        let parameters = parameter_identities
            .into_iter()
            .zip(declared_parameters)
            .map(|(identity, semantic)| BridgeParameter { identity, semantic })
            .collect();
        order.push(
            own_fid
                .and_then(|function| ir.fn_source_order.get(&function).copied())
                .unwrap_or(u32::MAX),
        );
        ir.classes[cid].bridges.push(Bridge {
            kind: BridgeKind::Function,
            target_function: own_fid,
            overridden_owner: Some(edge.overridden_owner),
            collection_barrier: edge.collection_barrier,
            parameters,
            name: bridge_name,
            erased_params: base_params,
            erased_ret: base_ret,
            concrete_params,
            concrete_ret,
            target_ret: None,
            barrier_plan: None,
            special,
            module_name_bridge: internal_name_bridge,
            target_name,
            property_implementation: None,
        });
        if packed_arguments {
            let class = &ir.classes[cid];
            let bridge = class.bridges.last().expect("the bridge just pushed");
            argument_arrays.record(class.fq_name_id(), bridge);
        }
    }
    Ok(())
}

/// A property overriding a supertype property with a different erased type (a covariant override
/// `from: Sub` over `from: Super`, or a generic `val x: T` erased to `Object` overridden with a concrete
/// type) needs a synthetic `getX()` returning the supertype's erased type that delegates to the concrete
/// getter — else a call through a supertype reference resolves to a getter that does not exist. A `var`
/// override needs the matching `setX(erased)`, else a write through the supertype silently no-ops.
fn property_bridges(
    ir: &mut IrFile,
    cid: usize,
    classpath: &crate::jvm::classpath::Classpath,
    callables: &crate::backend::CheckedBackendCallables,
    override_results: &crate::jvm::override_results::OverrideResults,
    order: &mut Vec<u32>,
) -> Result<(), SkipReason> {
    let internal_name = ir.classes[cid].fq_name;
    let edges = ir
        .property_overrides
        .get(&internal_name)
        .cloned()
        .unwrap_or_default();
    for edge in edges {
        let implementation_property = match edge.implementation {
            crate::fir::ResolvedPropertyOverrideTarget::Module(declaration) => {
                ir.checked_properties.get(&declaration)
            }
            crate::fir::ResolvedPropertyOverrideTarget::External(_) => None,
        };
        let declared_here = match edge.implementation_getter {
            Some(getter) => ir.classes[cid].methods.contains(&getter),
            None => {
                implementation_property.is_some_and(|property| property.class == Some(cid as u32))
            }
        };
        if edge.implementation_owner == internal_name && !declared_here {
            continue;
        }
        let source_getter = property_getter_name(&edge.name);
        let bridge_getter = match edge.overridden {
            crate::fir::ResolvedPropertyOverrideTarget::Module(property) => {
                property_accessor(ir, property, false)
                    .map(|function| ir.functions[function as usize].name.clone())
                    .unwrap_or_else(|| source_getter.clone())
            }
            crate::fir::ResolvedPropertyOverrideTarget::External(target) => {
                external_method_name(callables, target)?
            }
        };
        let target_getter = if let Some(function) = edge.implementation_getter {
            ir.functions[function as usize].name.clone()
        } else {
            match edge.implementation {
                crate::fir::ResolvedPropertyOverrideTarget::Module(property) => {
                    property_accessor(ir, property, false)
                        .map(|function| ir.functions[function as usize].name.clone())
                        .unwrap_or_else(|| source_getter.clone())
                }
                crate::fir::ResolvedPropertyOverrideTarget::External(target) => {
                    external_method_name(callables, target)?
                }
            }
        };
        let sees_internal = sees_internal_member(
            classpath,
            edge.overridden_owner,
            matches!(
                edge.overridden,
                crate::fir::ResolvedPropertyOverrideTarget::External(_)
            ),
        );
        let overridden_getter_internal =
            property_target_is_internal(ir, callables, edge.overridden, false);
        let internal_getter_bridge = sees_internal
            && overridden_getter_internal
            && !property_target_is_internal(ir, callables, edge.implementation, false);
        let internal_setter_bridge = sees_internal
            && edge.overridden_mutable
            && edge.implementation_mutable
            && property_target_is_internal(ir, callables, edge.overridden, true)
            && !property_target_is_internal(ir, callables, edge.implementation, true);
        // Same rule as a method: an internal accessor belongs to a friend. Visibility decides
        // that, not a `$` in the JVM name. Another visible property of the Kotlin name does not
        // make this class the internal slot's override.
        if !sees_internal && overridden_getter_internal {
            continue;
        }
        crate::trace_compiler!(
            "bridges",
            "stable property override class={internal_name} implementation={:?} owner={} overridden={:?} owner={} source={} bridge={bridge_getter} target={target_getter} covering={:?}",
            edge.implementation,
            edge.implementation_owner,
            edge.overridden,
            edge.overridden_owner,
            edge.name,
            edge.has_kotlin_superclass_override,
        );
        let declared_receiver = edge.declared_receiver.map(bridge_erasure);
        let implementation_receiver = edge.implementation_receiver.map(bridge_erasure);
        let getter_results = PropertyGetterResults::of(ir, callables, &edge, override_results)?;
        if crate::jvm::names::same_type_descriptor(
            getter_results.declared,
            getter_results.implementation,
        ) && match (declared_receiver, implementation_receiver) {
            (None, None) => true,
            (Some(declared), Some(implementation)) => {
                crate::jvm::names::same_type_descriptor(declared, implementation)
            }
            _ => false,
        } && bridge_getter == target_getter
            && !internal_getter_bridge
            && !internal_setter_bridge
        {
            continue;
        }
        let getter_special = matches!(
            edge.overridden,
            crate::fir::ResolvedPropertyOverrideTarget::External(_)
        ) && bridge_getter != source_getter
            && !overridden_getter_internal;
        if getter_special && edge.has_kotlin_superclass_override {
            continue;
        }
        let bridge_descriptor = method_descriptor(
            &declared_receiver.into_iter().collect::<Vec<_>>(),
            bridge_erasure(getter_results.declared),
        );
        if bridge_getter != target_getter
            && inherits_final_special_bridge(ir, cid, classpath, &bridge_getter, &bridge_descriptor)
        {
            continue;
        }
        // A compiler-generated accessor (a delegation forwarder) takes its own place among the
        // class's members, as a forwarding function does; the property it implements is another
        // class's declaration.
        let position = match edge.implementation_getter {
            Some(getter) if declared_here => ir.fn_source_order.get(&getter).copied(),
            _ => implementation_property
                .filter(|_| declared_here)
                .map(|property| property.source_order),
        }
        .unwrap_or(u32::MAX);
        let pushed_before = ir.classes[cid].bridges.len();
        if let (Some(declared), Some(implementation)) =
            (edge.declared_receiver, edge.implementation_receiver)
        {
            push_member_extension_accessor_bridges(ir, cid, &edge, declared, implementation);
            order.resize(
                order.len() + ir.classes[cid].bridges.len() - pushed_before,
                position,
            );
            continue;
        }
        let setter_target = edge
            .implementation_setter
            .map(|function| ir.functions[function as usize].name.clone())
            .unwrap_or_else(|| property_setter_name(&edge.name));
        push_property_bridge(
            ir,
            cid,
            &edge,
            getter_results,
            PropertyBridgeNames {
                getter_name: bridge_getter,
                getter_target: target_getter,
                setter_target,
                internal_getter: internal_getter_bridge,
                internal_setter: internal_setter_bridge,
            },
        );
        order.resize(
            order.len() + ir.classes[cid].bridges.len() - pushed_before,
            position,
        );
    }
    Ok(())
}

/// A member-extension property's accessors are ordinary methods taking the receiver
/// (`getX(receiver)`, `setX(receiver, value)`), so their bridges have the function shape: the
/// overridden declaration's erased receiver and type delegating to the implementation's accessors.
fn push_member_extension_accessor_bridges(
    ir: &mut IrFile,
    cid: usize,
    edge: &crate::ir::IrPropertyOverride,
    declared_receiver: Ty,
    implementation_receiver: Ty,
) {
    let own = |accessor: Option<u32>| accessor.filter(|f| ir.classes[cid].methods.contains(f));
    let (getter, setter) = (
        own(edge.implementation_getter),
        own(edge.implementation_setter),
    );
    let receiver = BridgeParameter {
        identity: crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
        semantic: declared_receiver,
    };
    let accessor = |name, target_function, erased_ret, concrete_ret| Bridge {
        kind: BridgeKind::Function,
        target_function,
        overridden_owner: None,
        collection_barrier: None,
        parameters: vec![receiver.clone()],
        name,
        erased_params: vec![bridge_erasure(declared_receiver)],
        erased_ret,
        concrete_params: vec![implementation_receiver],
        concrete_ret,
        target_ret: None,
        barrier_plan: None,
        special: false,
        module_name_bridge: false,
        target_name: None,
        property_implementation: None,
    };
    let mut accessors = vec![accessor(
        property_getter_name(&edge.name),
        getter,
        bridge_erasure(edge.declared_type),
        edge.implementation_type,
    )];
    if edge.overridden_mutable && edge.implementation_mutable {
        let mut setter = accessor(property_setter_name(&edge.name), setter, Ty::Unit, Ty::Unit);
        setter.parameters.push(BridgeParameter {
            identity: crate::fir::ResolvedParameterIdentity::PropertySetterValue,
            semantic: edge.declared_type,
        });
        setter
            .erased_params
            .push(bridge_erasure(edge.declared_type));
        setter.concrete_params.push(edge.implementation_type);
        accessors.push(setter);
    }
    for bridge in accessors {
        if !ir.classes[cid].bridges.iter().any(|existing| {
            existing.name == bridge.name && existing.erased_params == bridge.erased_params
        }) {
            ir.classes[cid].bridges.push(bridge);
        }
    }
}

/// `getV$main` or `getP-HASH$lib1` shares its suffix with `setV$main` / `setP-HASH$lib1`.
fn setter_name_with_getter_suffix(property: &str, getter_jvm: &str) -> String {
    let getter = property_getter_name(property);
    let setter = property_setter_name(property);
    match getter_jvm.strip_prefix(&getter) {
        Some(rest) if rest.starts_with('$') || rest.starts_with('-') => format!("{setter}{rest}"),
        _ => setter,
    }
}

/// The `get<X>()` bridge (and, for a `var` override, the `set<X>()` one). A bridge already recorded
/// under the accessor's name and descriptor wins — the first supertype in the walk is the nearest
/// one. Each delegates to the implementation's own accessor function when this class declares it.
struct PropertyBridgeNames {
    getter_name: String,
    getter_target: String,
    setter_target: String,
    internal_getter: bool,
    internal_setter: bool,
}

fn push_property_bridge(
    ir: &mut IrFile,
    cid: usize,
    edge: &crate::ir::IrPropertyOverride,
    getter_results: PropertyGetterResults,
    names: PropertyBridgeNames,
) {
    let PropertyBridgeNames {
        getter_name,
        getter_target,
        setter_target,
        internal_getter: internal_getter_bridge,
        internal_setter: internal_setter_bridge,
    } = names;
    let own = |accessor: Option<u32>| accessor.filter(|f| ir.classes[cid].methods.contains(f));
    let (getter, setter) = (
        own(edge.implementation_getter),
        own(edge.implementation_setter),
    );
    // A boxed getter can need one bridge per overridden slot (`getSize()I` for an `Int` slot and
    // `getSize()Object` for a type parameter's), so a recorded bridge covers this one only when its
    // descriptor is the same.
    let has_getter = ir.classes[cid].bridges.iter().any(|b| {
        b.name == getter_name
            && b.erased_params.is_empty()
            && crate::jvm::names::same_type_descriptor(
                bridge_erasure(b.erased_ret),
                bridge_erasure(getter_results.declared),
            )
    });
    if !has_getter {
        let target_name =
            (internal_getter_bridge || getter_name != getter_target).then_some(getter_target);
        let special = !internal_getter_bridge && getter_name != property_getter_name(&edge.name);
        ir.classes[cid].bridges.push(Bridge {
            kind: BridgeKind::PropertyGetter,
            target_function: getter,
            overridden_owner: None,
            collection_barrier: None,
            parameters: Vec::new(),
            name: getter_name.clone(),
            erased_params: vec![],
            erased_ret: getter_results.declared,
            concrete_params: vec![],
            concrete_ret: getter_results.implementation,
            target_ret: None,
            barrier_plan: None,
            special,
            module_name_bridge: internal_getter_bridge,
            target_name,
            property_implementation: property_implementation(edge, BridgeAccessorRole::Getter),
        });
    }
    if !(edge.overridden_mutable && edge.implementation_mutable) {
        return;
    }
    // The setter keeps the declared types: over a slot whose getter only differs by the boxed
    // result, the setter's descriptor is already the implementation's own.
    let setter_param = bridge_erasure(edge.declared_type);
    // The getter bridge already uses the overridden accessor's JVM name (`getV$main`). The setter
    // has no separate external identity on the edge, so it takes that same suffix (`setV$main`).
    // A same-module override still spells the Kotlin accessor here; the suffix pass renames the
    // bridge afterwards. It has to be recorded now, with `target_name` equal to that Kotlin name,
    // or the value-class retain drops it as a duplicate of the public setter.
    let sname = setter_name_with_getter_suffix(&edge.name, &getter_name);
    let has_setter = (!internal_setter_bridge
        && crate::jvm::names::same_type_descriptor(setter_param, edge.implementation_type))
        || ir.classes[cid].bridges.iter().any(|b| {
            b.name == sname
                && b.erased_params.len() == 1
                && crate::jvm::names::same_type_descriptor(
                    bridge_erasure(b.erased_params[0]),
                    setter_param,
                )
        });
    if !has_setter {
        let setter_target_name =
            (internal_setter_bridge || sname != setter_target).then_some(setter_target);
        ir.classes[cid].bridges.push(Bridge {
            kind: BridgeKind::PropertySetter,
            target_function: setter,
            overridden_owner: None,
            collection_barrier: None,
            parameters: vec![BridgeParameter {
                identity: crate::fir::ResolvedParameterIdentity::PropertySetterValue,
                semantic: edge.declared_type,
            }],
            name: sname,
            erased_params: vec![edge.declared_type],
            erased_ret: Ty::Unit,
            concrete_params: vec![edge.implementation_type],
            concrete_ret: Ty::Unit,
            target_ret: None,
            barrier_plan: None,
            special: false,
            module_name_bridge: internal_setter_bridge,
            target_name: setter_target_name,
            property_implementation: property_implementation(edge, BridgeAccessorRole::Setter),
        });
    }
}

/// The JVM results of the two getters a property override edge joins: the declared types, or their
/// wrappers where a scalar getter result over a reference-returning overridden getter is boxed (see
/// `jvm::override_results`). A setter keeps the declared types.
#[derive(Clone, Copy)]
struct PropertyGetterResults {
    declared: Ty,
    implementation: Ty,
}

impl PropertyGetterResults {
    fn of(
        ir: &IrFile,
        callables: &crate::backend::CheckedBackendCallables,
        edge: &crate::ir::IrPropertyOverride,
        override_results: &crate::jvm::override_results::OverrideResults,
    ) -> Result<Self, SkipReason> {
        let boxes = |property, declared| match property {
            crate::fir::ResolvedPropertyOverrideTarget::Module(property) => Ok(override_results
                .boxed_property_result(ir, property)
                .is_some()),
            crate::fir::ResolvedPropertyOverrideTarget::External(_) => {
                crate::jvm::override_results::external_boxed_getter(callables, property, declared)
            }
        };
        let physical = |ty: Ty, boxed: bool| if boxed { Ty::nullable(ty) } else { ty };
        let implementation_boxed = match edge.implementation_getter {
            Some(forwarder) => override_results.boxes(forwarder),
            None => boxes(edge.implementation, edge.implementation_type)?,
        };
        Ok(Self {
            declared: physical(
                edge.declared_type,
                boxes(edge.overridden, edge.declared_type)?,
            ),
            implementation: physical(edge.implementation_type, implementation_boxed),
        })
    }
}

fn property_implementation(
    edge: &crate::ir::IrPropertyOverride,
    accessor: BridgeAccessorRole,
) -> Option<BridgePropertyImplementation> {
    match edge.implementation {
        crate::fir::ResolvedPropertyOverrideTarget::Module(property) => {
            Some(BridgePropertyImplementation { property, accessor })
        }
        crate::fir::ResolvedPropertyOverrideTarget::External(_) => None,
    }
}

/// Bridge-signature erasure: a type parameter becomes its bound's storage type, a nullable keeps its
/// wrapper. This is the shape a descriptor is written from, so it defines when two signatures COLLIDE.
pub(super) fn bridge_erasure(ty: Ty) -> Ty {
    bridge_erasure_visiting(ty, &mut std::collections::HashSet::new())
}

fn bridge_erasure_visiting(ty: Ty, visiting: &mut std::collections::HashSet<&'static str>) -> Ty {
    match ty {
        // `<T : S>` where `S : Entity` erases to `Entity`, whether `S` is declared on the method
        // or on the enclosing classifier. A cycle (`T : S`, `S : T`) has no class bound.
        Ty::TyParam(name, bound) => {
            if !visiting.insert(name) {
                return Ty::obj("kotlin/Any");
            }
            let erased = stored_value_ty(bridge_erasure_visiting(*bound, visiting));
            visiting.remove(name);
            erased
        }
        Ty::Nullable(inner) => Ty::nullable(bridge_erasure_visiting(*inner, visiting)),
        Ty::Obj(internal, _) if internal == crate::types::TypeName::ROOT => Ty::obj("kotlin/Any"),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::bridge_erasure;
    use crate::types::Ty;

    #[test]
    fn dependent_type_parameter_erases_to_its_class_bound_for_bridge_descriptors() {
        let owner = Ty::ty_param("S", Ty::obj("sample/Entity"));
        let method = Ty::ty_param("T", owner);

        assert_eq!(bridge_erasure(method), Ty::obj("sample/Entity"));
        assert_eq!(bridge_erasure(owner), Ty::obj("sample/Entity"));
    }

    #[test]
    fn a_repeated_type_parameter_name_stops_before_the_carried_class_bound() {
        let parameter = Ty::ty_param("T", Ty::obj("sample/Entity"));
        let repeated = Ty::ty_param("T", parameter);

        assert_eq!(bridge_erasure(repeated), Ty::obj("kotlin/Any"));
    }

    #[test]
    fn an_empty_classifier_erases_to_any() {
        assert_eq!(bridge_erasure(Ty::obj("")), Ty::obj("kotlin/Any"));
    }
}

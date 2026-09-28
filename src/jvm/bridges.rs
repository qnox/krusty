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

use crate::ir::{Bridge, BridgeKind, BridgeParameter, IrFile};
use crate::jvm::backend::SkipReason;
use crate::jvm::names::{method_descriptor, type_descriptor};
use crate::names::{property_getter_name, property_setter_name};
use crate::types::{stored_value_ty, Ty};

/// Every bridge family this class needs, appended to `IrClass::bridges`.
pub(super) fn derive_bridges(
    ir: &mut IrFile,
    classpath: &crate::jvm::classpath::Classpath,
    override_results: &crate::jvm::override_results::OverrideResults,
    argument_arrays: &mut crate::jvm::function_argument_arrays::FunctionArgumentArrays,
) -> Result<(), SkipReason> {
    for cid in 0..ir.classes.len() {
        // Source-declared classes and declaration-owned enum-entry subclasses only. Lambdas and
        // callable-reference classes have emitter-chosen supertypes and no Kotlin override edges;
        // an enum-entry body, however, contains source override declarations whose stable edges are
        // attached to its common-IR subclass before this JVM representation pass.
        if !ir.classes[cid].is_source_declared && ir.classes[cid].enum_entry_of.is_none() {
            continue;
        }
        let first = ir.classes[cid].bridges.len();
        let mut order = Vec::new();
        superclass_method_bridges(
            ir,
            cid,
            classpath,
            override_results,
            argument_arrays,
            &mut order,
        )?;
        property_bridges(ir, cid, classpath, &mut order)?;
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
    classpath: &crate::jvm::classpath::Classpath,
    target: crate::fir::ExternalCallableId,
) -> Result<String, SkipReason> {
    let realization = classpath
        .external_callable(target)
        .ok_or(SkipReason::Bridges)?;
    let callable = realization.callable;
    Ok(crate::jvm::names::mapped_builtin_virtual_name(
        callable.owner,
        &callable.name,
        &callable.descriptor,
    )
    .to_owned())
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
        // through that wrapper.
        if let crate::fir::ResolvedFunctionOverrideTarget::Module(callable) = edge.overridden {
            if override_results
                .boxed_callable_result(ir, callable)
                .is_some()
            {
                declared_result = Ty::nullable(declared_result);
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
            crate::fir::ResolvedFunctionOverrideTarget::Module(_) => edge.name.clone(),
            crate::fir::ResolvedFunctionOverrideTarget::External(target) => {
                external_method_name(classpath, target)?
            }
        };
        let target_name = if edge.implementation_function.is_some() {
            edge.name.clone()
        } else {
            match edge.implementation {
                crate::fir::ResolvedFunctionOverrideTarget::Module(_) => edge.name.clone(),
                crate::fir::ResolvedFunctionOverrideTarget::External(target) => {
                    external_method_name(classpath, target)?
                }
            }
        };
        if bridge_name != edge.name && edge.has_kotlin_superclass_override {
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
            if base_params == own_params && !vc_ret {
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
        let target_name = (bridge_name != target_name).then_some(target_name);
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
        let special = bridge_name != edge.name;
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
            target_name,
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
            crate::fir::ResolvedPropertyOverrideTarget::Module(_) => source_getter.clone(),
            crate::fir::ResolvedPropertyOverrideTarget::External(target) => {
                external_method_name(classpath, target)?
            }
        };
        let target_getter = match edge.implementation {
            _ if edge.implementation_getter.is_some() => source_getter.clone(),
            crate::fir::ResolvedPropertyOverrideTarget::Module(_) => source_getter.clone(),
            crate::fir::ResolvedPropertyOverrideTarget::External(target) => {
                external_method_name(classpath, target)?
            }
        };
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
        if type_descriptor(edge.declared_type) == type_descriptor(edge.implementation_type)
            && declared_receiver.map(type_descriptor)
                == implementation_receiver.map(type_descriptor)
            && bridge_getter == target_getter
        {
            continue;
        }
        if bridge_getter != source_getter && edge.has_kotlin_superclass_override {
            continue;
        }
        let bridge_descriptor = method_descriptor(
            &declared_receiver.into_iter().collect::<Vec<_>>(),
            bridge_erasure(edge.declared_type),
        );
        if bridge_getter != target_getter
            && inherits_final_special_bridge(ir, cid, classpath, &bridge_getter, &bridge_descriptor)
        {
            continue;
        }
        let position = implementation_property
            .filter(|_| declared_here)
            .map_or(u32::MAX, |property| property.source_order);
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
        push_property_bridge(ir, cid, &edge, bridge_getter, target_getter);
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
        target_name: None,
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

/// The `get<X>()` bridge (and, for a `var` override, the `set<X>()` one). A bridge already recorded under
/// the accessor's name wins — the first supertype in the walk is the nearest one. Each delegates to
/// the implementation's own accessor function when this class declares it.
fn push_property_bridge(
    ir: &mut IrFile,
    cid: usize,
    edge: &crate::ir::IrPropertyOverride,
    getter_name: String,
    getter_target: String,
) {
    let own = |accessor: Option<u32>| accessor.filter(|f| ir.classes[cid].methods.contains(f));
    let (getter, setter) = (
        own(edge.implementation_getter),
        own(edge.implementation_setter),
    );
    let has_getter = ir.classes[cid]
        .bridges
        .iter()
        .any(|b| b.name == getter_name && b.erased_params.is_empty());
    if !has_getter {
        let target_name = (getter_name != getter_target).then_some(getter_target);
        let special = getter_name != property_getter_name(&edge.name);
        ir.classes[cid].bridges.push(Bridge {
            kind: BridgeKind::PropertyGetter,
            target_function: getter,
            overridden_owner: None,
            collection_barrier: None,
            parameters: Vec::new(),
            name: getter_name,
            erased_params: vec![],
            erased_ret: edge.declared_type,
            concrete_params: vec![],
            concrete_ret: edge.implementation_type,
            target_ret: None,
            barrier_plan: None,
            special,
            target_name,
        });
    }
    if !(edge.overridden_mutable && edge.implementation_mutable) {
        return;
    }
    let sname = property_setter_name(&edge.name);
    let has_setter = ir.classes[cid]
        .bridges
        .iter()
        .any(|b| b.name == sname && b.erased_params.len() == 1);
    if !has_setter {
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
            target_name: None,
        });
    }
}

/// Bridge-signature erasure: a type parameter becomes its bound's storage type, a nullable keeps its
/// wrapper. This is the shape a descriptor is written from, so it defines when two signatures COLLIDE.
fn bridge_erasure(ty: Ty) -> Ty {
    match ty {
        // JVM method descriptors erase a type parameter whose first bound is another type
        // parameter to Object. The generic Signature still records `T : S`; recursively erasing
        // through `S` here would invent `Entity` for `<T : S, S : Entity<...>>`, disagreeing with
        // the descriptor the emitter (and kotlinc) actually publishes.
        Ty::TyParam(_, bound) if matches!(*bound, Ty::TyParam(..)) => Ty::obj("kotlin/Any"),
        Ty::TyParam(_, bound) => stored_value_ty(bridge_erasure(*bound)),
        Ty::Nullable(inner) => Ty::nullable(bridge_erasure(*inner)),
        Ty::Obj(internal, _) if internal == crate::types::TypeName::ROOT => Ty::obj("kotlin/Any"),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::bridge_erasure;
    use crate::types::Ty;

    #[test]
    fn dependent_type_parameter_erases_to_object_for_bridge_descriptors() {
        let owner = Ty::ty_param("S", Ty::obj("sample/Entity"));
        let method = Ty::ty_param("T", owner);

        assert_eq!(bridge_erasure(method), Ty::obj("kotlin/Any"));
        assert_eq!(bridge_erasure(owner), Ty::obj("sample/Entity"));
    }

    #[test]
    fn an_empty_classifier_erases_to_any() {
        assert_eq!(bridge_erasure(Ty::obj("")), Ty::obj("kotlin/Any"));
    }
}

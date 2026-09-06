//! Bridge-method derivation.
//!
//! A bridge is not a Kotlin declaration: it exists because the JVM dispatches on an ERASED descriptor,
//! so an override whose descriptor differs from the supertype's (generic, covariant, mangled, or
//! physically renamed by a mapped interface) is invisible through a supertype reference without a
//! synthetic `ACC_BRIDGE` method carrying the supertype's descriptor. Descriptors, erasure, accessor
//! names and `@JvmName` mangling are all JVM realizations of a declaration, so the derivation belongs
//! here rather than in `ir_lower`, which only records what the source declares.
//!
//! The pass consumes exact override edges selected while Pass 1's declaration providers were live. It
//! runs before the value-class pass so an existing bridge's target is retargeted/renamed with the
//! mangled name once mangling is known.

use crate::ir::{Bridge, BridgeKind, IrFile};
use crate::jvm::backend::SkipReason;
use crate::jvm::names::{method_descriptor, type_descriptor};
use crate::names::{property_getter_name, property_setter_name};
use crate::types::{stored_value_ty, Ty};

/// Every bridge family this class needs, appended to `IrClass::bridges`.
pub fn derive_bridges(
    ir: &mut IrFile,
    classpath: &crate::jvm::classpath::Classpath,
) -> Result<(), SkipReason> {
    for cid in 0..ir.classes.len() {
        // Source-declared classes and declaration-owned enum-entry subclasses only. Lambdas and
        // callable-reference classes have emitter-chosen supertypes and no Kotlin override edges;
        // an enum-entry body, however, contains source override declarations whose stable edges are
        // attached to its common-IR subclass before this JVM representation pass.
        if !ir.classes[cid].is_source_declared && ir.classes[cid].enum_entry_of.is_none() {
            continue;
        }
        superclass_method_bridges(ir, cid, classpath)?;
        property_bridges(ir, cid, classpath);
    }
    Ok(())
}

fn external_method_name(
    classpath: &crate::jvm::classpath::Classpath,
    target: crate::fir::ExternalCallableId,
    fallback: &str,
) -> String {
    classpath.external_callable(target).map_or_else(
        || fallback.to_owned(),
        |realization| {
            let callable = realization.callable;
            crate::jvm::names::mapped_builtin_virtual_name(&callable.owner.render(), &callable.name)
                .to_owned()
        },
    )
}

/// A method overriding a superclass method with a different erased signature (a generic or covariant
/// override) needs an `ACC_BRIDGE` method carrying the SUPERCLASS's descriptor that delegates to the
/// concrete override — without it a call through a base reference resolves to a method that is not there.
fn superclass_method_bridges(
    ir: &mut IrFile,
    cid: usize,
    classpath: &crate::jvm::classpath::Classpath,
) -> Result<(), SkipReason> {
    let internal_name = ir.classes[cid].fq_name;
    let edges = ir
        .function_overrides
        .get(&internal_name)
        .cloned()
        .unwrap_or_default();
    for edge in edges {
        let own_fid = edge
            .implementation_function
            .or_else(|| match edge.implementation {
                crate::fir::ResolvedFunctionOverrideTarget::Module(declaration) => {
                    ir.checked_callable_functions.get(&declaration).copied()
                }
                crate::fir::ResolvedFunctionOverrideTarget::External(_) => None,
            });
        if edge.implementation_owner == internal_name
            && own_fid.is_none_or(|function| !ir.classes[cid].methods.contains(&function))
        {
            continue;
        }
        // Common IR records the selected declaration and its call-site semantic signature. For an
        // external declaration, recover the provider's canonical UNAPPLIED source signature by the
        // opaque identity: the edge may say `Echo<String>.echo: String`, while the declaration still
        // says `Echo<T>.echo: T`. Only then apply JVM bridge erasure. Using the physical descriptor
        // here is too early: it would manufacture a bridge for semantic value-class parameters such
        // as `Continuation.resumeWith(Result<T>)` before the value-class pass realizes their carrier.
        let (base_params, base_ret) = match edge.overridden {
            crate::fir::ResolvedFunctionOverrideTarget::Module(_) => (
                edge.declared_parameters
                    .iter()
                    .copied()
                    .map(bridge_erasure)
                    .collect::<Vec<_>>(),
                bridge_erasure(edge.declared_result),
            ),
            crate::fir::ResolvedFunctionOverrideTarget::External(target) => {
                let realization = classpath
                    .external_callable(target)
                    .ok_or(SkipReason::Bridges)?;
                if realization.kind != crate::jvm::classpath::ExternalCallableKind::Member {
                    return Err(SkipReason::Bridges);
                }
                let callable = realization.callable;
                let declared_parameters = callable
                    .declared_params
                    .or_else(|| {
                        callable
                            .generic_sig
                            .as_ref()
                            .map(|signature| signature.params.clone().into_boxed_slice())
                    })
                    .unwrap_or_else(|| callable.params.into_boxed_slice());
                let declared_result = callable
                    .declared_ret
                    .or_else(|| callable.generic_sig.as_ref().map(|signature| signature.ret))
                    .unwrap_or(callable.ret);
                (
                    declared_parameters
                        .iter()
                        .copied()
                        .map(bridge_erasure)
                        .collect(),
                    bridge_erasure(declared_result),
                )
            }
        };
        let concrete_params = own_fid
            .map(|function| ir.functions[function as usize].params.clone())
            .unwrap_or_else(|| edge.implementation_parameters.clone());
        let concrete_ret = own_fid
            .map(|function| ir.functions[function as usize].ret)
            .unwrap_or(edge.implementation_result);
        let own_params = concrete_params
            .iter()
            .copied()
            .map(bridge_erasure)
            .collect::<Vec<_>>();
        let own_ret = bridge_erasure(concrete_ret);
        let bridge_name = match edge.overridden {
            crate::fir::ResolvedFunctionOverrideTarget::Module(_) => edge.name.clone(),
            crate::fir::ResolvedFunctionOverrideTarget::External(target) => {
                external_method_name(classpath, target, &edge.name)
            }
        };
        let target_name = if edge.implementation_function.is_some() {
            edge.name.clone()
        } else {
            match edge.implementation {
                crate::fir::ResolvedFunctionOverrideTarget::Module(_) => edge.name.clone(),
                crate::fir::ResolvedFunctionOverrideTarget::External(target) => {
                    external_method_name(classpath, target, &edge.name)
                }
            }
        };
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
                    .is_some_and(|name| ir.is_value_class_name(name))
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
        if edge.suspend || own_fid.is_some_and(|function| ir.suspend_funs.contains(&function)) {
            let vc_ret = own_ret
                .non_null()
                .obj_internal()
                .is_some_and(|n| ir.is_value_class_name(n));
            if base_params == own_params && !vc_ret {
                continue;
            }
        }
        if ir.classes[cid].bridges.iter().any(|bridge| {
            bridge.name == bridge_name
                && bridge.erased_params == base_params
                && bridge.erased_ret == base_ret
        }) {
            continue;
        }
        let target_name = (bridge_name != target_name).then_some(target_name);
        ir.classes[cid].bridges.push(Bridge {
            kind: BridgeKind::Function,
            target_function: own_fid,
            name: bridge_name,
            erased_params: base_params,
            erased_ret: base_ret,
            concrete_params,
            concrete_ret,
            target_ret: None,
            type_safe_barrier: false,
            target_name,
            box_ret: None,
            unbox_params: Vec::new(),
        });
    }
    Ok(())
}

/// A property overriding a supertype property with a different erased type (a covariant override
/// `from: Sub` over `from: Super`, or a generic `val x: T` erased to `Object` overridden with a concrete
/// type) needs a synthetic `getX()` returning the supertype's erased type that delegates to the concrete
/// getter — else a call through a supertype reference resolves to a getter that does not exist. A `var`
/// override needs the matching `setX(erased)`, else a write through the supertype silently no-ops.
fn property_bridges(ir: &mut IrFile, cid: usize, classpath: &crate::jvm::classpath::Classpath) {
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
        if edge.implementation_owner == internal_name
            && implementation_property.is_none_or(|property| property.class != Some(cid as u32))
        {
            continue;
        }
        let source_getter = property_getter_name(&edge.name);
        let bridge_getter = match edge.overridden {
            crate::fir::ResolvedPropertyOverrideTarget::Module(_) => source_getter.clone(),
            crate::fir::ResolvedPropertyOverrideTarget::External(target) => {
                external_method_name(classpath, target, &source_getter)
            }
        };
        let target_getter = match edge.implementation {
            crate::fir::ResolvedPropertyOverrideTarget::Module(_) => source_getter.clone(),
            crate::fir::ResolvedPropertyOverrideTarget::External(target) => {
                external_method_name(classpath, target, &source_getter)
            }
        };
        if type_descriptor(edge.declared_type) == type_descriptor(edge.implementation_type)
            && bridge_getter == target_getter
        {
            continue;
        }
        let name = edge.name.clone();
        push_property_bridge(
            ir,
            cid,
            &name,
            edge.declared_type,
            edge.implementation_type,
            (edge.declared_type, edge.implementation_type),
            edge.overridden_mutable && edge.implementation_mutable,
            bridge_getter,
            target_getter,
        );
    }
}

/// The `get<X>()` bridge (and, for a `var` override, the `set<X>()` one). A bridge already recorded under
/// the accessor's name wins — the first supertype in the walk is the nearest one.
fn push_property_bridge(
    ir: &mut IrFile,
    cid: usize,
    pname: &str,
    super_ret: Ty,
    own_ret: Ty,
    (super_ty, own_ty): (Ty, Ty),
    needs_setter: bool,
    getter_name: String,
    getter_target: String,
) {
    let has_getter = ir.classes[cid]
        .bridges
        .iter()
        .any(|b| b.name == getter_name && b.erased_params.is_empty());
    if !has_getter {
        let target_name = (getter_name != getter_target).then_some(getter_target);
        ir.classes[cid].bridges.push(Bridge {
            kind: BridgeKind::PropertyGetter,
            target_function: None,
            name: getter_name,
            erased_params: vec![],
            erased_ret: super_ret,
            concrete_params: vec![],
            concrete_ret: own_ret,
            target_ret: None,
            type_safe_barrier: false,
            target_name,
            box_ret: None,
            unbox_params: Vec::new(),
        });
    }
    if !needs_setter {
        return;
    }
    let sname = property_setter_name(pname);
    let has_setter = ir.classes[cid]
        .bridges
        .iter()
        .any(|b| b.name == sname && b.erased_params.len() == 1);
    if !has_setter {
        ir.classes[cid].bridges.push(Bridge {
            kind: BridgeKind::PropertySetter,
            target_function: None,
            name: sname,
            erased_params: vec![super_ty],
            erased_ret: Ty::Unit,
            concrete_params: vec![own_ty],
            concrete_ret: Ty::Unit,
            target_ret: None,
            type_safe_barrier: false,
            target_name: None,
            box_ret: None,
            unbox_params: Vec::new(),
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
        Ty::Obj(internal, _) if internal.render().is_empty() => Ty::obj("kotlin/Any"),
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
}

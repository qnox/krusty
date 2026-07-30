//! Bridge-method derivation.
//!
//! A bridge is not a Kotlin declaration: it exists because the JVM dispatches on an ERASED descriptor,
//! so an override whose descriptor differs from the supertype's (generic, covariant, mangled, or
//! physically renamed by a mapped interface) is invisible through a supertype reference without a
//! synthetic `ACC_BRIDGE` method carrying the supertype's descriptor. Descriptors, erasure, accessor
//! names and `@JvmName` mangling are all JVM realizations of a declaration, so the derivation belongs
//! here rather than in `ir_lower`, which only records what the source declares.
//!
//! The pass reads the OWN side from the IR (`IrClass::methods`/`properties`) and the SUPERTYPE side from
//! the checker's symbols, and runs before the value-class pass so an existing bridge's target is
//! retargeted/renamed with the mangled name once mangling is known.

use crate::frontend::FrontendSymbols;
use crate::ir::{Bridge, IrFile};
use crate::jvm::backend::SkipReason;
use crate::jvm::names::type_descriptor;
use crate::names::{property_getter_name, property_setter_name};
use crate::types::{stored_value_ty, Ty, TypeName};

/// Every bridge family this class needs, appended to `IrClass::bridges`.
pub fn derive_bridges(ir: &mut IrFile, syms: &FrontendSymbols) -> Result<(), SkipReason> {
    for cid in 0..ir.classes.len() {
        if ir.classes[cid].is_interface {
            continue;
        }
        // Source-declared classes only. A synthetic class (a lambda, a callable-reference subclass) has no
        // source declaration to override anything, and its supertypes are the emitter's own choice.
        if syms.class_by_type_name(ir.classes[cid].fq_name).is_none() {
            continue;
        }
        superclass_method_bridges(ir, cid)?;
        property_bridges(ir, cid, syms);
        mapped_interface_bridges(ir, cid, syms);
    }
    Ok(())
}

/// The nearest same-named method above this class, walking the superclass chain within the file.
fn super_chain_method(ir: &IrFile, cid: usize, name: &str) -> Option<u32> {
    let mut owner = ir.classes[cid].superclass;
    loop {
        let base = ir.classes.iter().find(|c| c.fq_name == owner)?;
        if let Some(fid) = base
            .methods
            .iter()
            .copied()
            .find(|fid| ir.functions[*fid as usize].name == name)
        {
            return Some(fid);
        }
        owner = base.superclass;
    }
}

/// A method overriding a superclass method with a different erased signature (a generic or covariant
/// override) needs an `ACC_BRIDGE` method carrying the SUPERCLASS's descriptor that delegates to the
/// concrete override — without it a call through a base reference resolves to a method that is not there.
fn superclass_method_bridges(ir: &mut IrFile, cid: usize) -> Result<(), SkipReason> {
    let bounds: Vec<String> = ir.classes[cid]
        .type_param_bounds
        .iter()
        .map(|(n, _)| n.clone())
        .collect();
    for own_fid in ir.classes[cid].methods.clone() {
        let name = ir.functions[own_fid as usize].name.clone();
        let Some(base_fid) = super_chain_method(ir, cid, &name) else {
            continue;
        };
        // A param/return typed by a class type-param that carries a *class* upper bound
        // (`class D<T : Foo> : Base<T>() { override fun bar(x: T) }`): kotlinc erases the override to the
        // bound (`bar(Foo)`) and synthesizes a `bar(Object)` bridge that `checkcast`s to `Foo` — that cast
        // is observable (it throws CCE on an out-of-bound arg passed through the erased supertype). krusty
        // erases the type-param to `Object` instead, so it would emit neither the bound descriptor nor the
        // casting bridge — a miscompile. Skip until bound-aware erasure exists.
        let bounded = |t: &Ty| matches!(t, Ty::TyParam(n, _) if bounds.iter().any(|b| b == n));
        let own = &ir.functions[own_fid as usize];
        if own.params.iter().any(bounded) || bounded(&own.ret) {
            return Err(SkipReason::Bridges);
        }
        let (op, or) = (own.params.clone(), own.ret);
        let base = &ir.functions[base_fid as usize];
        let (bp, br) = (base.params.clone(), base.ret);
        // A different ARITY means this is a sibling OVERLOAD of the base method, not an override.
        if bp.len() != op.len() || (bp == op && br == or) {
            continue;
        }
        // A suspend override needing an erasure bridge can't be modeled (the coroutine pass rewrites the
        // concrete method to CPS afterwards but never fixes up the bridge) — skip the file rather than
        // emit a broken bridge.
        if ir.suspend_funs.contains(&own_fid) {
            return Err(SkipReason::Bridges);
        }
        ir.classes[cid].bridges.push(Bridge {
            name,
            erased_params: bp,
            erased_ret: br,
            concrete_params: op,
            concrete_ret: or,
            type_safe_barrier: false,
            target_name: None,
            box_ret: None,
            unbox_params: Vec::new(),
        });
    }
    Ok(())
}

/// The accessor's return type for a property this class (or a same-file supertype) declares: the
/// source-written accessor's own return when there is one, else the declared property type.
fn accessor_ret(ir: &IrFile, owner: TypeName, name: &str, declared: Ty) -> Ty {
    ir.classes
        .iter()
        .find(|c| c.fq_name == owner)
        .and_then(|c| c.properties.iter().find(|p| p.name == name))
        .and_then(|p| p.getter)
        .map(|fid| ir.functions[fid as usize].ret)
        .unwrap_or(declared)
}

/// A property overriding a supertype property with a different erased type (a covariant override
/// `from: Sub` over `from: Super`, or a generic `val x: T` erased to `Object` overridden with a concrete
/// type) needs a synthetic `getX()` returning the supertype's erased type that delegates to the concrete
/// getter — else a call through a supertype reference resolves to a getter that does not exist. A `var`
/// override needs the matching `setX(erased)`, else a write through the supertype silently no-ops.
fn property_bridges(ir: &mut IrFile, cid: usize, syms: &FrontendSymbols) {
    let internal_name = ir.classes[cid].fq_name;
    for sup in syms.supertype_internal_names_from(internal_name) {
        let Some(sc) = syms.class_by_type_name(sup) else {
            continue;
        };
        for (pname, sty, base_is_var) in sc.props.clone() {
            let Some((own_ty, own_is_var)) = syms.prop_of_name(internal_name, &pname) else {
                continue;
            };
            if type_descriptor(sty) == type_descriptor(own_ty) {
                continue;
            }
            push_property_bridge(
                ir,
                cid,
                &pname,
                accessor_ret(ir, sup, &pname, sty),
                accessor_ret(ir, internal_name, &pname, own_ty),
                (sty, own_ty),
                base_is_var && own_is_var,
            );
        }
    }
    // The CLASSPATH-supertype twin of the loop above: a property whose INFERRED type covariantly narrows
    // a classpath supertype property (`override val context = EmptyCoroutineContext` under
    // `Continuation.context: CoroutineContext`) needs the same `get<X>()` bridge. The classpath
    // interface's accessor is a plain METHOD (`getContext()`), so pair this class's own properties
    // against the supertype's member set.
    let own_prop_names: Vec<String> = ir.classes[cid]
        .properties
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let sup_names: Vec<TypeName> = syms
        .class_by_type_name(internal_name)
        .map(|ci| {
            ci.interfaces
                .iter_ids()
                .chain(ci.super_internal)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for sup in sup_names {
        let recv = Ty::Obj(sup, &[]);
        for pname in &own_prop_names {
            let Some((own_ty, _)) = syms.prop_of_name(internal_name, pname) else {
                continue;
            };
            let ps = syms.libraries.property_members(recv, pname);
            let Some(pi) = ps
                .overloads
                .iter()
                .find(|p| matches!(p.kind, crate::libraries::PropKind::Member))
            else {
                continue;
            };
            if type_descriptor(pi.ty) == type_descriptor(own_ty) {
                continue;
            }
            push_property_bridge(ir, cid, pname, pi.ty, own_ty, (pi.ty, own_ty), false);
        }
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
) {
    let gname = property_getter_name(pname);
    let has_getter = ir.classes[cid]
        .bridges
        .iter()
        .any(|b| b.name == gname && b.erased_params.is_empty());
    if !has_getter {
        ir.classes[cid].bridges.push(Bridge {
            name: gname,
            erased_params: vec![],
            erased_ret: super_ret,
            concrete_params: vec![],
            concrete_ret: own_ret,
            type_safe_barrier: false,
            target_name: None,
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
            name: sname,
            erased_params: vec![super_ty],
            erased_ret: Ty::Unit,
            concrete_params: vec![own_ty],
            concrete_ret: Ty::Unit,
            type_safe_barrier: false,
            target_name: None,
            box_ret: None,
            unbox_params: Vec::new(),
        });
    }
}

/// A mapped Kotlin interface may expose a different PHYSICAL JVM member name than the source name the
/// override is written under (the Kotlin name is the platform's, the class file's is Java's). Forward
/// the physical name to the source override.
fn mapped_interface_bridges(ir: &mut IrFile, cid: usize, syms: &FrontendSymbols) {
    let internal_name = ir.classes[cid].fq_name;
    let mapped_members = syms
        .applied_hierarchy(Ty::obj_name(internal_name))
        .into_iter()
        .skip(1)
        .flat_map(|(_, supertype, _)| syms.libraries.mapped_interface_members(supertype))
        .collect::<Vec<_>>();
    for mapped in mapped_members {
        if mapped.is_property {
            let Some((concrete_ret, _)) = syms.prop_of_name(internal_name, &mapped.source_name)
            else {
                continue;
            };
            let already = ir.classes[cid].bridges.iter().any(|bridge| {
                bridge.name == mapped.physical_name && bridge.erased_params.is_empty()
            });
            if already {
                continue;
            }
            ir.classes[cid].bridges.push(Bridge {
                name: mapped.physical_name,
                erased_params: vec![],
                erased_ret: mapped.ret,
                concrete_params: vec![],
                concrete_ret,
                type_safe_barrier: false,
                target_name: Some(property_getter_name(&mapped.source_name)),
                box_ret: None,
                unbox_params: Vec::new(),
            });
        } else {
            let Some((_, implementation)) = syms.method_matching_with_owner_name(
                internal_name,
                &mapped.source_name,
                &mapped.params,
            ) else {
                continue;
            };
            let erased_params: Vec<Ty> =
                mapped.params.iter().copied().map(stored_value_ty).collect();
            let already = ir.classes[cid].bridges.iter().any(|bridge| {
                bridge.name == mapped.physical_name && bridge.erased_params == erased_params
            });
            if already {
                continue;
            }
            ir.classes[cid].bridges.push(Bridge {
                name: mapped.physical_name,
                erased_params,
                erased_ret: mapped.ret,
                concrete_params: implementation
                    .params
                    .iter()
                    .copied()
                    .map(stored_value_ty)
                    .collect(),
                concrete_ret: implementation.ret,
                type_safe_barrier: false,
                target_name: Some(mapped.source_name),
                box_ret: None,
                unbox_params: Vec::new(),
            });
        }
    }
}

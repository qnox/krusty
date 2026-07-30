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
use crate::names::{accessor_property_name, property_getter_name, property_setter_name};
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
        interface_bridges(ir, cid, syms)?;
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

/// Bridge-signature erasure: a type parameter becomes its bound's storage type, a nullable keeps its
/// wrapper. This is the shape a descriptor is written from, so it defines when two signatures COLLIDE.
fn bridge_erasure(ty: Ty) -> Ty {
    match ty {
        Ty::TyParam(_, bound) => stored_value_ty(bridge_erasure(*bound)),
        Ty::Nullable(inner) => Ty::nullable(bridge_erasure(*inner)),
        Ty::Obj(internal, _) if internal.render().is_empty() => Ty::obj("kotlin/Any"),
        other => other,
    }
}

fn unique_match<T>(items: &[T], mut predicate: impl FnMut(&T) -> bool) -> Option<&T> {
    let mut matches = items.iter().filter(|item| predicate(item));
    match (matches.next(), matches.next()) {
        (Some(item), None) => Some(item),
        _ => None,
    }
}

fn class_of(ir: &IrFile, name: TypeName) -> Option<&crate::ir::IrClass> {
    ir.classes.iter().find(|c| c.fq_name == name)
}

/// Methods of a class in this file, by name.
fn methods_named<'a>(ir: &'a IrFile, owner: TypeName, name: &'a str) -> Vec<u32> {
    class_of(ir, owner)
        .map(|c| {
            c.methods
                .iter()
                .copied()
                .filter(|fid| ir.functions[*fid as usize].name == name)
                .collect()
        })
        .unwrap_or_default()
}

/// The nearest declaration of a property at or above `internal`, within this file.
fn declared_property(ir: &IrFile, internal: TypeName, name: &str) -> Option<(TypeName, Ty)> {
    let mut cur = internal;
    loop {
        let c = class_of(ir, cur)?;
        if let Some(p) = c.properties.iter().find(|p| p.name == name) {
            return Some((cur, p.ty));
        }
        cur = c.superclass;
    }
}

/// Every method of an interface in this file, including its super-interfaces'.
fn iface_methods(ir: &IrFile, itf: TypeName) -> Vec<(TypeName, String, u32)> {
    let mut out = Vec::new();
    let mut stack = vec![itf];
    let mut seen = std::collections::HashSet::new();
    while let Some(i) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        if let Some(c) = class_of(ir, i) {
            for fid in c.methods.iter().copied() {
                out.push((i, ir.functions[fid as usize].name.clone(), fid));
            }
            stack.extend(c.interfaces.iter_ids());
        }
    }
    out
}

/// Two erased types that write the SAME descriptor (a platform type and its Kotlin twin do).
fn types_match(left: Ty, right: Ty) -> bool {
    if left == right {
        return true;
    }
    if left.is_reference() != right.is_reference() {
        return false;
    }
    let (Some(left_name), Some(right_name)) = (
        left.non_null().kotlin_class_internal(),
        right.non_null().kotlin_class_internal(),
    ) else {
        return false;
    };
    crate::symbol_resolver::platform_type_names_match(left_name, right_name)
}

fn param_narrows(syms: &FrontendSymbols, erased: Ty, concrete: Ty) -> bool {
    types_match(erased, concrete)
        || erased.is_erased_top()
        || syms.is_source_subtype(concrete, erased)
}

fn return_admits(syms: &FrontendSymbols, erased: Ty, concrete: Ty, allow_fake: bool) -> bool {
    param_narrows(syms, erased, concrete)
        || (allow_fake
            && erased.is_reference()
            && concrete.is_reference()
            && concrete.is_erased_top())
}

/// A value class's parameter erases to its underlying type — the shape the concrete method really takes
/// once the value-class pass has run.
fn post_value_erasure(syms: &FrontendSymbols, ty: Ty) -> Ty {
    if ty.is_nullable() {
        return bridge_erasure(ty);
    }
    syms.libraries
        .value_underlying(ty)
        .map(bridge_erasure)
        .unwrap_or_else(|| bridge_erasure(ty))
}

/// The method a supertype's erased descriptor must land on: searched up the superclass chain first, then
/// across default methods of the interfaces. `None` when nothing implements the obligation here.
fn resolve_bridge_target(
    ir: &IrFile,
    syms: &FrontendSymbols,
    internal: TypeName,
    name: &str,
    erased_params: &[Ty],
    erased_ret: Ty,
    allow_fake_override: bool,
) -> Option<u32> {
    let mut current = Some(internal);
    let mut seen = std::collections::HashSet::new();
    while let Some(owner) = current {
        if !seen.insert(owner) {
            return None;
        }
        let Some(class) = class_of(ir, owner) else {
            break;
        };
        let overloads = methods_named(ir, owner, name);
        if overloads.is_empty() {
            current = Some(class.superclass);
            continue;
        }
        let mut candidates = overloads
            .into_iter()
            .filter_map(|fid| {
                let function = &ir.functions[fid as usize];
                (function.params.len() == erased_params.len()).then(|| {
                    let params = function
                        .params
                        .iter()
                        .copied()
                        .map(bridge_erasure)
                        .collect::<Vec<_>>();
                    (fid, params, bridge_erasure(function.ret))
                })
            })
            .collect::<Vec<_>>();
        if let Some((fid, _, _)) = unique_match(&candidates, |(_, params, ret)| {
            params == erased_params && *ret == erased_ret
        }) {
            return Some(*fid);
        }
        candidates.retain(|(fid, params, ret)| {
            let generic_override = params
                .iter()
                .zip(erased_params)
                .all(|(&concrete, &erased)| param_narrows(syms, erased, concrete));
            let fake_override = allow_fake_override
                && params
                    .iter()
                    .zip(erased_params)
                    .all(|(&concrete, &erased)| {
                        types_match(concrete, erased) || concrete.is_erased_top()
                    });
            let changes_signature = params != erased_params || *ret != erased_ret;
            let return_admitted = return_admits(syms, erased_ret, *ret, allow_fake_override);
            (generic_override || fake_override)
                && return_admitted
                && changes_signature
                // A FRESH declaration in the class itself is not an override, so a supertype's descriptor
                // must not be pointed at it.
                && (owner != internal || !ir.fresh_method_decls.contains(fid))
        });
        if let Some((fid, _, _)) =
            unique_match(&candidates, |(_, params, _)| params == erased_params)
        {
            return Some(*fid);
        }
        if candidates.len() == 1 {
            return Some(candidates[0].0);
        }
        if !candidates.is_empty() {
            return None;
        }
        current = Some(class.superclass);
    }
    resolve_default_bridge_target(ir, syms, internal, name, erased_params, erased_ret)
}

/// The obligation may be satisfied by an inherited interface DEFAULT method rather than by anything the
/// class hierarchy declares.
fn resolve_default_bridge_target(
    ir: &IrFile,
    syms: &FrontendSymbols,
    internal: TypeName,
    name: &str,
    erased_params: &[Ty],
    erased_ret: Ty,
) -> Option<u32> {
    let mut queue = std::collections::VecDeque::new();
    let mut current = Some(internal);
    while let Some(owner) = current {
        let Some(class) = class_of(ir, owner) else {
            break;
        };
        queue.extend(class.interfaces.iter_ids());
        current = Some(class.superclass);
    }
    let mut seen = std::collections::HashSet::new();
    while let Some(owner) = queue.pop_front() {
        if !seen.insert(owner) {
            continue;
        }
        let Some(class) = class_of(ir, owner) else {
            continue;
        };
        let overloads = methods_named(ir, owner, name);
        if !overloads.is_empty() {
            let candidates = overloads
                .into_iter()
                .filter_map(|fid| {
                    let function = &ir.functions[fid as usize];
                    // A DEFAULT method (one with a body) can be the target; an abstract one cannot.
                    if function.body.is_none() || function.params.len() != erased_params.len() {
                        return None;
                    }
                    let params = function
                        .params
                        .iter()
                        .copied()
                        .map(bridge_erasure)
                        .collect::<Vec<_>>();
                    let ret = bridge_erasure(function.ret);
                    let compatible = params
                        .iter()
                        .zip(erased_params)
                        .all(|(&concrete, &erased)| param_narrows(syms, erased, concrete))
                        && return_admits(syms, erased_ret, ret, false);
                    compatible.then_some((fid, params, ret))
                })
                .collect::<Vec<_>>();
            if let Some((fid, _, _)) = unique_match(&candidates, |(_, params, ret)| {
                params == erased_params && *ret == erased_ret
            }) {
                return Some(*fid);
            }
            if candidates.len() == 1 {
                return Some(candidates[0].0);
            }
            if !candidates.is_empty() {
                return None;
            }
        }
        queue.extend(class.interfaces.iter_ids());
    }
    None
}

/// For each method an implemented interface obliges the class to provide, when the class's actual
/// implementation (declared, inherited, or a property accessor the backend synthesizes) has a different
/// erased signature than the interface's, add a bridge carrying the interface's descriptor.
fn interface_bridges(
    ir: &mut IrFile,
    cid: usize,
    syms: &FrontendSymbols,
) -> Result<(), SkipReason> {
    let internal_name = ir.classes[cid].fq_name;
    let mut ifaces = ir.classes[cid].interfaces.clone();
    for sup in syms.supertype_internal_names_from(internal_name) {
        let is_iface = syms
            .class_by_type_name(sup)
            .is_some_and(|c| c.is_interface())
            || syms
                .libraries
                .resolve_type_name(sup)
                .is_some_and(|t| t.is_interface());
        if is_iface && !ifaces.contains_name(sup) {
            ifaces.push_name(sup);
        }
    }
    let mut seen: std::collections::HashSet<(String, Vec<Ty>, Ty)> = ir.classes[cid]
        .bridges
        .iter()
        .map(|bridge| {
            (
                bridge.name.clone(),
                bridge
                    .erased_params
                    .iter()
                    .copied()
                    .map(bridge_erasure)
                    .collect(),
                bridge_erasure(bridge.erased_ret),
            )
        })
        .collect();
    for itf in ifaces.iter_ids() {
        let classpath_interface = syms.libraries.resolve_type_name(itf).is_some();
        let applied_interface_args = syms
            .applied_hierarchy(Ty::obj_name(internal_name))
            .into_iter()
            .find_map(|(owner, applied, _)| (owner == itf).then(|| applied.type_args().to_vec()))
            .unwrap_or_default();
        // `default`: whether the interface method has a body, known only for an interface declared in this
        // file. `logical_params`: the generic signature's declared parameters, for a classpath interface.
        type Obligation = (String, Vec<Ty>, Ty, Option<bool>, Option<Vec<Ty>>);
        let obligations: Vec<Obligation> = if class_of(ir, itf).is_some() {
            iface_methods(ir, itf)
                .into_iter()
                .map(|(_, name, fid)| {
                    let function = &ir.functions[fid as usize];
                    (
                        name,
                        function
                            .params
                            .iter()
                            .copied()
                            .map(bridge_erasure)
                            .collect(),
                        bridge_erasure(function.ret),
                        Some(function.body.is_some()),
                        None,
                    )
                })
                .collect()
        } else if let Some(interface) = syms.libraries.resolve_type_name(itf) {
            interface
                .members
                .iter()
                .map(|member| {
                    (
                        member.name.clone(),
                        member.params.iter().copied().map(bridge_erasure).collect(),
                        bridge_erasure(member.ret),
                        None,
                        member
                            .generic_sig
                            .as_ref()
                            .map(|signature| signature.params.clone()),
                    )
                })
                .collect()
        } else if let Some(interface) = syms.class_by_type_name(itf) {
            interface
                .methods
                .iter()
                .flat_map(|(name, signatures)| {
                    signatures.iter().map(move |signature| {
                        (
                            name.clone(),
                            signature
                                .params
                                .iter()
                                .copied()
                                .map(bridge_erasure)
                                .collect(),
                            bridge_erasure(signature.ret),
                            None,
                            None,
                        )
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        for (name, erased_params, erased_ret, default, logical_params) in obligations {
            let impl_fid = resolve_bridge_target(
                ir,
                syms,
                internal_name,
                &name,
                &erased_params,
                erased_ret,
                default == Some(false),
            );
            // A property accessor is not an IR method — this backend synthesizes it — so the DECLARATION
            // is the implementation the obligation is satisfied by.
            let impl_sig = match impl_fid {
                Some(fid) => {
                    let f = &ir.functions[fid as usize];
                    Some((f.params.clone(), f.ret, f.dispatch_receiver))
                }
                None => accessor_property_name(&name).and_then(|prop| {
                    let (declaring, ty) = declared_property(ir, internal_name, &prop)?;
                    Some(if name.starts_with("set") {
                        (vec![ty], Ty::Unit, Some(declaring))
                    } else {
                        (Vec::new(), ty, Some(declaring))
                    })
                }),
            };
            let Some((concrete_params, concrete_ret, impl_owner)) = impl_sig else {
                continue;
            };
            let concrete_erased_params = concrete_params
                .iter()
                .copied()
                .map(bridge_erasure)
                .collect::<Vec<_>>();
            if erased_params == concrete_erased_params && erased_ret == bridge_erasure(concrete_ret)
            {
                continue;
            }
            // An INHERITED implementation whose signature already matches needs no bridge here: the
            // supertype that declares it carries its own.
            let inherited_signature_matches = impl_owner != Some(internal_name)
                && erased_params.len() == concrete_params.len()
                && erased_params
                    .iter()
                    .zip(&concrete_params)
                    .all(|(&erased, &concrete)| types_match(erased, bridge_erasure(concrete)))
                && types_match(erased_ret, bridge_erasure(concrete_ret));
            if inherited_signature_matches {
                continue;
            }
            let post_value_params = concrete_params
                .iter()
                .copied()
                .map(|ty| post_value_erasure(syms, ty))
                .collect::<Vec<_>>();
            let logical_match = logical_params.as_ref().is_some_and(|logical| {
                logical.len() == concrete_params.len()
                    && logical
                        .iter()
                        .zip(&concrete_params)
                        .all(|(&declared, &concrete)| types_match(declared, concrete))
            });
            let specializes_value_parameter = concrete_params.iter().copied().any(|concrete| {
                syms.libraries.value_underlying(concrete).is_some()
                    && applied_interface_args
                        .iter()
                        .copied()
                        .any(|argument| types_match(concrete, argument))
            });
            if (logical_match || (classpath_interface && !specializes_value_parameter))
                && erased_params.len() == post_value_params.len()
                && erased_params
                    .iter()
                    .zip(&post_value_params)
                    .all(|(&erased, &concrete)| types_match(erased, concrete))
                && erased_ret == bridge_erasure(concrete_ret)
            {
                continue;
            }
            if impl_fid.is_some_and(|fid| ir.suspend_funs.contains(&fid)) {
                return Err(SkipReason::Bridges);
            }
            if seen.insert((name.clone(), erased_params.clone(), erased_ret)) {
                ir.classes[cid].bridges.push(Bridge {
                    name,
                    erased_params,
                    erased_ret,
                    concrete_params,
                    concrete_ret,
                    type_safe_barrier: false,
                    target_name: None,
                    box_ret: None,
                    unbox_params: Vec::new(),
                });
            }
        }
    }
    Ok(())
}

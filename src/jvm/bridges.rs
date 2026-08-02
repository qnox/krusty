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
        superclass_method_bridges(ir, cid, syms)?;
        property_bridges(ir, cid, syms);
        mapped_interface_bridges(ir, cid, syms);
        interface_bridges(ir, cid, syms)?;
    }
    Ok(())
}

#[derive(Clone)]
struct MethodShape {
    params: Vec<Ty>,
    ret: Ty,
}

/// Methods DECLARED on one semantic owner, normalized to the same erased shape regardless of where the
/// owner came from. IR declarations are authoritative when present because later lowering may have
/// refined their physical shape; module symbols cover a sibling source owner without IR in this file;
/// library symbols cover the first external boundary. Keeping that provider distinction inside this
/// adapter prevents bridge policy from growing separate same-file/module/classpath branches.
fn declared_method_shapes(
    ir: &IrFile,
    syms: &FrontendSymbols,
    owner: TypeName,
    name: &str,
) -> Vec<MethodShape> {
    if let Some(class) = class_of(ir, owner) {
        return class
            .methods
            .iter()
            .copied()
            .filter_map(|fid| {
                let function = &ir.functions[fid as usize];
                (function.name == name).then(|| MethodShape {
                    params: function
                        .params
                        .iter()
                        .copied()
                        .map(bridge_erasure)
                        .collect(),
                    ret: bridge_erasure(function.ret),
                })
            })
            .collect();
    }
    if let Some(class) = syms.class_by_type_name(owner) {
        return class
            .methods
            .get(name)
            .into_iter()
            .flatten()
            .map(|signature| MethodShape {
                params: signature
                    .params
                    .iter()
                    .copied()
                    .map(bridge_erasure)
                    .collect(),
                ret: bridge_erasure(signature.ret),
            })
            .collect();
    }
    syms.libraries
        .resolve_type_name(owner)
        .into_iter()
        .flat_map(|class| {
            class
                .members
                .iter()
                .filter(move |member| member.name == name)
                .map(|member| MethodShape {
                    params: member.params.iter().copied().map(bridge_erasure).collect(),
                    ret: bridge_erasure(member.ret),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The direct semantic superclass. Source and library symbol providers expose different storage
/// records, but the bridge walk only needs the language fact "the one parent that is not an interface".
/// Centralizing that normalization also avoids inferring ownership from rendered JVM class names.
fn direct_superclass(syms: &FrontendSymbols, owner: TypeName) -> Option<TypeName> {
    if let Some(class) = syms.class_by_type_name(owner) {
        return class.super_internal;
    }
    syms.libraries.resolve_type_name(owner).and_then(|class| {
        class.supertypes.iter_ids().find(|&candidate| {
            !syms
                .libraries
                .resolve_type_name(candidate)
                .is_some_and(|ty| ty.is_interface())
                && !syms
                    .class_by_type_name(candidate)
                    .is_some_and(|ty| ty.is_interface())
        })
    })
}

/// A method overriding a superclass method with a different erased signature (a generic or covariant
/// override) needs an `ACC_BRIDGE` method carrying the SUPERCLASS's descriptor that delegates to the
/// concrete override — without it a call through a base reference resolves to a method that is not there.
fn superclass_method_bridges(
    ir: &mut IrFile,
    cid: usize,
    syms: &FrontendSymbols,
) -> Result<(), SkipReason> {
    for own_fid in ir.classes[cid].methods.clone() {
        // The explicit source modifier is the semantic discriminator. A same-named fresh declaration is
        // an overload, not evidence that a superclass descriptor should delegate to it; backend/plugin
        // synthesized methods are deliberately absent and may satisfy inherited obligations.
        if ir.fresh_method_decls.contains(&own_fid) {
            continue;
        }
        let name = ir.functions[own_fid as usize].name.clone();
        let own = &ir.functions[own_fid as usize];
        let own_shape = MethodShape {
            params: own.params.iter().copied().map(bridge_erasure).collect(),
            ret: bridge_erasure(own.ret),
        };
        let mut owner = Some(ir.classes[cid].superclass);
        let mut seen = std::collections::HashSet::new();
        let base_shape = loop {
            let Some(base_owner) = owner.filter(|owner| seen.insert(*owner)) else {
                break None;
            };
            let mut compatible = declared_method_shapes(ir, syms, base_owner, &name)
                .into_iter()
                .filter(|base| {
                    base.params.len() == own_shape.params.len()
                        && base
                            .params
                            .iter()
                            .zip(&own_shape.params)
                            .all(|(&erased, &concrete)| param_narrows(syms, erased, concrete))
                        && return_admits(syms, base.ret, own_shape.ret, false)
                })
                .collect::<Vec<_>>();
            // An exact parameter descriptor identifies the overridden overload before a merely
            // compatible generic one. If neither choice is unique, declining the file is safer than
            // emitting a bridge to an arbitrary overload.
            let exact = compatible
                .iter()
                .filter(|shape| shape.params == own_shape.params)
                .count();
            if exact == 1 {
                break compatible
                    .into_iter()
                    .find(|shape| shape.params == own_shape.params);
            }
            if exact > 1 || compatible.len() > 1 {
                return Err(SkipReason::Bridges);
            }
            if let Some(shape) = compatible.pop() {
                break Some(shape);
            }
            // A nearer class may declare only sibling overloads. Keep walking: the method explicitly
            // marked `override` can still implement a declaration farther up the superclass chain.
            owner = direct_superclass(syms, base_owner);
        };
        let Some(base) = base_shape else {
            continue;
        };
        if base.params == own_shape.params && base.ret == own_shape.ret {
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
            erased_params: base.params,
            erased_ret: base.ret,
            concrete_params: own.params.clone(),
            concrete_ret: own.ret,
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

#[derive(Clone, Copy)]
struct PropertyShape {
    ty: Ty,
    accessor_ret: Ty,
    is_var: bool,
}

/// One property DECLARED by a semantic owner, normalized across IR/module/library providers. The
/// provider lookup is intentionally confined here; callers reason only about property type, accessor
/// realization, and mutability, so source and classpath properties cannot drift into twin algorithms.
fn declared_property_shape(
    ir: &IrFile,
    syms: &FrontendSymbols,
    owner: TypeName,
    name: &str,
) -> Option<PropertyShape> {
    if let Some(class) = syms.class_by_type_name(owner) {
        let (ty, is_var) = class.prop(name)?;
        return Some(PropertyShape {
            ty,
            accessor_ret: accessor_ret(ir, owner, name, ty),
            is_var,
        });
    }
    syms.libraries
        .property_members(Ty::obj_name(owner), name)
        .overloads
        .into_iter()
        .find(|property| {
            matches!(property.kind, crate::libraries::PropKind::Member) && property.owner == owner
        })
        .map(|property| PropertyShape {
            ty: property.ty,
            accessor_ret: property.ty,
            is_var: property.setter.is_some(),
        })
}

/// A property overriding a supertype property with a different erased type (a covariant override
/// `from: Sub` over `from: Super`, or a generic `val x: T` erased to `Object` overridden with a concrete
/// type) needs a synthetic `getX()` returning the supertype's erased type that delegates to the concrete
/// getter — else a call through a supertype reference resolves to a getter that does not exist. A `var`
/// override needs the matching `setX(erased)`, else a write through the supertype silently no-ops.
fn property_bridges(ir: &mut IrFile, cid: usize, syms: &FrontendSymbols) {
    let internal_name = ir.classes[cid].fq_name;
    let own_properties: Vec<(String, PropertyShape)> = ir.classes[cid]
        .properties
        .iter()
        .filter_map(|property| {
            let (ty, is_var) = syms.prop_of_name(internal_name, &property.name)?;
            Some((
                property.name.clone(),
                PropertyShape {
                    ty,
                    accessor_ret: accessor_ret(ir, internal_name, &property.name, ty),
                    is_var,
                },
            ))
        })
        .collect();
    let mut supertypes = syms
        .applied_hierarchy(Ty::obj_name(internal_name))
        .into_iter()
        .filter(|(owner, _, _)| *owner != internal_name)
        .collect::<Vec<_>>();
    // `applied_hierarchy` is a graph walk whose sibling order is an implementation detail. Bridge
    // deduplication intentionally lets the nearest declaration win, so make that semantic ordering
    // explicit before examining superclass and interface providers.
    supertypes.sort_by_key(|(_, _, depth)| *depth);
    for (super_owner, _, _) in supertypes {
        for (name, own) in &own_properties {
            let Some(base) = declared_property_shape(ir, syms, super_owner, name) else {
                continue;
            };
            if type_descriptor(base.ty) == type_descriptor(own.ty) {
                continue;
            }
            push_property_bridge(
                ir,
                cid,
                name,
                base.accessor_ret,
                own.accessor_ret,
                (base.ty, own.ty),
                base.is_var && own.is_var,
            );
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

/// The nearest property declaration whose GENERATED accessor has `accessor`, together with whether the
/// match is its setter. Matching the forward naming functions is deliberate: reversing `getURL` or
/// `setOpen` loses information (`URL` vs `uRL`, `isOpen` vs `open`) and can point an interface obligation
/// at the wrong property. This walk consumes declarations, never rendered owner/class names.
fn declared_property_accessor(
    ir: &IrFile,
    internal: TypeName,
    accessor: &str,
) -> Option<(TypeName, Ty, bool)> {
    let mut cur = internal;
    loop {
        let c = class_of(ir, cur)?;
        if let Some(property) = c.properties.iter().find(|property| {
            property_getter_name(&property.name) == accessor
                || (property.is_var && property_setter_name(&property.name) == accessor)
        }) {
            let setter = property.is_var && property_setter_name(&property.name) == accessor;
            return Some((cur, property.ty, setter));
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
                None => declared_property_accessor(ir, internal_name, &name).map(
                    |(declaring, ty, setter)| {
                        if setter {
                            (vec![ty], Ty::Unit, Some(declaring))
                        } else {
                            (Vec::new(), ty, Some(declaring))
                        }
                    },
                ),
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

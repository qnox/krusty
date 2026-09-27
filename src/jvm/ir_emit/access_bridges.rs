//! The synthetic `access$…` methods a private declaration needs when something outside it calls it.
//!
//! Each one forwards to an already-selected declaration; nothing here looks a target up, and the
//! bridge's own shape follows that declaration's rather than the call site's.

use super::*;

pub(super) fn emit_function_reference_access_bridge(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    cw: &mut ClassWriter,
    owner_is_interface: bool,
    line: u32,
) {
    let function = &ir.functions[fid as usize];
    let parameters = jvm_function_params(ir, fid);
    let result = jvm_declared_ty(&function.ret);
    // A STATIC target — a value class's member accessor, realized over the carrier — is already
    // callable without an instance, so the bridge takes exactly its parameters and `invokestatic`s
    // it. An instance target keeps the owner ahead of them and a non-virtual call to the private
    // declaration.
    let mut bridge_parameters = Vec::with_capacity(parameters.len() + 1);
    if !function.is_static {
        bridge_parameters.push(Ty::obj(owner));
    }
    bridge_parameters.extend(parameters.iter().copied());
    let mut code = CodeBuilder::new(bridge_parameters.iter().map(|ty| slot_words(*ty)).sum());
    let mut slot = 0u16;
    if !function.is_static {
        code.aload(0);
        slot = 1;
    }
    for &parameter in &parameters {
        load(parameter, slot, &mut code);
        slot += slot_words(parameter);
    }
    let descriptor = method_descriptor(&parameters, result);
    let method = if owner_is_interface {
        cw.interface_methodref(owner, &function.name, &descriptor)
    } else {
        cw.methodref(owner, &function.name, &descriptor)
    };
    let argument_words = parameters.iter().map(|ty| slot_words(*ty) as i32).sum();
    if line != 0 {
        code.mark_line(line);
    }
    if function.is_static {
        code.invokestatic(method, argument_words, slot_words(result) as i32);
    } else {
        code.invokespecial(method, argument_words, slot_words(result) as i32);
    }
    emit_return(result, &mut code);
    code.ensure_locals(slot.max(1));
    code.link();
    let name = format!("access${}", function.name);
    let descriptor = method_descriptor(&bridge_parameters, result);
    cw.add_method(
        0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
        &name,
        &descriptor,
        &code,
    );
    set_bridge_locals(ir, fid, owner, &parameters, &name, &descriptor, cw);
}

/// kotlinc's whole-method locals of an `access$…` bridge: `$this` for an instance target, then the
/// target's own parameter names over the slots they arrive in.
fn set_bridge_locals(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    parameters: &[Ty],
    name: &str,
    descriptor: &str,
    cw: &mut ClassWriter,
) {
    let names = crate::jvm::parameter_names::function_locals(ir, fid, parameters)
        .expect("an access bridge target carries exact parameter identities");
    let mut locals = Vec::with_capacity(parameters.len() + 1);
    let mut slot = 0u16;
    if !ir.functions[fid as usize].is_static {
        locals.push(("$this".to_string(), format!("L{owner};"), 0));
        slot = 1;
    }
    for (name, &parameter) in names.into_iter().zip(parameters) {
        if let Some(name) = name {
            locals.push((name, local_variable_desc(parameter), slot));
        }
        slot += slot_words(parameter);
    }
    cw.set_method_debug(name, descriptor, None, &locals);
}

/// Find private instance calls whose caller and declaration are different JVM classes.
///
/// FIR/common IR retain Kotlin ownership and the selected member identity only. The Java-8 access
/// bridge is a physical realization, so this whole-file reachability walk belongs at the backend
/// boundary and runs once per emission pass, never once per method candidate.
pub(super) fn cross_owner_private_member_calls(
    ir: &IrFile,
    facade: &str,
    class_member_fids: &std::collections::HashSet<u32>,
    private_interface_bodies_are_members: bool,
) -> std::collections::HashSet<u32> {
    let mut result = std::collections::HashSet::new();
    let mut scan = |owner: &str, roots: Vec<crate::ir::ExprId>| {
        let mut seen = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(expression) = stack.pop() {
            if !seen.insert(expression) {
                continue;
            }
            // A value-class `-impl` call and a private getter read carry their exact target.
            let target = match ir.expr(expression) {
                IrExpr::MethodCall { class, index, .. } => {
                    Some((*class, ir.classes[*class as usize].methods[*index as usize]))
                }
                IrExpr::Call {
                    callee: Callee::Static { owner, .. },
                    ..
                }
                | IrExpr::PropertyRead { owner, .. } => {
                    ir.jvm_member_targets.get(&expression).map(|&function| {
                        let class = ir.class_id_by_name(*owner);
                        (
                            class.expect("a realized member's owner is in this file"),
                            function,
                        )
                    })
                }
                _ => None,
            };
            if let Some((class, target)) = target {
                let target_class = &ir.classes[class as usize];
                if target_class.fq_name() != owner
                    && (private_interface_bodies_are_members || !target_class.is_interface)
                    && ir.private_methods.contains(&target)
                {
                    result.insert(target);
                }
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| stack.push(child));
        }
    };

    let facade_roots = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(fid, function)| {
            !class_member_fids.contains(&(*fid as u32)) && function.dispatch_receiver.is_none()
        })
        .filter_map(|(_, function)| function.body)
        .chain(
            ir.statics
                .iter()
                .filter(|property| property.owner.is_none())
                .map(|property| property.init),
        )
        .collect();
    scan(facade, facade_roots);

    for class in &ir.classes {
        let owner = class.fq_name();
        let mut roots = class
            .methods
            .iter()
            .filter_map(|fid| {
                ir.functions
                    .get(*fid as usize)
                    .and_then(|function| function.body)
            })
            .collect::<Vec<_>>();
        for fid in &class.methods {
            if let Some(defaults) = ir
                .fn_params
                .get(fid)
                .and_then(|parameters| parameters.defaults.as_ref())
            {
                roots.extend(defaults.iter().flatten().copied());
            }
        }
        roots.extend(class.init_body);
        roots.extend(class.super_arg_prelude.iter().copied());
        roots.extend(class.super_args.iter().copied());
        roots.extend(
            class
                .properties
                .iter()
                .filter_map(|property| property.initializer),
        );
        for constructor in &class.secondary_ctors {
            roots.extend(constructor.body);
            roots.extend(constructor.defaults.iter().flatten().copied());
            roots.extend(constructor.delegate_prelude.iter().copied());
            roots.extend(constructor.delegate_args.iter().copied());
        }
        for entry in &class.enum_entries {
            roots.extend(entry.args.iter().copied());
        }
        roots.extend(
            ir.statics
                .iter()
                .filter(|property| property.owner_matches(&owner))
                .map(|property| property.init),
        );
        scan(&owner, roots);
    }
    result
}

/// The `access$…` bridges of `class`'s private members that another class calls, after its declared
/// members as kotlinc's synthetic-accessor lowering appends them. A member realized as a static
/// value-class `-impl` gets a bridge over the same parameters.
pub(super) fn emit_private_member_access_bridges(
    ir: &IrFile,
    class: &IrClass,
    owner: &str,
    cw: &mut ClassWriter,
    run: &EmitRun,
) {
    let bridged = run.private_member_access_bridges.borrow();
    for &fid in class.methods.iter().filter(|fid| bridged.contains(fid)) {
        if ir.functions[fid as usize].is_static {
            emit_function_reference_access_bridge(ir, fid, owner, cw, false, class.decl_line);
        } else {
            emit_private_member_access_bridge(ir, fid, owner, cw, false, class.decl_line);
        }
    }
}

/// How another class reads a private property through the bridge of its exact `getter`: a static
/// value-class `-impl` getter's bridge takes the same carrier, an instance getter's takes the owner.
pub(super) fn private_member_read_access(
    ir: &IrFile,
    getter: u32,
    owner: &str,
) -> crate::jvm::inline::PropertyAccess {
    use crate::jvm::inline::PropertyAccess;
    let function = &ir.functions[getter as usize];
    let parameters = jvm_function_params(ir, getter);
    let result = jvm_declared_ty(&function.ret);
    let name = format!("access${}", function.name);
    if function.is_static {
        return PropertyAccess::Accessor {
            owner: owner.to_string(),
            name,
            descriptor: method_descriptor(&parameters, result),
            is_static: true,
            is_interface: false,
        };
    }
    let mut bridge_parameters = vec![Ty::obj(owner)];
    bridge_parameters.extend(parameters);
    PropertyAccess::AccessBridge {
        owner: owner.to_string(),
        name,
        descriptor: method_descriptor(&bridge_parameters, result),
    }
}

/// Emit the Java-8 realization of a Kotlin private member used by a lexically related class.
/// The bridge takes the semantic dispatch receiver as its leading static operand, then forwards to
/// the already-selected declaration. No lookup or overload selection occurs here.
pub(super) fn emit_private_member_access_bridge(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    cw: &mut ClassWriter,
    owner_is_interface: bool,
    line: u32,
) {
    let function = &ir.functions[fid as usize];
    debug_assert!(!function.is_static);
    let parameters = jvm_function_params(ir, fid);
    let result = jvm_declared_ty(&function.ret);
    let target_descriptor = method_descriptor(&parameters, result);
    let mut bridge_parameters = Vec::with_capacity(parameters.len() + 1);
    bridge_parameters.push(Ty::obj(owner));
    bridge_parameters.extend(parameters.iter().copied());
    let bridge_descriptor = method_descriptor(&bridge_parameters, result);
    let bridge_name = format!("access${}", function.name);
    let mut code = CodeBuilder::new(
        bridge_parameters
            .iter()
            .map(|parameter| slot_words(*parameter))
            .sum(),
    );
    code.aload(0);
    let mut slot = 1;
    for parameter in &parameters {
        load(*parameter, slot, &mut code);
        slot += slot_words(*parameter);
    }
    let target = if owner_is_interface {
        cw.interface_methodref(owner, &function.name, &target_descriptor)
    } else {
        cw.methodref(owner, &function.name, &target_descriptor)
    };
    let argument_words = parameters
        .iter()
        .map(|parameter| slot_words(*parameter) as i32)
        .sum();
    if line != 0 {
        code.mark_line(line);
    }
    code.invokespecial(target, argument_words, slot_words(result) as i32);
    emit_return(result, &mut code);
    code.ensure_locals(slot);
    code.link();
    cw.add_method(
        if owner_is_interface {
            0x1009 // PUBLIC | STATIC | SYNTHETIC (FINAL is illegal on an interface method)
        } else {
            0x1019 // PUBLIC | STATIC | FINAL | SYNTHETIC
        },
        &bridge_name,
        &bridge_descriptor,
        &code,
    );
    set_bridge_locals(
        ir,
        fid,
        owner,
        &parameters,
        &bridge_name,
        &bridge_descriptor,
        cw,
    );
}

/// The `public static final synthetic access$<name>` forwarder a PRIVATE facade function gets when
/// a class body calls it: a cross-class private `invokestatic` is illegal.
pub(super) fn emit_facade_function_access_bridge(
    ir: &IrFile,
    function: u32,
    facade: &str,
    cw: &mut ClassWriter,
) {
    let f = &ir.functions[function as usize];
    let param_tys = jvm_function_params(ir, function);
    let ret = jvm_declared_ty(&f.ret);
    let desc = method_descriptor(&param_tys, ret);
    let words: u16 = param_tys.iter().map(|t| slot_words(*t)).sum();
    let mut g = CodeBuilder::new(words);
    let mut slot: u16 = 0;
    for &t in &param_tys {
        load(t, slot, &mut g);
        slot += slot_words(t);
    }
    // One accessor per target, as kotlinc's `SyntheticAccessorLowering` keys them: a function whose
    // emission-built state machine re-enters it through `access$<name>` already has it.
    let name = format!("access${}", f.name);
    if cw.declares_method(&name, &desc) {
        return;
    }
    let m = cw.methodref(facade, &f.name, &desc);
    let aw: i32 = words as i32;
    g.invokestatic(m, aw, slot_words(ret) as i32);
    emit_return(ret, &mut g);
    g.ensure_locals(words);
    g.link();
    cw.add_method(
        0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
        &name, &desc, &g,
    );
}

impl Emitter<'_> {
    /// Whether this class reaches `function`, a private member of `owner`, through its bridge.
    pub(super) fn reaches_through_bridge(&self, owner: &str, function: u32) -> bool {
        self.owner != owner
            && self
                .run
                .private_member_access_bridges
                .borrow()
                .contains(&function)
    }
}

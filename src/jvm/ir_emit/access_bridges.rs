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

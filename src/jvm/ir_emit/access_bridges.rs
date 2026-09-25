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
    if function.is_static {
        code.invokestatic(method, argument_words, slot_words(result) as i32);
    } else {
        code.invokespecial(method, argument_words, slot_words(result) as i32);
    }
    emit_return(result, &mut code);
    code.ensure_locals(slot.max(1));
    code.link();
    cw.add_method(
        0x1019, // PUBLIC | STATIC | FINAL | SYNTHETIC
        &format!("access${}", function.name),
        &method_descriptor(&bridge_parameters, result),
        &code,
    );
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
    let m = cw.methodref(facade, &f.name, &desc);
    let aw: i32 = words as i32;
    g.invokestatic(m, aw, slot_words(ret) as i32);
    emit_return(ret, &mut g);
    g.ensure_locals(words);
    g.link();
    cw.add_method(
        0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
        &format!("access${}", f.name),
        &desc,
        &g,
    );
}

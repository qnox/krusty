//! JVM-default compatibility-holder forwarding.

use super::*;

/// Emit one receiver-first `$DefaultImpls` forward to an interface bridge or dependency holder.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_holder_forward(
    cw: &mut ClassWriter,
    interface: crate::types::TypeName,
    member_name: &str,
    param_tys: &[Ty],
    semantic_params: &[Ty],
    param_names: &[String],
    guards: &[Option<String>],
    ret: Ty,
    semantic_ret: Ty,
    signature: Option<&str>,
    decl_line: u32,
    java_parameters: bool,
    target: JdHolderTarget<'_>,
) {
    assert_eq!(
        param_names.len(),
        param_tys.len(),
        "a compatibility holder needs exact declaration parameter identities"
    );
    let fq = interface.render();
    let mut with_receiver = vec![Ty::obj_name(interface)];
    with_receiver.extend_from_slice(param_tys);
    let desc = method_descriptor(&with_receiver, ret);
    cw.seed_utf8(member_name);
    cw.seed_utf8(&desc);
    if let Some(signature) = signature {
        cw.seed_utf8(signature);
    }
    let method_parameters = if java_parameters {
        super::super::method_parameters::holder_forward(param_names, param_tys)
    } else {
        Vec::new()
    };
    for (parameter, _) in &method_parameters {
        cw.seed_utf8(parameter);
    }
    let deprecated = matches!(target, JdHolderTarget::AccessBridge);
    if deprecated {
        cw.seed_utf8("Ljava/lang/Deprecated;");
    }
    let ret_ann = jd_nullability_annotation(&semantic_ret);
    if let Some(annotation) = ret_ann {
        cw.seed_utf8(annotation);
    }
    let mut parameter_annotations = vec![Some("Lorg/jetbrains/annotations/NotNull;")];
    parameter_annotations.extend(semantic_params.iter().map(jd_nullability_annotation));
    for annotation in parameter_annotations.iter().flatten() {
        cw.seed_utf8(annotation);
    }

    let argument_words = 1 + param_tys.iter().map(|ty| slot_words(*ty)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    let mut slot = 1u16;
    for (index, guard) in guards.iter().enumerate().take(param_tys.len()) {
        if let Some(parameter_name) = guard {
            code.aload(slot);
            code.push_string(parameter_name, cw);
            let check = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullParameter",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            );
            code.invokestatic(check, 2, 0);
        }
        slot += slot_words(param_tys[index]);
    }
    if decl_line != 0 {
        code.mark_line(decl_line);
    }
    code.aload(0);
    if let JdHolderTarget::DependencyHolder { declaring, .. } = &target {
        let declaring_class = cw.class_ref(&declaring.render());
        code.checkcast(declaring_class);
    }
    let mut slot = 1u16;
    for ty in param_tys {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    match target {
        JdHolderTarget::AccessBridge => {
            let bridge = cw.interface_methodref(&fq, &format!("access${member_name}$jd"), &desc);
            code.invokestatic(bridge, argument_words as i32, slot_words(ret) as i32);
        }
        JdHolderTarget::DependencyHolder {
            holder, descriptor, ..
        } => {
            let target = cw.methodref(&holder.render(), member_name, descriptor);
            code.invokestatic(target, argument_words as i32, slot_words(ret) as i32);
        }
    }
    emit_return(ret, &mut code);
    code.ensure_locals(argument_words);
    code.link();
    cw.add_method_sig(0x0009, member_name, &desc, &code, signature);
    cw.set_method_parameters(member_name, &desc, &method_parameters);

    let mut locals = vec![("$this".to_string(), format!("L{fq};"), 0)];
    let mut slot = 1u16;
    for (index, parameter) in param_tys.iter().enumerate() {
        let parameter_name = param_names[index].clone();
        locals.push((parameter_name, local_variable_desc(*parameter), slot));
        slot += slot_words(*parameter);
    }
    cw.set_method_debug(member_name, &desc, None, &locals);
    if deprecated {
        cw.mark_method_deprecated(member_name, &desc);
        cw.add_method_visible_marker_annotation(member_name, &desc, "Ljava/lang/Deprecated;");
    }
    if ret_ann.is_some() || parameter_annotations.iter().any(Option::is_some) {
        cw.set_method_nullability(member_name, &desc, ret_ann, &parameter_annotations);
    }
}

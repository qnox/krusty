//! Remaining members of a class-mode lambda after its erased invoke plan is selected.

use super::*;

/// The members after `invoke`: a fun-interface adapter's delegate and equality, the singleton
/// `INSTANCE`, and the class's `@Metadata`.
pub(super) fn finish_lambda_class(
    mut cw: ClassWriter,
    plan: &LambdaClassPlan,
    field_descs: &[String],
    opts: &EmitOptions,
) -> (String, Vec<u8>) {
    if plan.function_adapter {
        assert_eq!(
            plan.captures.len(),
            1,
            "a FunctionAdapter SAM captures exactly its callable reference"
        );
        let delegate_field = cw.fieldref(&plan.internal, "$captured$0", &field_descs[0]);

        let mut delegate = CodeBuilder::new(1);
        delegate.aload(0);
        delegate.getfield(delegate_field, 1);
        delegate.areturn();
        cw.add_method(
            0x0011,
            "getFunctionDelegate",
            "()Lkotlin/Function;",
            &delegate,
        );

        let mut equals = CodeBuilder::new(2);
        let not_same = equals.new_label();
        let adapter = equals.new_label();
        equals.aload(0);
        equals.aload(1);
        equals.if_acmpne(not_same);
        equals.push_int(1, &mut cw);
        equals.ireturn();
        equals.bind(not_same);
        equals.aload(1);
        let function_adapter = cw.class_ref("kotlin/jvm/internal/FunctionAdapter");
        equals.instance_of(function_adapter);
        equals.ifne(adapter);
        equals.push_int(0, &mut cw);
        equals.ireturn();
        equals.bind(adapter);
        equals.aload(0);
        equals.getfield(delegate_field, 1);
        equals.aload(1);
        equals.checkcast(function_adapter);
        let get_delegate = cw.interface_methodref(
            "kotlin/jvm/internal/FunctionAdapter",
            "getFunctionDelegate",
            "()Lkotlin/Function;",
        );
        equals.invokeinterface(get_delegate, 0, 1);
        let object_equals = cw.methodref("java/lang/Object", "equals", "(Ljava/lang/Object;)Z");
        equals.invokevirtual(object_equals, 1, 1);
        equals.ireturn();
        equals.link();
        cw.add_method(0x0011, "equals", "(Ljava/lang/Object;)Z", &equals);

        let mut hash_code = CodeBuilder::new(1);
        hash_code.aload(0);
        hash_code.getfield(delegate_field, 1);
        let object_hash = cw.methodref("java/lang/Object", "hashCode", "()I");
        hash_code.invokevirtual(object_hash, 0, 1);
        hash_code.ireturn();
        cw.add_method(0x0011, "hashCode", "()I", &hash_code);
    }

    if plan.captures.is_empty() {
        let instance_desc = format!("L{};", plan.internal);
        cw.add_field(0x0019, "INSTANCE", &instance_desc); // ACC_PUBLIC | ACC_STATIC | ACC_FINAL
        let mut clinit = CodeBuilder::new(0);
        let class_index = cw.class_ref(&plan.internal);
        clinit.new_obj(class_index);
        clinit.dup();
        let ctor_ref = cw.methodref(&plan.internal, "<init>", "()V");
        clinit.invokespecial(ctor_ref, 0, 0);
        let field = cw.fieldref(&plan.internal, "INSTANCE", &instance_desc);
        clinit.putstatic(field, 1);
        clinit.ret_void();
        cw.add_method(0x0008, "<clinit>", "()V", &clinit); // ACC_STATIC
    }
    if let Some(reflection) = &plan.reflection {
        cw.set_kotlin_metadata(
            3,
            opts.metadata_stamp(),
            synthetic_class_xi(SYNTHETIC_LOCAL),
            &reflection.d1,
            &reflection.d2,
        );
    }
    (plan.internal.clone(), cw.finish())
}

/// kotlinc's typed `invoke(own parameters…)`: the captures read from the class's fields and the
/// arguments passed to the lambda's implementation method, whose scalar result is boxed.
pub(super) fn emit_typed_lambda_invoke(
    cw: &mut ClassWriter,
    plan: &LambdaClassPlan,
    field_descs: &[String],
    own_params: &[Ty],
    impl_ret: Ty,
    typed: &class_lambda_reflection::TypedInvoke,
) {
    let typed_desc = typed.descriptor.as_str();
    let own_words: u16 = own_params.iter().map(|ty| slot_words(*ty)).sum();
    let mut code = CodeBuilder::new(1 + own_words);
    for (index, ty) in plan.captures.iter().enumerate() {
        code.aload(0);
        let field = cw.fieldref(
            &plan.internal,
            &format!("$captured${index}"),
            &field_descs[index],
        );
        code.getfield(field, slot_words(*ty) as i32);
    }
    let mut slot = 1u16;
    for ty in own_params {
        load_slot(&mut code, slot, *ty);
        slot += slot_words(*ty);
    }
    let impl_words: i32 = plan
        .captures
        .iter()
        .chain(own_params)
        .map(|ty| slot_words(*ty) as i32)
        .sum();
    let impl_ref = if plan.owner_is_interface {
        cw.interface_methodref(&plan.impl_owner, &plan.impl_name, &plan.impl_desc)
    } else {
        cw.methodref(&plan.impl_owner, &plan.impl_name, &plan.impl_desc)
    };
    code.invokestatic(impl_ref, impl_words, slot_words(impl_ret) as i32);
    if typed_desc.ends_with(")V") {
        // The implementation of a `Unit` lambda may return the `Unit` value itself.
        match slot_words(impl_ret) {
            1 => code.pop(),
            2 => code.pop2(),
            _ => {}
        }
        code.ret_void();
    } else {
        if !descriptor_is_reference(&type_descriptor(impl_ret)) {
            box_prim_free(cw, &mut code, impl_ret);
        }
        code.areturn();
    }
    // ACC_PUBLIC | ACC_FINAL
    cw.add_method_sig(
        0x0011,
        "invoke",
        typed_desc,
        &code,
        typed.signature.as_deref(),
    );
}

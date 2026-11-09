//! Remaining members of a class-mode lambda after its erased invoke plan is selected.

use super::*;

/// The members after the SAM slot: a fun-interface adapter's delegate and equality, and the
/// singleton `INSTANCE`.
pub(super) fn finish_lambda_class(
    mut cw: ClassWriter,
    plan: &LambdaClassPlan,
    field_descs: &[String],
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
    (plan.internal.clone(), cw.finish())
}

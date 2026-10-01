//! The `INSTANCE` a class kotlinc makes a singleton of is read through: a callable-reference or
//! lambda class that holds no state.

use super::*;

/// `public static final INSTANCE` of the class's own type.
pub(super) fn add_singleton_instance_field(cw: &mut ClassWriter, class: &str) {
    cw.add_field(0x0019, "INSTANCE", &format!("L{class};"));
}

/// `<clinit>`: construct the one instance and store it in `INSTANCE`.
pub(super) fn emit_singleton_instance_clinit(cw: &mut ClassWriter, class: &str) {
    // The method header interns before its code, as a writer visiting the method first does.
    cw.seed_utf8("<clinit>");
    cw.seed_utf8("()V");
    let descriptor = format!("L{class};");
    let classifier = cw.class_ref(class);
    let constructor = cw.methodref(class, "<init>", "()V");
    let field = cw.fieldref(class, "INSTANCE", &descriptor);
    let mut code = CodeBuilder::new(0);
    code.new_obj(classifier);
    code.dup();
    code.invokespecial(constructor, 0, 0);
    code.putstatic(field, 1);
    code.ret_void();
    finish_code::<0x0008>(cw, "<clinit>", "()V", &mut code, 0);
}

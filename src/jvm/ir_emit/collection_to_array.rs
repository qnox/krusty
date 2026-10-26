//! JVM emission of a collection class's `toArray` members (kotlinc's `ToArrayLowering`).
//!
//! Both are public, delegate to `kotlin.jvm.internal.CollectionToArray`, and carry no line numbers:
//! kotlinc builds them without a source offset. They follow every other member the lowering
//! pipeline adds before `<clinit>`, the generic one first.

use super::{finish_code_sig, ClassWriter, CodeBuilder, EmitEnv};
use crate::ir::IrClass;

const COLLECTION: &str = "java/util/Collection";
const TO_ARRAY: &str = "kotlin/jvm/internal/CollectionToArray";
const OBJECTS: &str = "[Ljava/lang/Object;";

pub(super) fn emit(class: &IrClass, cw: &mut ClassWriter, env: &EmitEnv<'_>) {
    let members = env
        .collection_members
        .to_array
        .for_class(class.fq_name_id());
    let this = (String::from("this"), format!("L{};", class.fq_name()), 0);
    if members.generic {
        let descriptor = format!("({OBJECTS}){OBJECTS}");
        let signature = "<T:Ljava/lang/Object;>([TT;)[TT;";
        cw.reserve_method_pool("toArray", &descriptor, Some(signature), &[]);
        let mut code = CodeBuilder::new(2);
        code.aload(1);
        code.push_string("array", cw);
        let check = cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "checkNotNullParameter",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        );
        code.invokestatic(check, 2, 0);
        code.aload(0);
        let collection = cw.class_ref(COLLECTION);
        code.checkcast(collection);
        code.aload(1);
        let to_array = cw.methodref(
            TO_ARRAY,
            "toArray",
            &format!("(L{COLLECTION};{OBJECTS}){OBJECTS}"),
        );
        code.invokestatic(to_array, 2, 1);
        code.areturn();
        finish_code_sig::<0x0001>(cw, "toArray", &descriptor, &mut code, 2, Some(signature));
        let locals = [
            this.clone(),
            (String::from("array"), OBJECTS.to_string(), 1),
        ];
        cw.set_method_debug("toArray", &descriptor, None, &locals);
    }
    if members.non_generic {
        let descriptor = format!("(){OBJECTS}");
        cw.reserve_method_pool("toArray", &descriptor, None, &[]);
        let mut code = CodeBuilder::new(1);
        code.aload(0);
        let collection = cw.class_ref(COLLECTION);
        code.checkcast(collection);
        let to_array = cw.methodref(TO_ARRAY, "toArray", &format!("(L{COLLECTION};){OBJECTS}"));
        code.invokestatic(to_array, 1, 1);
        code.areturn();
        finish_code_sig::<0x0001>(cw, "toArray", &descriptor, &mut code, 1, None);
        cw.set_method_debug("toArray", &descriptor, None, &[this]);
    }
}

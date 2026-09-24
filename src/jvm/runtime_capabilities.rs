//! Runtime helper contracts selected from exact provider declarations.
//!
//! A helper owner's presence is not a method capability. This boundary verifies the physical JVM
//! declaration before a backend pass is allowed to emit a call to it.

use super::classpath::Classpath;
use super::classreader::ClassInfo;

const NULL_OUT_SPILL_OWNER: &str = "kotlin/coroutines/jvm/internal/SpillingKt";
const NULL_OUT_SPILL_NAME: &str = "nullOutSpilledVariable";
const NULL_OUT_SPILL_DESCRIPTOR: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";

/// Whether the selected stdlib provides the exact helper used to clear a dead coroutine spill.
pub(super) fn null_out_spilled_variable(classpath: &Classpath) -> bool {
    classpath.find(NULL_OUT_SPILL_OWNER).is_some_and(|class| {
        has_public_static_method(&class, NULL_OUT_SPILL_NAME, NULL_OUT_SPILL_DESCRIPTOR)
    })
}

fn has_public_static_method(class: &ClassInfo, name: &str, descriptor: &str) -> bool {
    class.methods.iter().any(|method| {
        method.name == name
            && method.descriptor == descriptor
            && method.is_public()
            && method.is_static()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::{ClassWriter, CodeBuilder, ACC_PRIVATE, ACC_PUBLIC, ACC_STATIC};
    use crate::jvm::classreader::parse_class;

    const DESCRIPTOR: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";

    fn class_with_method(access: u16, descriptor: &str, argument_locals: u16) -> ClassInfo {
        let mut class = ClassWriter::new("fixture/probe/SpillCleanup", "java/lang/Object");
        let mut code = CodeBuilder::new(argument_locals);
        code.aconst_null();
        code.areturn();
        class.add_method(access, "clearDeadReference", descriptor, &code);
        parse_class(&class.finish()).expect("parse fixture class")
    }

    #[test]
    fn exact_public_static_method_rejects_near_matches() {
        let cases = [
            class_with_method(ACC_PUBLIC | ACC_STATIC, "()Ljava/lang/Object;", 0),
            class_with_method(ACC_PRIVATE | ACC_STATIC, DESCRIPTOR, 1),
            class_with_method(ACC_PUBLIC, DESCRIPTOR, 2),
        ];

        for class in cases {
            assert!(!has_public_static_method(
                &class,
                "clearDeadReference",
                DESCRIPTOR,
            ));
        }
    }

    #[test]
    fn exact_public_static_method_accepts_the_declared_contract() {
        let class = class_with_method(ACC_PUBLIC | ACC_STATIC, DESCRIPTOR, 1);
        assert!(has_public_static_method(
            &class,
            "clearDeadReference",
            DESCRIPTOR,
        ));
    }
}

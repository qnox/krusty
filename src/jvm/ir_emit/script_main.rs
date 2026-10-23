//! The `main` kotlinc gives a script class: a JVM entry point that hands the class and the
//! command-line arguments to the scripting runtime, which constructs and runs the script.

use crate::jvm::classfile::{ClassWriter, CodeBuilder};

const ARGUMENTS: &str = "args";
const ARGUMENTS_DESCRIPTOR: &str = "[Ljava/lang/String;";
const MAIN_DESCRIPTOR: &str = "([Ljava/lang/String;)V";

/// `public static final void main(String[] args)`, which calls
/// `kotlin.script.experimental.jvm.RunnerKt.runCompiledScript(Class, String[])`.
pub(super) fn emit(script: &str, param_assertions: bool, cw: &mut ClassWriter) {
    cw.reserve_method_pool("main", MAIN_DESCRIPTOR, None, &[]);
    let mut code = CodeBuilder::new(1);
    if param_assertions {
        code.aload(0);
        code.push_string(ARGUMENTS, cw);
        let check = cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "checkNotNullParameter",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        );
        code.invokestatic(check, 2, 0);
    }
    code.ldc_class(script, cw);
    code.aload(0);
    let run = cw.methodref(
        "kotlin/script/experimental/jvm/RunnerKt",
        "runCompiledScript",
        "(Ljava/lang/Class;[Ljava/lang/String;)V",
    );
    code.invokestatic(run, 2, 0);
    code.ret_void();
    cw.add_method(0x0019, "main", MAIN_DESCRIPTOR, &code);
    cw.set_method_debug(
        "main",
        MAIN_DESCRIPTOR,
        None,
        &[(ARGUMENTS.to_string(), ARGUMENTS_DESCRIPTOR.to_string(), 0)],
    );
}

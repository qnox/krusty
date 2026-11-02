//! kotlinc's launcher bridge for a parameterless `fun main()`.
//!
//! The JVM launcher looks for `main([Ljava/lang/String;)V`; the no-argument form is only recognized
//! by JEP 445 (Java 21+ preview, final in 25). When the frontend selected a parameterless `main()`
//! as its unit's entry point, kotlinc therefore adds a synthetic `main(String[])` that calls it, so
//! the output runs on any JVM. The frontend's selection already prefers a declared
//! `main(args: Array<String>)`; the bridge additionally never duplicates a facade method that
//! already has the launcher's JVM signature, such as an `Array<String?>` parameter, an
//! `Array<String>.main()` extension, or a `@JvmName("main")` function taking the arguments.

use super::*;
use crate::ir::MainEntryParameters;

const LAUNCHER_NAME: &str = "main";
const LAUNCHER_DESCRIPTOR: &str = "([Ljava/lang/String;)V";

/// The parameterless entry point that gets a launcher bridge on this facade.
pub(super) struct MainBridge {
    entry: u32,
}

impl MainBridge {
    /// The bridge the facade needs, given the functions it emits.
    pub(super) fn plan(ir: &IrFile, env: &EmitEnv, facade_functions: &[u32]) -> Option<MainBridge> {
        let entry = ir
            .entry_point
            .filter(|entry| entry.parameters == MainEntryParameters::None)?;
        // A `suspend fun main` has its continuation parameter after lowering; its bridge needs the
        // coroutine runner and is not emitted yet.
        if !ir.functions[entry.function as usize].params.is_empty()
            || !facade_functions.contains(&entry.function)
        {
            return None;
        }
        let declares_launcher = facade_functions.iter().any(|&function| {
            ir.functions[function as usize].name == LAUNCHER_NAME
                && method_descriptor(
                    &jvm_function_params(ir, function),
                    jvm_declared_ty(&env.override_results.physical_result(ir, function)),
                ) == LAUNCHER_DESCRIPTOR
        });
        (!declares_launcher).then_some(MainBridge {
            entry: entry.function,
        })
    }

    /// Whether the bridge follows `function`, the entry point it calls.
    pub(super) fn follows(&self, function: u32) -> bool {
        self.entry == function
    }

    pub(super) fn emit(&self, ir: &IrFile, facade: &str, cw: &mut ClassWriter) {
        // kotlinc visits the bridge's header before its body's call to `main()`.
        cw.reserve_method_pool(LAUNCHER_NAME, LAUNCHER_DESCRIPTOR, None, &[]);
        let mut bridge = CodeBuilder::new(1);
        let entry = &ir.functions[self.entry as usize].name;
        let target = cw.methodref(facade, entry, "()V");
        bridge.invokestatic(target, 0, 0);
        bridge.ret_void();
        bridge.ensure_locals(1);
        bridge.link();
        cw.add_method(
            0x1009, // PUBLIC | STATIC | SYNTHETIC
            LAUNCHER_NAME,
            LAUNCHER_DESCRIPTOR,
            &bridge,
        );
        cw.set_method_debug(
            LAUNCHER_NAME,
            LAUNCHER_DESCRIPTOR,
            None,
            &[("args".to_string(), "[Ljava/lang/String;".to_string(), 0)],
        );
    }
}

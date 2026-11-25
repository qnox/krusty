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
use krusty::ir::MainEntryParameters;

const LAUNCHER_NAME: &str = "main";
const LAUNCHER_DESCRIPTOR: &str = "([Ljava/lang/String;)V";

/// The launcher bridge a facade still owes, or none when it needs none or has emitted it.
///
/// kotlinc appends the bridge to the file's declarations: it follows every declared function and
/// precedes the lifted lambdas and local functions, which have no source order.
pub(super) struct PendingMainBridge(Option<MainBridge>);

impl PendingMainBridge {
    pub(super) fn plan(ir: &IrFile, env: &EmitEnv, facade_functions: &[u32]) -> Self {
        PendingMainBridge(MainBridge::plan(ir, env, facade_functions))
    }

    /// Emit the bridge ahead of `function` when it is the first one without a source position,
    /// after `preceding` emits what kotlinc generates before it.
    pub(super) fn before_function(
        &mut self,
        ir: &IrFile,
        function: u32,
        facade: &str,
        cw: &mut ClassWriter,
        preceding: impl FnOnce(&mut ClassWriter),
    ) {
        if !ir.fn_source_order.contains_key(&function) {
            self.finish(ir, facade, cw, preceding);
        }
    }

    /// Emit the bridge, after `preceding`, if it has not been emitted yet.
    pub(super) fn finish(
        &mut self,
        ir: &IrFile,
        facade: &str,
        cw: &mut ClassWriter,
        preceding: impl FnOnce(&mut ClassWriter),
    ) {
        if let Some(bridge) = self.0.take() {
            preceding(cw);
            bridge.emit(ir, facade, cw);
        }
    }
}

/// The parameterless entry point that gets a launcher bridge on this facade.
struct MainBridge {
    entry: u32,
}

impl MainBridge {
    /// The bridge the facade needs, given the functions it emits.
    fn plan(ir: &IrFile, env: &EmitEnv, facade_functions: &[u32]) -> Option<MainBridge> {
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

    fn emit(&self, ir: &IrFile, facade: &str, cw: &mut ClassWriter) {
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

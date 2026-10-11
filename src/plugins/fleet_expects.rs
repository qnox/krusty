//! Native reimplementation of JetBrains Fleet's `expects` compiler plugin
//! (`fleet.multiplatform.expects.ExpectsPluginRegistrar`, plugin id `expects-compiler-plugin`).
//!
//! Fleet replaces Kotlin's `expect`/`actual` with a convention its plugin links after the
//! frontend: a function whose whole body is a call of `fleet.util.multiplatform.linkToActual()`
//! is implemented by the top-level function in the same package that carries
//! `@fleet.util.multiplatform.Actual`, has the same name plus the platform suffix (`Jvm` on the
//! JVM), and the same signature. kotlinc's plugin rewrites the body into
//! `return <name>Jvm(<parameters>)` and reports its errors on the declarations involved.
//!
//! The selection is a frontend decision (`resolve::plugin_expects`): the checker identifies the
//! `linkToActual()` call by the declaration it resolved to, selects the actual among the module's
//! declarations, reports the plugin's errors, and records the selected declaration. Checked FIR
//! then builds the call from that decision (`fir::body_check::linked_actual`). This module owns only
//! the plugin's activation and its platform naming convention.

use crate::compilation_target::CompilationTarget;
use crate::plugins::IrPlugin;

/// The Fleet `expects` plugin. It has no options.
pub struct FleetExpectsPlugin;

impl IrPlugin for FleetExpectsPlugin {
    fn name(&self) -> &str {
        "fleet-expects"
    }

    fn links_actual_functions(&self) -> bool {
        true
    }
}

/// The suffix that names an `@Actual` function for `target`: kotlinc's plugin capitalizes each
/// `-`-separated word of the single target platform's name (`JVM` → `Jvm`).
pub fn actual_function_suffix(target: CompilationTarget) -> &'static str {
    match target {
        CompilationTarget::Jvm => "Jvm",
        CompilationTarget::Js => "Js",
        CompilationTarget::WasmJs => "WasmJs",
        CompilationTarget::WasmWasi => "WasmWasi",
        CompilationTarget::Native => "Native",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_jvm_suffix_is_kotlincs_capitalized_platform_name() {
        assert_eq!(actual_function_suffix(CompilationTarget::Jvm), "Jvm");
        assert!(FleetExpectsPlugin.links_actual_functions());
    }
}

//! Native reimplementation of the Compose compiler plugin (`androidx.compose.compiler.plugins.kotlin`).
//!
//! kotlinc's plugin rewrites every `@Composable` function into the Compose runtime's calling
//! convention: each one takes the caller's `Composer` and a `$changed` bit set, and its body runs
//! inside a group the runtime can skip and restart. The pass here covers that transform part by
//! part, byte for byte with kotlinc's output; [`source_facts`] rejects every composable construct
//! it does not cover yet, so a module either compiles exactly as kotlinc compiles it or fails.
//! Until the coverage is complete the registry reports the plugin as only partly implemented, and
//! a build gives a Compose module to kotlinc.
//!
//! Covered: `Unit` top-level composable functions with a block body whose parameters are
//! primitives, `String` or `@Composable () -> Unit` values, with constant default arguments, made
//! restartable and skippable with per-parameter change tracking, their calls to one another in the
//! same file, and calls of their lambda parameters, whose applier scheme is inferred. A composable
//! lambda such a call passes that captures nothing becomes a property of the file's
//! `ComposableSingletons` object ([`singleton_lambdas`]), with a restartable group of its own; one
//! that captures value parameters of its function is remembered in the composition at the call.
//! Source information, trace markers and `@FunctionKeyMeta` follow the plugin's options. Every
//! public or internal class gets the `$stable` field and `@StabilityInferred` kotlinc infers for it
//! ([`class_stability`]).

mod applier_targets;
mod class_stability;
mod durable_keys;
mod options;
mod restart_groups;
mod runtime;
mod singleton_lambdas;
mod source_facts;

pub use options::ComposeOptions;
pub use source_facts::ComposeFileFacts;

use crate::ir::IrFile;
use crate::plugins::runtime_declarations::PluginRuntime;
use crate::plugins::{
    IrPlugin, PluginContext, PluginRejection, PluginRelease, PluginSourceView, TransformedEntry,
};

/// The runtime's `@Composable` annotation.
const COMPOSABLE: &str = "androidx/compose/runtime/Composable";

/// The Compose plugin, configured for one compilation.
pub struct ComposePlugin {
    options: ComposeOptions,
    /// Where the file's composable singletons are created, which the plugin release decides.
    singleton_creation: singleton_lambdas::SingletonCreation,
}

impl ComposePlugin {
    pub fn new(options: ComposeOptions) -> Self {
        Self {
            options,
            singleton_creation: singleton_lambdas::SingletonCreation::of_release(None),
        }
    }

    /// Reproduce the compiler plugin of `release`, or the newest one when no jar named a release
    /// (see [`PluginRelease`]).
    pub fn with_compiler_plugin_release(mut self, release: Option<PluginRelease>) -> Self {
        self.singleton_creation = singleton_lambdas::SingletonCreation::of_release(release);
        self
    }
}

impl IrPlugin for ComposePlugin {
    fn name(&self) -> &str {
        "compose"
    }

    fn runtime_requirements(
        &self,
    ) -> Vec<crate::plugins::runtime_declarations::RuntimeRequirement> {
        runtime::requirements()
    }

    fn record_source_facts(
        &self,
        ir: &mut IrFile,
        source: &PluginSourceView<'_>,
        runtime: &mut dyn PluginRuntime,
        rejections: &mut Vec<PluginRejection>,
    ) {
        let settings = source_facts::Settings {
            options: &self.options,
            singleton_creation: self.singleton_creation,
        };
        source_facts::record(ir, source, runtime, settings, rejections);
    }

    fn transform_bodies(&self, ir: &mut IrFile, _ctx: &PluginContext) {
        restart_groups::transform(ir);
    }

    fn passes_transformed_entry_defaults(&self, entry: &TransformedEntry<'_>) -> bool {
        entry
            .annotations
            .contains(&crate::types::type_name(COMPOSABLE))
            && source_facts::takes_composer_frame(entry.synthetic, entry.parameters, true)
    }
}

#[cfg(test)]
mod transformed_entry_tests {
    use super::{ComposeOptions, ComposePlugin, COMPOSABLE};
    use crate::plugins::{IrPlugin, TransformedEntry};
    use crate::types::{type_name, Ty};

    fn composer() -> Ty {
        Ty::obj("androidx/compose/runtime/Composer")
    }

    fn passes(annotations: &[&str], synthetic: &[Ty]) -> bool {
        let annotations = annotations
            .iter()
            .map(|annotation| type_name(annotation))
            .collect::<Vec<_>>();
        ComposePlugin::new(ComposeOptions::from_options(&[])).passes_transformed_entry_defaults(
            &TransformedEntry {
                annotations: &annotations,
                synthetic,
                parameters: 3,
            },
        )
    }

    #[test]
    fn a_composable_with_the_composer_changed_and_mask_is_the_entry_this_plugin_passes() {
        assert!(passes(&[COMPOSABLE], &[composer(), Ty::Int, Ty::Int]));
    }

    #[test]
    fn a_tail_of_other_roles_ending_in_the_mask_words_is_not() {
        assert!(!passes(&[COMPOSABLE], &[Ty::Long, Ty::Int, Ty::Int]));
        assert!(!passes(&[COMPOSABLE], &[Ty::Int, Ty::Int, Ty::Int]));
        assert!(!passes(
            &[COMPOSABLE],
            &[Ty::obj("lib/NotAComposer"), Ty::Int, Ty::Int]
        ));
        assert!(!passes(&[COMPOSABLE], &[composer(), Ty::Long, Ty::Int]));
        assert!(!passes(
            &[COMPOSABLE],
            &[composer(), Ty::Int, Ty::Int, Ty::Int]
        ));
    }

    #[test]
    fn the_same_frame_on_a_declaration_that_is_not_composable_is_not() {
        let frame = [composer(), Ty::Int, Ty::Int];
        assert!(!passes(&[], &frame));
        assert!(!passes(&["kotlin/Deprecated"], &frame));
    }
}

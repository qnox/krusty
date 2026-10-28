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
//! Covered: parameterless `Unit` top-level composable functions with a block body, made
//! restartable and skippable, and their calls to one another in the same file. Source
//! information, trace markers and `@FunctionKeyMeta` follow the plugin's options.

mod durable_keys;
mod options;
mod restart_groups;
mod runtime;
mod source_facts;

pub use options::ComposeOptions;
pub use source_facts::ComposeFileFacts;

use crate::ir::IrFile;
use crate::plugins::{IrPlugin, PluginContext, PluginRejection, PluginSourceView};

/// The runtime's `@Composable` annotation.
const COMPOSABLE: &str = "androidx/compose/runtime/Composable";

/// The Compose plugin, configured for one compilation.
pub struct ComposePlugin {
    options: ComposeOptions,
}

impl ComposePlugin {
    pub fn new(options: ComposeOptions) -> Self {
        Self { options }
    }
}

impl IrPlugin for ComposePlugin {
    fn name(&self) -> &str {
        "compose"
    }

    fn record_source_facts(
        &self,
        ir: &mut IrFile,
        source: &PluginSourceView<'_>,
        rejections: &mut Vec<PluginRejection>,
    ) {
        source_facts::record(ir, source, &self.options, rejections);
    }

    fn transform_bodies(&self, ir: &mut IrFile, _ctx: &PluginContext) {
        restart_groups::transform(ir);
    }
}

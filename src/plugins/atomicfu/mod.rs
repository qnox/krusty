//! Native reimplementation of the kotlinx-atomicfu compiler plugin (`org.jetbrains.kotlinx.atomicfu`).
//!
//! kotlinc's plugin replaces every `kotlinx.atomicfu` atomic a class declares with the platform's
//! own atomic primitive, so the library's wrapper classes never exist at run time. On the JVM an
//! atomic property of a class becomes a `volatile` field updated through a
//! `java.util.concurrent.atomic` field updater.
//!
//! The work is split at the frontend/backend handoff. [`source_facts`] classifies the file's
//! atomics and the operations on them while their checked identities are still at hand, rejects
//! every form this implementation does not cover yet, and selects the representation the
//! compilation's target supports. A JVM compilation whose `java.util.concurrent.atomic` field
//! updaters are present lowers through those. Wasm lowers the value field with plain reads and
//! writes, matching the wasm atomicfu runtime. Native lowers that field with atomic instructions.
//! A JVM compilation without the updater classes, and the JavaScript target, are rejected with a
//! diagnostic. [`field_updaters`] rewrites the JVM backend's IR; [`direct_fields`] rewrites the
//! others at the handoff. The class's Kotlin metadata is recorded before either runs, so it keeps
//! the declared atomic property, as kotlinc's does.

mod direct_fields;
mod field_updaters;
mod source_facts;

pub use source_facts::AtomicfuFileFacts;

use crate::compilation_target::CompilationTarget;
use crate::ir::IrFile;
use crate::plugins::runtime_declarations::PluginRuntime;
use crate::plugins::{IrPlugin, PluginContext, PluginRejection, PluginSourceView};

pub struct AtomicfuPlugin {
    release: Option<crate::plugins::PluginRelease>,
    target: Option<CompilationTarget>,
}

impl AtomicfuPlugin {
    pub fn new(
        release: Option<crate::plugins::PluginRelease>,
        target: Option<CompilationTarget>,
    ) -> Self {
        Self { release, target }
    }
}

impl IrPlugin for AtomicfuPlugin {
    fn name(&self) -> &str {
        "atomicfu"
    }

    fn record_source_facts(
        &self,
        ir: &mut IrFile,
        source: &PluginSourceView<'_>,
        runtime: &mut dyn PluginRuntime,
        rejections: &mut Vec<PluginRejection>,
    ) {
        source_facts::record(ir, source, runtime, rejections, self.release, self.target);
    }

    fn transform_bodies(&self, ir: &mut IrFile, _ctx: &PluginContext) {
        field_updaters::transform(ir);
    }
}

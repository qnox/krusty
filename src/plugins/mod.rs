//! krusty plugin API (PoC) — the compiler-extension surface. See `docs/PLUGIN_API.md`.
//!
//! Kotlin extensions live in two worlds with very different coupling to compiler internals, so
//! krusty supports each through a different door:
//!
//!   1. NATIVE IR PLUGINS ([`IrPlugin`]) — the in-process equivalent of Kotlin's FIR
//!      declaration/supertype generation + IR backend transforms (Compose, kotlinx.serialization).
//!      They run as passes alongside `jvm::value_classes::lower_value_classes`. They CAN synthesize
//!      and mutate declarations. Reference impl: [`serialization`].
//!
//!   2. CODEGEN HOSTS ([`ksp`]) — the in-process host for codegen-only annotation processors
//!      (KSP, APT: Micronaut, Dagger, Room). Those run UNMODIFIED on a JVM sidecar through a shim
//!      that implements their interfaces; they only READ a resolved symbol view and EMIT new source
//!      files. Modeled here in Rust to prove the front-stage fixpoint pipeline.
//!
//! AST vs IR (see the doc): declaration/supertype generation is PRODUCTION-HOSTED at the signature
//! phase (pre-typecheck) so generated symbols resolve, and so the same resolved-symbol view backs a
//! future LSP. Only body/expression rewrite genuinely belongs at the IR level. This self-contained
//! PoC runs all three phases over `IrFile` for testability; the phase split is documented per-hook.

pub mod cli;
pub mod ksp;
pub mod registry;
pub mod serialization;

use std::collections::HashMap;

use crate::ir::{ClassId, IrFile};

/// Side table of applied annotations, keyed by `ClassId`. A side table because `IrClass` does not
/// yet store applied annotations (only known-flag bools like `is_data`). The production integration
/// adds `annotations: Vec<String>` to `IrClass`, populated in `ir_lower`; then this becomes a thin
/// accessor over the IR. Kept separate here so the gate stays `0 FAIL` (no edits to `IrClass`
/// struct-literal sites).
#[derive(Default, Clone)]
pub struct PluginContext {
    pub class_annotations: HashMap<ClassId, Vec<String>>,
}

impl PluginContext {
    /// `ClassId`s carrying the annotation `fq` (a Kotlin FqName, e.g. `kotlinx/serialization/Serializable`).
    pub fn classes_with(&self, fq: &str) -> Vec<ClassId> {
        let mut ids: Vec<ClassId> = self
            .class_annotations
            .iter()
            .filter(|(_, anns)| anns.iter().any(|a| a == fq))
            .map(|(&id, _)| id)
            .collect();
        ids.sort_unstable(); // deterministic order (HashMap iteration is not)
        ids
    }

    pub fn has_annotation(&self, class: ClassId, fq: &str) -> bool {
        self.class_annotations
            .get(&class)
            .is_some_and(|anns| anns.iter().any(|a| a == fq))
    }
}

/// A native IR plugin — the in-process equivalent of Kotlin's FIR + IR backend extensions. Each
/// method mirrors one real Kotlin extension point so a port maps method-for-method. All three have
/// a no-op default, so a plugin overrides only the phases it needs.
pub trait IrPlugin {
    fn name(&self) -> &str;

    /// `FirSupertypeGenerationExtension` — add interfaces/superclasses to *existing* classes
    /// (Parcelize makes a class implement `Parcelable`). PRODUCTION: signature phase (pre-typecheck).
    fn generate_supertypes(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}

    /// `FirDeclarationGenerationExtension` — synthesize *new* classes/members (serialization's
    /// `$serializer` + `serializer()`). PRODUCTION: signature phase, so user references resolve.
    fn generate_declarations(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}

    /// `IrGenerationExtension` (backend IR) — fill in / rewrite method bodies (serialize/deserialize,
    /// Compose `$composer` threading). This is the genuine IR-level hook; runs post-`ir_lower`.
    fn transform_bodies(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}
}

/// Runs registered plugins over an `IrFile`, phase by phase **globally** (all plugins' supertypes,
/// then all declarations, then all bodies) — matching kotlinc's phase ordering, so one plugin can
/// rely on another's supertypes being in place before its declarations run.
#[derive(Default)]
pub struct PluginHost {
    plugins: Vec<Box<dyn IrPlugin>>,
}

impl PluginHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: Box<dyn IrPlugin>) {
        self.plugins.push(plugin);
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Names of the registered plugins, in run order (introspection / tests).
    pub fn plugin_names(&self) -> Vec<&str> {
        self.plugins.iter().map(|p| p.name()).collect()
    }

    pub fn run(&self, ir: &mut IrFile, ctx: &PluginContext) {
        for p in &self.plugins {
            p.generate_supertypes(ir, ctx);
        }
        for p in &self.plugins {
            p.generate_declarations(ir, ctx);
        }
        for p in &self.plugins {
            p.transform_bodies(ir, ctx);
        }
    }
}

/// Fill all `IrClass` fields with empty defaults for a synthesized class — a builder helper plugins
/// use so adding a node doesn't depend on every field. (`IrClass` deliberately has no `Default`
/// derive in production code; this is a PoC convenience local to the plugin layer.)
pub(crate) fn synthetic_class(fq_name: impl Into<String>) -> crate::ir::IrClass {
    crate::ir::IrClass {
        fq_name: fq_name.into(),
        is_value: false,
        type_param_bounds: Vec::new(),
        field_type_params: Vec::new(),
        supertypes: Vec::new(),
        fields: Vec::new(),
        ctor_param_count: 0,
        ctor_args: Vec::new(),
        init_body: None,
        methods: Vec::new(),
        is_interface: false,
        superclass: "java/lang/Object".to_string(),
        super_args: Vec::new(),
        enum_entries: Vec::new(),
        enum_entry_subclass: Vec::new(),
        enum_entry_of: None,
        prop_ref: None,
        bridges: Vec::new(),
        interfaces: Vec::new(),
        is_object: false,
        ctor_param_checks: Vec::new(),
        is_companion: false,
        companion_class: None,
        field_final: Vec::new(),
        secondary_ctors: Vec::new(),
        has_primary_ctor: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrFile;

    struct TouchPlugin;
    impl IrPlugin for TouchPlugin {
        fn name(&self) -> &str {
            "touch"
        }
        fn generate_declarations(&self, ir: &mut IrFile, _ctx: &PluginContext) {
            ir.classes.push(synthetic_class("demo/Generated"));
        }
    }

    #[test]
    fn context_indexes_annotations() {
        let mut ctx = PluginContext::default();
        ctx.class_annotations
            .insert(0, vec!["a/B".to_string(), "c/D".to_string()]);
        ctx.class_annotations.insert(1, vec!["a/B".to_string()]);
        assert_eq!(ctx.classes_with("a/B"), vec![0, 1]);
        assert_eq!(ctx.classes_with("c/D"), vec![0]);
        assert!(ctx.has_annotation(0, "c/D"));
        assert!(!ctx.has_annotation(1, "c/D"));
    }

    #[test]
    fn host_runs_registered_plugins() {
        let mut host = PluginHost::new();
        host.register(Box::new(TouchPlugin));
        let mut ir = IrFile::default();
        host.run(&mut ir, &PluginContext::default());
        assert_eq!(ir.classes.len(), 1);
        assert_eq!(ir.classes[0].fq_name, "demo/Generated");
    }
}

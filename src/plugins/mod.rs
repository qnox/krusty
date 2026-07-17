//! krusty plugin API. See `docs/PLUGIN_API.md`.
//!
//! Kotlin extensions have different coupling to compiler internals, so krusty exposes separate
//! native-plugin and codegen-host APIs:
//!
//!   1. NATIVE IR PLUGINS ([`IrPlugin`]) — the in-process equivalent of Kotlin's FIR
//!      declaration/supertype generation + IR backend transforms (Compose, kotlinx.serialization).
//!      They run as compiler passes and can synthesize or mutate declarations. Reference impl:
//!      [`serialization`].
//!
//!   2. CODEGEN HOSTS ([`ksp`]) — the in-process host for codegen-only annotation processors
//!      (KSP, APT: Micronaut, Dagger, Room). They run on a JVM sidecar through a shim that
//!      implements their interfaces, read a resolved symbol view, and emit new source files.
//!
//! AST vs IR (see the doc): declaration/supertype generation belongs before type checking so
//! generated symbols resolve. Body and expression rewriting belongs at the IR level.

pub mod cli;
pub mod deps;
pub mod ksp;
pub mod registry;
pub mod serialization;

use std::collections::HashMap;

use crate::ir::{ClassId, IrFile};
use crate::types::Ty;

/// The unqualified tail of an annotation name (`kotlinx/serialization/Serializable` or
/// `kotlinx.serialization.Serializable` → `Serializable`).
pub(crate) fn annotation_simple_name(a: &str) -> &str {
    a.rsplit(['/', '.']).next().unwrap_or(a)
}

/// Side table of applied annotations, keyed by `ClassId`, plus target services required by native
/// plugins.
pub struct PluginContext {
    pub class_annotations: HashMap<ClassId, Vec<String>>,
    target_type_descriptor: fn(Ty) -> Option<String>,
}

impl Default for PluginContext {
    fn default() -> Self {
        Self {
            class_annotations: HashMap::new(),
            target_type_descriptor: no_target_type_descriptor,
        }
    }
}

impl Clone for PluginContext {
    fn clone(&self) -> Self {
        Self {
            class_annotations: self.class_annotations.clone(),
            target_type_descriptor: self.target_type_descriptor,
        }
    }
}

impl PluginContext {
    pub fn with_target_type_descriptor(mut self, f: fn(Ty) -> Option<String>) -> Self {
        self.target_type_descriptor = f;
        self
    }

    pub fn target_type_descriptor(&self, ty: Ty) -> Option<String> {
        (self.target_type_descriptor)(ty)
    }

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

    /// `ClassId`s carrying an annotation whose **simple name** equals `simple` — so a source
    /// `@Serializable` (captured as `"Serializable"`) and a fully-qualified
    /// `kotlinx/serialization/Serializable` both match.
    pub fn classes_with_simple(&self, simple: &str) -> Vec<ClassId> {
        let mut ids: Vec<ClassId> = self
            .class_annotations
            .iter()
            .filter(|(_, anns)| anns.iter().any(|a| annotation_simple_name(a) == simple))
            .map(|(&id, _)| id)
            .collect();
        ids.sort_unstable();
        ids
    }

    pub fn has_annotation(&self, class: ClassId, fq: &str) -> bool {
        self.class_annotations
            .get(&class)
            .is_some_and(|anns| anns.iter().any(|a| a == fq))
    }

    /// Build the annotation index from parsed source by matching class declarations to IR classes by
    /// fully-qualified internal name.
    pub fn from_source(file: &crate::ast::File, ir: &IrFile) -> PluginContext {
        use std::collections::HashMap;
        let pkg_prefix = file
            .package
            .as_deref()
            .map(|p| format!("{}/", p.replace('.', "/")))
            .unwrap_or_default();
        // Key by fully-qualified internal name (`pkg/Foo`), matching IrClass.fq_name exactly. A NESTED
        // class is hoisted with a dot-separated name (`Outer.Inner`) but its IrClass.fq_name uses `$`
        // (`pkg/Outer$Inner`), so normalize `.`→`$` to match.
        let by_fq: HashMap<String, &Vec<String>> = file
            .decl_arena
            .iter()
            .filter_map(|d| match d {
                crate::ast::Decl::Class(c) if !c.annotations.is_empty() => Some((
                    format!("{pkg_prefix}{}", c.name.replace('.', "$")),
                    &c.annotations,
                )),
                _ => None,
            })
            .collect();
        let mut ctx = PluginContext::default();
        for (i, c) in ir.classes.iter().enumerate() {
            if let Some(anns) = by_fq.get(&c.fq_name) {
                ctx.class_annotations.insert(i as u32, (*anns).clone());
            }
        }
        ctx
    }
}

fn no_target_type_descriptor(_ty: Ty) -> Option<String> {
    None
}

/// A native IR plugin with explicit hooks for supertype generation, declaration generation, and IR
/// body transformation.
pub trait IrPlugin {
    fn name(&self) -> &str;

    /// Add interfaces or superclasses to existing classes.
    fn generate_supertypes(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}

    /// Synthesize new classes or members.
    fn generate_declarations(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}

    /// Fill in or rewrite method bodies after IR lowering.
    fn transform_bodies(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}
}

/// Run the natively-supported compiler-extension plugins over a freshly-lowered `IrFile`, driven by
/// the file's source annotations.
pub fn run_enabled(
    ir: &mut IrFile,
    file: &crate::ast::File,
    module_name: &str,
    target_type_descriptor: fn(Ty) -> Option<String>,
) {
    let ctx =
        PluginContext::from_source(file, ir).with_target_type_descriptor(target_type_descriptor);
    if ctx.classes_with_simple("Serializable").is_empty() {
        return;
    }
    // The `write$Self$<module>` helper is mangled with the compilation's module name (kotlinc's
    // >=1.6 ABI); thread it through so it matches the real Gradle module, not the "main" default.
    let mut host = PluginHost::new();
    host.register(Box::new(serialization::SerializationPlugin::new(
        serialization::SerializationAbi::default(),
        module_name,
    )));
    host.run(ir, &ctx);
}

/// Runs registered plugins over an `IrFile` phase by phase: all supertypes, then all declarations,
/// then all body transforms.
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

/// Fill all `IrClass` fields with empty defaults for a synthesized class.
pub(crate) fn synthetic_class(fq_name: impl Into<String>) -> crate::ir::IrClass {
    crate::ir::IrClass {
        fq_name: fq_name.into(),
        serial_names: Vec::new(),
        custom_serializer: None,
        field_serializers: Vec::new(),
        contextual_fields: Vec::new(),
        is_value: false,
        type_param_bounds: Vec::new(),
        type_params: Vec::new(),
        supertypes: Vec::new(),
        fields: Vec::new(),
        ctor_param_count: 0,
        ctor_args: Vec::new(),
        init_body: None,
        explicit_param_stores: false,
        methods: Vec::new(),
        is_interface: false,
        is_annotation: false,
        annotation_impl_of: None,
        is_sealed: false,
        is_abstract: false,
        superclass: "java/lang/Object".to_string(),
        super_args: Vec::new(),
        enum_entries: Vec::new(),
        enum_entry_of: None,
        prop_ref: None,
        bridges: Vec::new(),
        interfaces: Vec::new(),
        is_object: false,
        is_companion: false,
        companion_class: None,
        func_ref: None,
        secondary_ctors: Vec::new(),
        has_primary_ctor: true,
        applied_annotations: Vec::new(),
        field_annotations: Vec::new(),
        runtime_retained: false,
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
    fn context_carries_target_type_descriptor_service() {
        fn descriptor(ty: Ty) -> Option<String> {
            (ty == Ty::Int).then(|| "I".to_string())
        }

        assert_eq!(
            PluginContext::default().target_type_descriptor(Ty::Int),
            None
        );
        assert_eq!(
            PluginContext::default()
                .with_target_type_descriptor(descriptor)
                .target_type_descriptor(Ty::Int)
                .as_deref(),
            Some("I")
        );
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

    #[test]
    fn from_source_matches_by_fqname_not_simple_name() {
        use crate::ast::{ClassDecl, Decl, File};
        // Two classes, same simple name `Foo`, different packages — only the annotated one's IrClass
        // must receive the annotation (no simple-name cross-contamination).
        let mut file = File {
            package: Some("a.b".to_string()),
            ..File::default()
        };
        let annotated = ClassDecl {
            name: "Foo".to_string(),
            annotations: vec!["Serializable".to_string()],
            ..blank_class("Foo")
        };
        file.decl_arena.push(Decl::Class(annotated));

        let mut ir = IrFile::default();
        let other = ir.add_class(synthetic_class("x/y/Foo")); // same simple name, different package
        let target = ir.add_class(synthetic_class("a/b/Foo")); // the real one

        let ctx = PluginContext::from_source(&file, &ir);
        assert!(ctx.has_annotation(target, "Serializable"));
        assert!(
            !ctx.has_annotation(other, "Serializable"),
            "annotation must not bleed onto a same-simple-name class in another package"
        );
    }

    /// A `ClassDecl` with only the fields `from_source` reads (name + annotations) populated.
    fn blank_class(name: &str) -> crate::ast::ClassDecl {
        let src = format!("class {name}");
        let mut d = crate::diag::DiagSink::new();
        let toks = crate::lexer::lex(&src, &mut d);
        let file = crate::parser::parse(&src, &toks, &mut d);
        match file.decl_arena.into_iter().next() {
            Some(crate::ast::Decl::Class(c)) => c,
            _ => unreachable!(),
        }
    }
}

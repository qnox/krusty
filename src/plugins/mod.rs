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

use crate::ir::{ClassId, IrFile};
use crate::types::Ty;
use std::collections::HashMap;

/// The unqualified tail of an annotation name (`kotlinx/serialization/Serializable` or
/// `kotlinx.serialization.Serializable` → `Serializable`).
pub(crate) fn annotation_simple_name(a: &str) -> &str {
    a.rsplit(['/', '.']).next().unwrap_or(a)
}

/// Applied annotations keyed by `ClassId`, plus source and target services required by native
/// plugins. Production contexts borrow annotation slices from the parsed source; owned annotations
/// are only for synthetic tests/manual plugin harnesses.
type ClassNameResolver<'a> = dyn Fn(&str) -> Option<String> + 'a;

pub struct PluginContext<'a> {
    pub class_annotations: HashMap<ClassId, AnnotationList<'a>>,
    source_file: Option<&'a crate::ast::File>,
    class_name_resolver: Option<&'a ClassNameResolver<'a>>,
    target_type_descriptor: fn(Ty) -> Option<String>,
}

#[derive(Clone, Debug)]
pub enum AnnotationList<'a> {
    Borrowed(&'a [String]),
    Owned(Vec<String>),
}

impl<'a> AnnotationList<'a> {
    pub fn as_slice(&self) -> &[String] {
        match self {
            AnnotationList::Borrowed(annotations) => annotations,
            AnnotationList::Owned(annotations) => annotations,
        }
    }
}

impl<'a> From<&'a [String]> for AnnotationList<'a> {
    fn from(value: &'a [String]) -> Self {
        AnnotationList::Borrowed(value)
    }
}

impl From<Vec<String>> for AnnotationList<'_> {
    fn from(value: Vec<String>) -> Self {
        AnnotationList::Owned(value)
    }
}

impl Default for AnnotationList<'_> {
    fn default() -> Self {
        AnnotationList::Owned(Vec::new())
    }
}

impl Default for PluginContext<'_> {
    fn default() -> Self {
        Self {
            class_annotations: HashMap::new(),
            source_file: None,
            class_name_resolver: None,
            target_type_descriptor: no_target_type_descriptor,
        }
    }
}

impl Clone for PluginContext<'_> {
    fn clone(&self) -> Self {
        Self {
            class_annotations: self.class_annotations.clone(),
            source_file: self.source_file,
            class_name_resolver: self.class_name_resolver,
            target_type_descriptor: self.target_type_descriptor,
        }
    }
}

impl<'a> PluginContext<'a> {
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
            .filter(|(_, anns)| anns.as_slice().iter().any(|a| a == fq))
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
            .filter(|(_, anns)| {
                anns.as_slice()
                    .iter()
                    .any(|a| annotation_simple_name(a) == simple)
            })
            .map(|(&id, _)| id)
            .collect();
        ids.sort_unstable();
        ids
    }

    pub fn has_annotation(&self, class: ClassId, fq: &str) -> bool {
        self.class_annotations
            .get(&class)
            .is_some_and(|anns| anns.as_slice().iter().any(|a| a == fq))
    }

    fn source_class(&self, ir: &IrFile, class: ClassId) -> Option<&'a crate::ast::ClassDecl> {
        let file = self.source_file?;
        let internal = &ir.classes.get(class as usize)?.fq_name;
        source_class_by_internal(file, internal)
    }

    fn class_literal_internal(
        &self,
        file: &crate::ast::File,
        e: crate::ast::ExprId,
    ) -> Option<String> {
        let crate::ast::Expr::CallableRef {
            receiver: Some(r),
            name,
        } = file.expr(e)
        else {
            return None;
        };
        if name != "class" {
            return None;
        }
        let crate::ast::Expr::Name(x) = file.expr(*r) else {
            return None;
        };
        self.class_name_resolver
            .and_then(|resolve| resolve(x))
            .or_else(|| Some(source_internal(file, x)))
    }

    pub fn class_annotation_class_literal_internal(
        &self,
        ir: &IrFile,
        class: ClassId,
        annotation: &str,
    ) -> Option<String> {
        let file = self.source_file?;
        let c = self.source_class(ir, class)?;
        let i = c
            .annotations
            .iter()
            .position(|a| annotation_simple_name(a) == annotation)?;
        let arg = c.annotation_args.get(i).and_then(|args| args.first())?;
        self.class_literal_internal(file, *arg)
    }

    pub fn property_annotation_class_literal_internal(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
        annotation: &str,
    ) -> Option<String> {
        let file = self.source_file?;
        let p = self
            .source_class(ir, class)?
            .props
            .iter()
            .find(|p| p.name == property)?;
        let i = p
            .annotations
            .iter()
            .position(|a| annotation_simple_name(a) == annotation)?;
        let arg = p.annotation_args.get(i).and_then(|args| args.first())?;
        self.class_literal_internal(file, *arg)
    }

    pub fn property_annotation_const_string(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
        annotation: &str,
    ) -> Option<String> {
        let file = self.source_file?;
        let p = self
            .source_class(ir, class)?
            .props
            .iter()
            .find(|p| p.name == property)?;
        let i = p
            .annotations
            .iter()
            .position(|a| annotation_simple_name(a) == annotation)?;
        let arg = p.annotation_args.get(i).and_then(|args| args.first())?;
        const_string_value(file, *arg)
    }

    pub fn property_has_annotation_simple(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
        annotation: &str,
    ) -> bool {
        self.source_class(ir, class)
            .and_then(|c| c.props.iter().find(|p| p.name == property))
            .is_some_and(|p| {
                p.annotations
                    .iter()
                    .any(|a| annotation_simple_name(a) == annotation)
            })
    }

    pub fn property_canonical_type_name(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
    ) -> Option<String> {
        let file = self.source_file?;
        let p = self
            .source_class(ir, class)?
            .props
            .iter()
            .find(|p| p.name == property)?;
        Some(canonical_type_name(file, &p.ty.name))
    }

    pub fn file_annotation_mentions_canonical_type(
        &self,
        annotation: &str,
        canonical_type: &str,
    ) -> bool {
        let Some(file) = self.source_file else {
            return false;
        };
        file.file_annotations
            .iter()
            .filter(|(ann, _)| annotation_simple_name(ann) == annotation)
            .flat_map(|(_, args)| args)
            .filter_map(|&arg| class_literal_name(file, arg))
            .map(|name| canonical_type_name(file, name))
            .any(|name| name == canonical_type)
    }

    /// Build the annotation index from parsed source by matching class declarations to IR classes by
    /// fully-qualified internal name.
    pub fn from_source(file: &'a crate::ast::File, ir: &IrFile) -> PluginContext<'a> {
        Self::from_source_with_class_resolver(file, ir, None)
    }

    pub fn from_source_with_class_resolver(
        file: &'a crate::ast::File,
        ir: &IrFile,
        class_name_resolver: Option<&'a ClassNameResolver<'a>>,
    ) -> PluginContext<'a> {
        let mut ctx = PluginContext {
            class_annotations: HashMap::new(),
            source_file: Some(file),
            class_name_resolver,
            target_type_descriptor: no_target_type_descriptor,
        };
        for (i, c) in ir.classes.iter().enumerate() {
            if let Some(cd) = source_class_by_internal(file, &c.fq_name) {
                if !cd.annotations.is_empty() {
                    ctx.class_annotations
                        .insert(i as u32, AnnotationList::Borrowed(&cd.annotations));
                }
            }
        }
        ctx
    }
}

fn source_class_by_internal<'a>(
    file: &'a crate::ast::File,
    internal: &str,
) -> Option<&'a crate::ast::ClassDecl> {
    let pkg_prefix = file
        .package
        .as_deref()
        .map(|p| format!("{}/", p.replace('.', "/")))
        .unwrap_or_default();
    file.decl_arena.iter().find_map(|d| match d {
        crate::ast::Decl::Class(c) if format!("{pkg_prefix}{}", c.name) == internal => Some(c),
        _ => None,
    })
}

fn source_internal(file: &crate::ast::File, name: &str) -> String {
    let mangled = name.replace('.', "$");
    match &file.package {
        Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), mangled),
        _ => mangled,
    }
}

fn class_literal_name(file: &crate::ast::File, e: crate::ast::ExprId) -> Option<&str> {
    let crate::ast::Expr::CallableRef {
        receiver: Some(r),
        name,
    } = file.expr(e)
    else {
        return None;
    };
    if name != "class" {
        return None;
    }
    match file.expr(*r) {
        crate::ast::Expr::Name(x) => Some(x.as_str()),
        _ => None,
    }
}

fn canonical_type_name(file: &crate::ast::File, name: &str) -> String {
    file.type_aliases
        .iter()
        .find_map(|(a, t)| (a == name).then(|| t.clone()))
        .unwrap_or_else(|| name.to_string())
}

fn const_string_value(file: &crate::ast::File, e: crate::ast::ExprId) -> Option<String> {
    const_string_value_d(file, e, 0)
}

fn const_string_value_d(
    file: &crate::ast::File,
    e: crate::ast::ExprId,
    depth: u32,
) -> Option<String> {
    if depth > 32 {
        return None;
    }
    match file.expr(e) {
        crate::ast::Expr::StringLit(s) => Some(s.clone()),
        crate::ast::Expr::Name(n) => top_level_const_string_d(file, n, depth + 1),
        crate::ast::Expr::Template(parts) => {
            let mut out = String::new();
            for p in parts {
                match p {
                    crate::ast::TemplatePart::Str(s) => out.push_str(s),
                    crate::ast::TemplatePart::Expr(x) => {
                        out.push_str(&const_string_value_d(file, *x, depth + 1)?)
                    }
                }
            }
            Some(out)
        }
        _ => None,
    }
}

fn top_level_const_string_d(file: &crate::ast::File, name: &str, depth: u32) -> Option<String> {
    if depth > 32 {
        return None;
    }
    file.decls.iter().find_map(|&d| match file.decl(d) {
        crate::ast::Decl::Property(p) if p.name == name => p
            .init
            .and_then(|i| const_string_value_d(file, i, depth + 1)),
        _ => None,
    })
}

fn no_target_type_descriptor(_ty: Ty) -> Option<String> {
    None
}

/// A native IR plugin with explicit hooks for supertype generation, declaration generation, and IR
/// body transformation.
pub trait IrPlugin {
    fn name(&self) -> &str;

    /// Add interfaces or superclasses to existing classes.
    fn generate_supertypes(&self, _ir: &mut IrFile, _ctx: &PluginContext<'_>) {}

    /// Synthesize new classes or members.
    fn generate_declarations(&self, _ir: &mut IrFile, _ctx: &PluginContext<'_>) {}

    /// Fill in or rewrite method bodies after IR lowering.
    fn transform_bodies(&self, _ir: &mut IrFile, _ctx: &PluginContext<'_>) {}
}

/// Run the natively-supported compiler-extension plugins over a freshly-lowered `IrFile`, driven by
/// the file's source annotations.
pub fn run_enabled(
    ir: &mut IrFile,
    file: &crate::ast::File,
    class_name_resolver: &ClassNameResolver<'_>,
    target_type_descriptor: fn(Ty) -> Option<String>,
) {
    let ctx = PluginContext::from_source_with_class_resolver(file, ir, Some(class_name_resolver))
        .with_target_type_descriptor(target_type_descriptor);
    if ctx.classes_with_simple("Serializable").is_empty() {
        return;
    }
    let mut host = PluginHost::new();
    host.register(Box::new(serialization::SerializationPlugin::default()));
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

    pub fn run(&self, ir: &mut IrFile, ctx: &PluginContext<'_>) {
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
        fn generate_declarations(&self, ir: &mut IrFile, _ctx: &PluginContext<'_>) {
            ir.classes.push(synthetic_class("demo/Generated"));
        }
    }

    #[test]
    fn context_indexes_annotations() {
        let mut ctx = PluginContext::default();
        ctx.class_annotations
            .insert(0, vec!["a/B".to_string(), "c/D".to_string()].into());
        ctx.class_annotations
            .insert(1, vec!["a/B".to_string()].into());
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

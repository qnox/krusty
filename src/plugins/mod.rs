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
pub mod codegen_loop;
pub mod deps;
pub mod ksp;
pub mod registry;
pub mod serialization;

use crate::ir::{ClassId, IrFile};
use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// A closed expression operation selected by a frontend plugin after ordinary name and overload
/// resolution. Core lowering copies this payload into an [`crate::ir::IrExpr::PluginPlaceholder`];
/// only the named plugin interprets the operation and data. This keeps plugin API recognition and
/// target realization out of the lowerer.
#[derive(Clone, Debug)]
pub struct PluginExpressionPlan {
    pub plugin: &'static str,
    pub operation: &'static str,
    pub data: Vec<TypeName>,
    /// Checker-selected source operands in target-parameter order with their selected parameter
    /// types. Lowering evaluates/coerces exactly this list and performs no argument remapping.
    pub operands: Vec<(crate::ast::ExprId, Ty)>,
}

/// One already-selected call projected into the plugin contract. Declaration identity, overload,
/// type arguments, and argument mapping are final; plugins may only attach an implementation plan.
#[derive(Clone, Debug)]
pub struct FrontendSelectedCall {
    pub expression: crate::ast::ExprId,
    /// Explicit source receiver and its final checked type. Plugins consume the parser coordinate
    /// while the bounded AST is live; checked FIR retains only the resulting operand.
    pub explicit_receiver: Option<(crate::ast::ExprId, Ty)>,
    pub owner: TypeName,
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub generic_sig: Option<crate::libraries::GenericSig>,
    pub inline: crate::libraries::InlineKind,
    pub implementation: Option<crate::libraries::PluginExpressionDeclaration>,
    pub type_arguments: Vec<Option<Ty>>,
    pub argument_slots: Vec<Option<crate::ast::ExprId>>,
}

/// Read-only, resolver-independent inputs for post-resolution plugin planning.
pub struct FrontendExpressionContext {
    pub calls: Vec<FrontendSelectedCall>,
    pub source_annotations: HashMap<TypeName, Vec<TypeName>>,
}

impl FrontendExpressionContext {
    pub fn has_source_annotation(&self, classifier: TypeName, annotation: TypeName) -> bool {
        self.source_annotations
            .get(&classifier)
            .is_some_and(|annotations| annotations.contains(&annotation))
    }
}

/// A callable declaration contributed by a compiler plugin before type checking. The frontend
/// publishes these through the same classifier member table as source declarations; resolution does
/// not know which plugin produced them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontendCallable {
    pub owner: FrontendCallableOwner,
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub generic_sig: Option<crate::libraries::GenericSig>,
    pub plugin_expression: Option<crate::libraries::PluginExpressionDeclaration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontendCallableOwner {
    Classifier,
    Companion,
}

/// The resolved source-class information available to frontend declaration generation.
pub struct FrontendClassContext<'a> {
    pub classifier: TypeName,
    pub kind: crate::libraries::TypeKind,
    pub type_parameters: &'a crate::types::TypeParameters<Vec<Ty>>,
    pub annotations: &'a [TypeName],
}

/// Applied annotations keyed by `ClassId`, plus target services required by native plugins.
/// Contexts are built exclusively from checked common IR: plugins never resolve source spelling.
pub struct PluginContext {
    pub class_annotations: HashMap<ClassId, Vec<TypeName>>,
    target_type_descriptor: fn(Ty) -> Option<String>,
    /// Types this compilation does NOT declare that already carry a plugin-generated serializer on
    /// the classpath — another module's `@Serializable` classes. A plugin can only DERIVE a
    /// serializer for what this file declares; for everything else the dependency's own generated
    /// one is the answer, and this set is how the target tells the plugin which types have one.
    external_serializers: std::collections::HashSet<String>,
}

impl Default for PluginContext {
    fn default() -> Self {
        Self {
            class_annotations: HashMap::new(),
            target_type_descriptor: no_target_type_descriptor,
            external_serializers: std::collections::HashSet::new(),
        }
    }
}

impl Clone for PluginContext {
    fn clone(&self) -> Self {
        Self {
            class_annotations: self.class_annotations.clone(),
            target_type_descriptor: self.target_type_descriptor,
            external_serializers: self.external_serializers.clone(),
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

    /// Record the types whose serializer already exists outside this compilation.
    pub fn with_external_serializers(
        mut self,
        serializers: std::collections::HashSet<String>,
    ) -> Self {
        self.external_serializers = serializers;
        self
    }

    /// Whether `internal` (a JVM internal name) carries a plugin-generated serializer the target can
    /// reference directly.
    pub fn has_external_serializer(&self, internal: &str) -> bool {
        self.external_serializers.contains(internal)
    }

    /// `ClassId`s carrying the exact resolved annotation identity.
    pub fn classes_with(&self, annotation: TypeName) -> Vec<ClassId> {
        let mut ids: Vec<ClassId> = self
            .class_annotations
            .iter()
            .filter(|(_, annotations)| annotations.contains(&annotation))
            .map(|(&id, _)| id)
            .collect();
        ids.sort_unstable(); // deterministic order (HashMap iteration is not)
        ids
    }

    pub fn has_annotation(&self, class: ClassId, annotation: TypeName) -> bool {
        self.class_annotations
            .get(&class)
            .is_some_and(|annotations| annotations.contains(&annotation))
    }

    fn ir_property_annotations<'ir>(
        &self,
        ir: &'ir IrFile,
        class: ClassId,
        property: &str,
    ) -> Option<&'ir crate::ir::DeclarationAnnotations> {
        ir.classes
            .get(class as usize)?
            .property_annotations
            .iter()
            .find(|annotations| annotations.property == property)
            .map(|annotations| &annotations.annotations)
    }

    fn annotation_named(
        annotations: &crate::ir::DeclarationAnnotations,
        annotation: TypeName,
    ) -> Option<&crate::ir::AppliedAnnotation> {
        annotations
            .applications()
            .find(|application| application.internal == annotation)
    }

    pub fn class_annotation_class_literal_internal(
        &self,
        ir: &IrFile,
        class: ClassId,
        annotation: TypeName,
    ) -> Option<String> {
        let value = ir
            .classes
            .get(class as usize)
            .and_then(|class| Self::annotation_named(&class.applied_annotations, annotation))
            .and_then(|annotation| annotation.values.first())
            .map(|(_, value)| value)?;
        match value {
            crate::ir::AnnoValue::Class(classifier) => Some(classifier.render()),
            _ => None,
        }
    }

    pub fn property_annotation_class_literal_internal(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
        annotation: TypeName,
    ) -> Option<String> {
        let value = self
            .ir_property_annotations(ir, class, property)
            .and_then(|annotations| Self::annotation_named(annotations, annotation))
            .and_then(|annotation| annotation.values.first())
            .map(|(_, value)| value)?;
        match value {
            crate::ir::AnnoValue::Class(classifier) => Some(classifier.render()),
            _ => None,
        }
    }

    pub fn property_annotation_const_string(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
        annotation: TypeName,
    ) -> Option<crate::kt_string::KtString> {
        let value = self
            .ir_property_annotations(ir, class, property)
            .and_then(|annotations| Self::annotation_named(annotations, annotation))
            .and_then(|annotation| annotation.values.first())
            .map(|(_, value)| value)?;
        match value {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::String(value)) => Some(value.clone()),
            _ => None,
        }
    }

    pub fn property_has_annotation(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
        annotation: TypeName,
    ) -> bool {
        self.ir_property_annotations(ir, class, property)
            .is_some_and(|annotations| Self::annotation_named(annotations, annotation).is_some())
    }

    pub fn property_canonical_type(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
    ) -> Option<TypeName> {
        ir.classes
            .get(class as usize)
            .and_then(|class| {
                class
                    .properties
                    .iter()
                    .find(|candidate| candidate.name == property)
            })
            .map(|property| property.ty)
            .and_then(|ty| ty.non_null().kotlin_class_internal())
    }

    pub fn file_annotation_mentions_canonical_type(
        &self,
        ir: &IrFile,
        annotation: TypeName,
        canonical_type: TypeName,
    ) -> bool {
        Self::annotation_named(&ir.file_annotations, annotation).is_some_and(|annotation| {
            annotation
                .values
                .iter()
                .any(|(_, value)| annotation_value_mentions_class(value, canonical_type))
        })
    }

    /// Build a plugin context exclusively from checked common IR. All annotation arguments have
    /// already been resolved and folded by the frontend; no parser declaration or source spelling
    /// is available to plugin realization.
    pub fn from_ir(ir: &IrFile) -> PluginContext {
        let mut context = PluginContext::default();
        for (class, declaration) in ir.classes.iter().enumerate() {
            let annotations = declaration
                .applied_annotations
                .applications()
                .map(|annotation| annotation.internal)
                .collect::<Vec<_>>();
            if !annotations.is_empty() {
                context.class_annotations.insert(class as u32, annotations);
            }
        }
        context
    }
}

fn annotation_value_mentions_class(value: &crate::ir::AnnoValue, canonical_type: TypeName) -> bool {
    match value {
        crate::ir::AnnoValue::Class(classifier) => *classifier == canonical_type,
        crate::ir::AnnoValue::Annotation(annotation) => annotation
            .values
            .iter()
            .any(|(_, value)| annotation_value_mentions_class(value, canonical_type)),
        crate::ir::AnnoValue::Array(values) => values
            .iter()
            .any(|value| annotation_value_mentions_class(value, canonical_type)),
        crate::ir::AnnoValue::Const(_) | crate::ir::AnnoValue::Enum(_, _) => false,
    }
}

fn no_target_type_descriptor(_ty: Ty) -> Option<String> {
    None
}

/// A native IR plugin with explicit hooks for supertype generation, declaration generation, and IR
/// body transformation.
pub trait IrPlugin {
    fn name(&self) -> &str;

    /// Contribute declarations that source code may reference. These declarations are published
    /// before checking; their bodies and target-specific realization remain later plugin phases.
    fn generate_frontend_declarations(
        &self,
        _ctx: &FrontendClassContext<'_>,
        _members: &mut Vec<FrontendCallable>,
    ) {
    }

    /// Attach plugin-owned expression plans after core has selected declarations and overloads.
    /// Implementations must identify an exact selected declaration; this hook is not a fallback name
    /// resolver and must never change the type checker's result.
    fn plan_frontend_expressions(
        &self,
        _ctx: &FrontendExpressionContext,
        _plans: &mut Vec<(crate::ast::ExprId, PluginExpressionPlan)>,
    ) {
    }

    /// Add interfaces or superclasses to existing classes.
    fn generate_supertypes(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}

    /// Synthesize new classes or members.
    fn generate_declarations(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}

    /// Fill in or rewrite method bodies after IR lowering.
    fn transform_bodies(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}
}

/// Run native backend plugins from frontend-checked common IR. Annotation names and values have
/// already been resolved and folded, so neither reparsed source nor spelling participates.
pub fn run_enabled(
    ir: &mut IrFile,
    module_name: &str,
    target_type_descriptor: fn(Ty) -> Option<String>,
    generated_serializer_exists: &dyn Fn(&str) -> bool,
) {
    let ctx = PluginContext::from_ir(ir).with_target_type_descriptor(target_type_descriptor);
    if ctx
        .classes_with(crate::types::type_name(serialization::SERIALIZABLE_FQ))
        .is_empty()
    {
        return;
    }
    let ctx = ctx.with_external_serializers(external_serializers(ir, generated_serializer_exists));
    enabled_plugins(module_name).run(ir, &ctx);
}

/// Field types this file does NOT declare whose generated serializer the target can reach: another
/// file of this module, or a dependency. A plugin derives a serializer only for what IT generates;
/// every other `@Serializable` class brings its own, and only the target knows where those live.
fn external_serializers(
    ir: &IrFile,
    generated_serializer_exists: &dyn Fn(&str) -> bool,
) -> std::collections::HashSet<String> {
    let declared: std::collections::HashSet<String> =
        ir.classes.iter().map(|class| class.fq_name()).collect();
    let mut candidates: Vec<Ty> = ir
        .classes
        .iter()
        .flat_map(|class| class.fields.iter().map(|field| field.ty))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut external = std::collections::HashSet::new();
    while let Some(ty) = candidates.pop() {
        // A type argument carries its own element serializer (`List<Inner>` needs `Inner`'s).
        candidates.extend(ty.non_null().type_args().iter().copied());
        let Some(internal) = ty.kotlin_class_internal().map(|name| name.render()) else {
            continue;
        };
        if !seen.insert(internal.clone()) || declared.contains(&internal) {
            continue;
        }
        if generated_serializer_exists(&internal) {
            external.insert(internal);
        }
    }
    external
}

pub(crate) fn enabled_plugins(module_name: &str) -> PluginHost {
    let mut host = PluginHost::new();
    host.register(Box::new(serialization::SerializationPlugin::new(
        serialization::SerializationAbi::default(),
        module_name,
    )));
    host
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

    pub fn generate_frontend_declarations(
        &self,
        ctx: &FrontendClassContext<'_>,
        members: &mut Vec<FrontendCallable>,
    ) {
        for plugin in &self.plugins {
            plugin.generate_frontend_declarations(ctx, members);
        }
    }

    pub fn plan_frontend_expressions(
        &self,
        ctx: &FrontendExpressionContext,
    ) -> Vec<(crate::ast::ExprId, PluginExpressionPlan)> {
        let mut plans = Vec::new();
        for plugin in &self.plugins {
            plugin.plan_frontend_expressions(ctx, &mut plans);
        }
        plans
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
    crate::ir::IrClass::synthetic(crate::types::type_name(&fq_name.into()))
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
            ir.add_class(synthetic_class("demo/Generated"));
        }
    }

    #[test]
    fn context_indexes_annotations() {
        let mut ctx = PluginContext::default();
        ctx.class_annotations.insert(
            0,
            vec![
                crate::types::type_name("a/Marker"),
                crate::types::type_name("c/D"),
            ],
        );
        ctx.class_annotations
            .insert(1, vec![crate::types::type_name("b/Marker")]);
        assert_eq!(
            ctx.classes_with(crate::types::type_name("a/Marker")),
            vec![0]
        );
        assert_eq!(
            ctx.classes_with(crate::types::type_name("b/Marker")),
            vec![1]
        );
        assert_eq!(ctx.classes_with(crate::types::type_name("c/D")), vec![0]);
        assert!(ctx.has_annotation(0, crate::types::type_name("c/D")));
        assert!(!ctx.has_annotation(1, crate::types::type_name("c/D")));
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
        assert!(ir.classes[0].fq_name_matches("demo/Generated"));
    }

    #[test]
    fn checked_ir_context_needs_no_source_for_class_or_file_annotations() {
        let retained = |name: &str, values| crate::ir::RetainedAnnotation {
            retention: crate::types::AnnotationRetention::Runtime,
            annotation: crate::ir::AppliedAnnotation {
                internal: crate::types::type_name(name),
                values,
            },
        };
        let mut ir = IrFile::default();
        let class = ir.add_class(synthetic_class("demo/Record"));
        ir.classes[class as usize].applied_annotations = crate::ir::DeclarationAnnotations::new(
            vec![retained("kotlinx/serialization/Serializable", Vec::new())],
        );
        ir.file_annotations = crate::ir::DeclarationAnnotations::new(vec![retained(
            "kotlinx/serialization/UseContextualSerialization",
            vec![(
                "forClasses".to_string(),
                crate::ir::AnnoValue::Array(vec![crate::ir::AnnoValue::Class(
                    crate::types::type_name("demo/External"),
                )]),
            )],
        )]);

        let ctx = PluginContext::from_ir(&ir);
        assert_eq!(
            ctx.classes_with(crate::types::type_name(
                "kotlinx/serialization/Serializable"
            )),
            vec![class]
        );
        assert!(ctx.file_annotation_mentions_canonical_type(
            &ir,
            crate::types::type_name("kotlinx/serialization/UseContextualSerialization"),
            crate::types::type_name("demo/External")
        ));
    }
}

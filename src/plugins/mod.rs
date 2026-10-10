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
    /// Resolved semantic types required by the plugin operation. These cross checked FIR as types,
    /// not rendered names, so type arguments and nullability cannot be lost and reconstructed.
    pub types: Vec<Ty>,
    /// Use the resolver-selected implicit receiver for the call as the first operand. The stable
    /// receiver coordinate remains in `TypeInfo`; checked FIR materializes that exact selection.
    pub implicit_receiver: bool,
    /// Checker-selected source operands in target-parameter order with their selected parameter
    /// types. Lowering evaluates/coerces exactly this list and performs no argument remapping.
    pub operands: Vec<(crate::ast::ExprId, Ty)>,
    /// Operands the plugin composes from frontend-selected declarations rather than from source
    /// syntax. They follow `operands`, in order.
    pub synthesized: Vec<PluginSynthesizedOperand>,
}

/// A value a plugin plan composes without source syntax. Every callable it names was selected in
/// the frontend from a declaration's full semantic signature, so lowering realizes the exact
/// declaration and no later phase looks a member up by name.
#[derive(Clone, Debug)]
pub enum PluginSynthesizedOperand {
    /// A checker-selected call on a singleton receiver. The complete ordinary member-call result
    /// crosses this boundary; FIR publication must not reconstruct any target fact from the name.
    SingletonCall {
        call: FrontendResolvedSingletonCall,
        arguments: Vec<PluginSynthesizedOperand>,
    },
    /// A nested plugin operation over synthesized operands, specialized by the named plugin.
    Operation {
        plugin: &'static str,
        operation: &'static str,
        data: Vec<TypeName>,
        types: Vec<Ty>,
        result: Ty,
        operands: Vec<PluginSynthesizedOperand>,
    },
}

/// One ordinary member call selected by the frontend for a plugin-composed expression.
///
/// The receiver, declaration, specialized signature, capabilities, generic substitutions, and
/// positional argument mapping are all the result of the normal resolver/checker path. A plugin may
/// recognize the selected declaration's semantic shape, but cannot enumerate or choose candidates.
#[derive(Clone, Debug)]
pub struct FrontendResolvedSingletonCall {
    pub receiver: TypeName,
    pub selected: FrontendSelectedMemberCall,
    pub type_arguments: Vec<Option<Ty>>,
    pub type_argument_bounds: Vec<Vec<Ty>>,
    pub argument_parameters: Vec<u32>,
}

/// The closed semantic result of ordinary member-call selection that a plugin-composed operand
/// needs. This contract owns no candidate or resolver state: core projects the selected receiver,
/// declaration, specialized result, and suspension fact before the plugin sees it.
#[derive(Clone, Debug)]
pub struct FrontendSelectedMemberCall {
    pub receiver: Ty,
    pub member: crate::libraries::LibraryMember,
    pub ret: Ty,
    pub suspend: bool,
}

/// The sole semantic query available while a plugin composes a source-less call. Implementations
/// route it through ordinary member candidate collection, overload selection, generic inference,
/// argument mapping, and access checking.
pub trait FrontendCallResolver {
    fn resolve_singleton_member_call(
        &self,
        classifier: TypeName,
        name: &str,
        arguments: &[Ty],
        expected_result: Ty,
    ) -> Option<FrontendResolvedSingletonCall>;
}

/// One already-selected call projected into the plugin contract. Declaration identity, overload,
/// type arguments, and argument mapping are final; plugins may only attach an implementation plan.
#[derive(Clone, Debug)]
pub struct FrontendSelectedCall {
    pub expression: crate::ast::ExprId,
    /// Explicit source receiver and its final checked type. Plugins consume the parser coordinate
    /// while the bounded AST is live; checked FIR retains only the resulting operand.
    pub explicit_receiver: Option<(crate::ast::ExprId, Ty)>,
    /// Semantic type of the resolver-selected implicit receiver, when the call has no explicit
    /// source receiver. Its stable scope coordinate remains keyed by `expression` in `TypeInfo`.
    pub implicit_receiver: Option<Ty>,
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

/// Read-only inputs for post-resolution plugin planning. Source calls and annotations are already
/// bound; the narrow resolver capability can only select an ordinary singleton member call whose
/// receiver, name, argument types, and expected result the plugin supplies.
pub struct FrontendExpressionContext<'a> {
    pub calls: Vec<FrontendSelectedCall>,
    /// Qualified annotation identities for source classifiers and dependency classifiers named by
    /// these calls.
    pub classifier_annotations: &'a dyn FrontendClassifierAnnotations,
    /// Absent only in isolated plugin-unit tests that do not compose calls.
    pub call_resolver: Option<&'a dyn FrontendCallResolver>,
}

/// The annotation identities of one classifier, answered per query. The frontend serves this from
/// its module index without materializing every source classifier for each planned body.
pub trait FrontendClassifierAnnotations {
    fn classifier_annotations(&self, classifier: TypeName) -> Option<&[TypeName]>;
}

/// An eagerly collected annotation table, keyed by classifier.
pub type ClassifierAnnotationMap = HashMap<TypeName, Vec<TypeName>>;

impl FrontendClassifierAnnotations for ClassifierAnnotationMap {
    fn classifier_annotations(&self, classifier: TypeName) -> Option<&[TypeName]> {
        self.get(&classifier).map(Vec::as_slice)
    }
}

impl FrontendExpressionContext<'_> {
    pub fn has_classifier_annotation(&self, classifier: TypeName, annotation: TypeName) -> bool {
        self.classifier_annotations
            .classifier_annotations(classifier)
            .is_some_and(|annotations| annotations.contains(&annotation))
    }

    pub fn resolve_singleton_member_call(
        &self,
        classifier: TypeName,
        name: &str,
        arguments: &[Ty],
        expected_result: Ty,
    ) -> Option<FrontendResolvedSingletonCall> {
        self.call_resolver?.resolve_singleton_member_call(
            classifier,
            name,
            arguments,
            expected_result,
        )
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
    /// Canonical source-visible identities, exactly parallel to `params`.
    pub param_names: Vec<String>,
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
    pub companion: Option<TypeName>,
    pub kind: crate::libraries::TypeKind,
    pub is_sealed: bool,
    pub type_parameters: &'a crate::types::TypeParameters<Vec<Ty>>,
    pub annotations: &'a [TypeName],
    /// Resolved class-literal arguments grouped by the ordinal of the annotation occurrence.
    pub annotation_class_arguments: &'a [(u32, TypeName)],
}

/// A source class as a frontend plugin CHECKER sees it: the resolved identities of its
/// annotations and the declaration facts of its properties. The checker builds it while the class's
/// syntax is live, from the same checked annotation applications lowering consumes, so a plugin
/// never resolves a spelling and an alias names the class it aliases.
pub struct FrontendClassCheckContext<'a> {
    pub kind: crate::libraries::TypeKind,
    pub annotations: &'a [TypeName],
    /// Explicit class-literal arguments grouped by the ordinal of the annotation occurrence, as in
    /// [`FrontendClassContext::annotation_class_arguments`].
    pub annotation_class_arguments: &'a [(u32, TypeName)],
    /// The class's properties in declaration order: primary-constructor properties first, then
    /// the properties declared in its body.
    pub properties: &'a [FrontendPropertyFacts],
}

/// One property of a [`FrontendClassCheckContext`].
pub struct FrontendPropertyFacts {
    /// Resolved annotation identities, in source order.
    pub annotations: Vec<TypeName>,
    /// Whether the property stores a value: not abstract, delegated or external, and either
    /// accessor-less or with a getter that reads `field`.
    pub has_backing_field: bool,
    /// An initializer, or a primary-constructor property's default value.
    pub has_initializer: bool,
    pub is_lateinit: bool,
    /// The whole declaration, modifiers and annotations included.
    pub declaration_span: crate::diag::Span,
}

/// An error a frontend plugin checker reports against a source declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontendPluginDiagnostic {
    pub span: crate::diag::Span,
    pub message: String,
}

/// A source classifier as a plugin sees it when one of its annotations names a CLASS the plugin
/// constructs on the classifier's behalf (`@Serializable(with = X::class)`): its resolved
/// annotations with their entry spans, its checked applications, and the declaration facts of every
/// source class named by a class-valued argument. The plugin selects and validates the constructor
/// from these facts alone; it never resolves a spelling or scans a class itself.
pub struct FrontendNamedClassContext<'a> {
    /// Resolved annotation identities with the span of each source annotation entry (`@` included).
    pub annotations: &'a [(TypeName, crate::diag::Span)],
    /// The checked applications of those annotations, with their folded named arguments.
    pub applied_annotations: &'a [crate::types::ResolvedAnnotation],
    /// The classifier's own type-parameter count.
    pub type_parameter_count: usize,
    pub named_classes: &'a [FrontendNamedClass],
}

/// A source class named by a class-valued annotation argument.
#[derive(Clone)]
pub struct FrontendNamedClass {
    pub classifier: TypeName,
    /// The class as a diagnostic names it without type arguments: `X<*, *>`.
    pub display_name: String,
    /// Primary-constructor value parameters in declaration order: source name and declared type.
    pub primary_constructor: Vec<(String, Ty)>,
    /// Each declared supertype with its type arguments as diagnostics render them, the class's
    /// own formals named by their owner (`T (of class X<T>)`).
    pub supertype_arguments: Vec<(TypeName, Vec<String>)>,
}

/// The constructor a plugin selected for a named class: the class, and the operand ordinal of the
/// generated caller passed to each primary-constructor parameter, in parameter order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontendNamedClassConstruction {
    pub classifier: TypeName,
    pub operands: Box<[u32]>,
}

/// Applied annotations keyed by `ClassId`, plus target services required by native plugins.
/// Contexts are built exclusively from checked common IR: plugins never resolve source spelling.
pub struct PluginContext {
    pub class_annotations: HashMap<ClassId, Vec<TypeName>>,
    /// Annotation classifiers whose resolved declaration carries kotlinx.serialization's
    /// `@SerialInfo` meta-annotation.
    serial_info_annotations: std::collections::HashSet<TypeName>,
    target_type_descriptor: fn(Ty) -> Option<String>,
    /// Types this compilation does NOT declare, mapped to the serializer that already exists for
    /// them: the one a class names for itself with `@Serializable(with = …)`, or the one another
    /// file's or a dependency's compilation generated for its kind. A plugin can only DERIVE a
    /// serializer for what this file declares; for everything else only the declaration's facts
    /// say where the serializer is.
    external_serializers: std::collections::HashMap<TypeName, serialization::ExternalSerializer>,
    /// Serializer classes the ACTIVE runtime actually provides. A builtin mapping is only usable
    /// when the artifact on the classpath carries that class: `InstantSerializer` ships in newer
    /// kotlinx.serialization cores but not in supported older ones, and emitting a reference to a
    /// class that is not there fails at class-load rather than at compile time.
    runtime_serializers: std::collections::HashSet<TypeName>,
    /// Serializer classes this compilation does NOT declare that a property's `@Serializable(with =
    /// …)` names, on the property or inside its declared type (`List<@Serializable(with = S::class)
    /// T>`), mapped to whether the provider confirms an `object` (read through `INSTANCE`) rather
    /// than a class (constructed). Absent ⇒ the provider could not classify it.
    external_serializer_objects: std::collections::HashMap<TypeName, bool>,
}

impl Default for PluginContext {
    fn default() -> Self {
        Self {
            class_annotations: HashMap::new(),
            serial_info_annotations: std::collections::HashSet::new(),
            target_type_descriptor: no_target_type_descriptor,
            external_serializers: std::collections::HashMap::new(),
            runtime_serializers: std::collections::HashSet::new(),
            external_serializer_objects: std::collections::HashMap::new(),
        }
    }
}

impl Clone for PluginContext {
    fn clone(&self) -> Self {
        Self {
            class_annotations: self.class_annotations.clone(),
            serial_info_annotations: self.serial_info_annotations.clone(),
            target_type_descriptor: self.target_type_descriptor,
            external_serializers: self.external_serializers.clone(),
            runtime_serializers: self.runtime_serializers.clone(),
            external_serializer_objects: self.external_serializer_objects.clone(),
        }
    }
}

impl PluginContext {
    fn with_serial_info_annotations(
        mut self,
        annotations: std::collections::HashSet<TypeName>,
    ) -> Self {
        self.serial_info_annotations = annotations;
        self
    }

    pub(crate) fn is_serial_info_annotation(&self, annotation: TypeName) -> bool {
        self.serial_info_annotations.contains(&annotation)
    }

    pub fn with_target_type_descriptor(mut self, f: fn(Ty) -> Option<String>) -> Self {
        self.target_type_descriptor = f;
        self
    }

    pub fn target_type_descriptor(&self, ty: Ty) -> Option<String> {
        (self.target_type_descriptor)(ty)
    }

    /// Record the serializer that exists outside this compilation for each type that has one.
    pub fn with_external_serializers(
        mut self,
        serializers: std::collections::HashMap<TypeName, serialization::ExternalSerializer>,
    ) -> Self {
        self.external_serializers = serializers;
        self
    }

    /// Record which serializer classes the active runtime provides.
    pub(crate) fn with_runtime_serializers(
        mut self,
        serializers: std::collections::HashSet<TypeName>,
    ) -> Self {
        self.runtime_serializers = serializers;
        self
    }

    /// Record the provider-confirmed kind of each external serializer class a property names.
    pub(crate) fn with_external_serializer_objects(
        mut self,
        objects: std::collections::HashMap<TypeName, bool>,
    ) -> Self {
        self.external_serializer_objects = objects;
        self
    }

    /// Whether external serializer class `serializer` is an `object`; `None` when the provider did
    /// not classify it.
    pub(crate) fn external_serializer_is_object(&self, serializer: TypeName) -> Option<bool> {
        self.external_serializer_objects.get(&serializer).copied()
    }

    /// Whether the active runtime provides `serializer`. A builtin mapping must consult this before
    /// emitting a reference: a class absent from the artifact on the classpath would only fail when
    /// the JVM tried to load it.
    pub(crate) fn runtime_provides(&self, serializer: TypeName) -> bool {
        self.runtime_serializers.contains(&serializer)
    }

    /// The serializer that exists outside this file for `classifier`, when there is one.
    pub fn external_serializer(
        &self,
        classifier: TypeName,
    ) -> Option<&serialization::ExternalSerializer> {
        self.external_serializers.get(&classifier)
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

    pub fn class_annotation_class_literal(
        &self,
        ir: &IrFile,
        class: ClassId,
        annotation: TypeName,
    ) -> Option<TypeName> {
        let value = ir
            .classes
            .get(class as usize)
            .and_then(|class| Self::annotation_named(&class.applied_annotations, annotation))
            .and_then(|annotation| annotation.values.first())
            .map(|(_, value)| value)?;
        match value {
            crate::ir::AnnoValue::Class(ty) => ty.kotlin_class_internal(),
            _ => None,
        }
    }

    pub fn property_annotation_class_literal(
        &self,
        ir: &IrFile,
        class: ClassId,
        property: &str,
        annotation: TypeName,
    ) -> Option<TypeName> {
        let value = self
            .ir_property_annotations(ir, class, property)
            .and_then(|annotations| Self::annotation_named(annotations, annotation))
            .and_then(|annotation| annotation.values.first())
            .map(|(_, value)| value)?;
        match value {
            crate::ir::AnnoValue::Class(ty) => ty.kotlin_class_internal(),
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

    /// The checked declared spelling of `class`'s ordinary property `property`: the same tree, with
    /// the same checked type-use annotation applications, that the property's `@Metadata` records.
    /// The source name locates the already selected serializable field inside this class; its stable
    /// declaration order selects the spelling, so a same-named member extension cannot replace it.
    /// The empty spelling when the declared type spelled no alias and carried no recorded
    /// type-use annotation.
    pub fn property_declared_type<'ir>(
        &self,
        ir: &'ir IrFile,
        class: ClassId,
        property: &str,
    ) -> &'ir crate::ir::Spelled {
        ir.classes
            .get(class as usize)
            .and_then(|class| {
                let property = class
                    .properties
                    .iter()
                    .find(|candidate| candidate.name == property)?;
                ir.prop_declared_spellings
                    .get(&(class.fq_name_id(), property.source_order))
            })
            .map_or(crate::ir::Spelled::NONE, |spellings| &spellings.ret)
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
        crate::ir::AnnoValue::Class(ty) => ty.kotlin_class_internal() == Some(canonical_type),
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

    /// Publish exact generated nested-class identities alongside the source classifier header.
    /// Consumers must not reconstruct these declarations from annotation or JVM-name spellings.
    fn publish_frontend_generated_classifiers(
        &self,
        _ctx: &FrontendClassContext<'_>,
        _classifiers: &mut Vec<crate::types::GeneratedClassifierFact>,
    ) {
    }

    /// Attach plugin-owned expression plans after core has selected declarations and overloads.
    /// Implementations must identify an exact selected declaration; this hook is not a fallback name
    /// resolver and must never change the type checker's result.
    fn plan_frontend_expressions(
        &self,
        _ctx: &FrontendExpressionContext<'_>,
        _plans: &mut Vec<(crate::ast::ExprId, PluginExpressionPlan)>,
    ) {
    }

    /// Report a source class the plugin rejects, as kotlinc's plugin checkers do. This runs in the
    /// frontend, so a rejected class never reaches backend generation.
    fn check_frontend_class(
        &self,
        _ctx: &FrontendClassCheckContext<'_>,
        _diagnostics: &mut Vec<FrontendPluginDiagnostic>,
    ) {
    }

    /// Select the primary constructor through which the plugin's generated code constructs a class
    /// one of the classifier's annotations names, reporting a constructor that breaks the plugin's
    /// contract instead. Runs once per source classifier, while its declarations are live.
    fn select_named_class_constructor(
        &self,
        _ctx: &FrontendNamedClassContext<'_>,
        _diagnostics: &mut Vec<FrontendPluginDiagnostic>,
    ) -> Option<FrontendNamedClassConstruction> {
        None
    }

    /// Add interfaces or superclasses to existing classes.
    fn generate_supertypes(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}

    /// Synthesize new classes or members.
    fn generate_declarations(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}

    /// Fill in or rewrite method bodies after IR lowering.
    fn transform_bodies(&self, _ir: &mut IrFile, _ctx: &PluginContext) {}
}

/// Run this compilation's native backend plugins over frontend-checked common IR. Annotation names
/// and values have already been resolved and folded, so neither reparsed source nor spelling
/// participates. `plugins` is the same selection the frontend ran; with none selected this is a no-op.
/// Returns whether any plugin ran, and so may have generated or completed declarations.
pub fn run_enabled(
    ir: &mut IrFile,
    plugins: &registry::NativePlugins,
    module_name: &str,
    target_type_descriptor: fn(Ty) -> Option<String>,
    classifiers: &dyn crate::types::ClassifierFactSource,
) -> bool {
    if plugins.is_empty() {
        return false;
    }
    let ctx = PluginContext::from_ir(ir).with_target_type_descriptor(target_type_descriptor);
    let uses_serialization = ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            crate::ir::IrExpr::PluginPlaceholder {
                plugin: "serialization",
                ..
            }
        )
    });
    if !uses_serialization
        && ctx
            .classes_with(crate::types::type_name(serialization::SERIALIZABLE_FQ))
            .is_empty()
    {
        return false;
    }
    record_external_value_classes(ir, classifiers);
    let external = external_serializers(ir, classifiers);
    let ctx = ctx
        .with_serial_info_annotations(serial_info_annotations(ir, classifiers))
        .with_external_serializers(external)
        .with_runtime_serializers(runtime_serializers(classifiers))
        .with_external_serializer_objects(declared_serializer_objects(ir, classifiers));
    plugins.host(module_name).run(ir, &ctx);
    true
}

/// The provider-confirmed kind of every serializer class a property's `@Serializable(with = …)`
/// names, on the property itself or on its declared type and type arguments, that this compilation
/// does not declare (a declared one is classified from its own IR class).
fn declared_serializer_objects(
    ir: &IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
) -> std::collections::HashMap<TypeName, bool> {
    let serializable = crate::types::type_name(serialization::SERIALIZABLE_FQ);
    let declared = ir
        .classes
        .iter()
        .map(|class| class.fq_name_id())
        .collect::<std::collections::HashSet<_>>();
    let property_annotations = ir
        .classes
        .iter()
        .flat_map(|class| &class.property_annotations)
        .flat_map(|property| property.annotations.applications())
        .filter(|annotation| annotation.internal == serializable)
        .flat_map(|annotation| &annotation.values)
        .filter_map(|(_, value)| match value {
            crate::ir::AnnoValue::Class(serializer) => serializer.kotlin_class_internal(),
            _ => None,
        });
    let mut type_annotations = Vec::new();
    for spelling in ir.prop_declared_spellings.values() {
        serialization::named_serializers(&spelling.ret, &mut type_annotations);
    }
    property_annotations
        .chain(type_annotations)
        .filter(|serializer| !declared.contains(serializer))
        .filter_map(|serializer| Some((serializer, classifiers.classifier_is_object(serializer)?)))
        .collect()
}

fn serial_info_annotations(
    ir: &IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
) -> std::collections::HashSet<TypeName> {
    let serial_info = crate::types::type_name("kotlinx/serialization/SerialInfo");
    let local_classes = ir
        .classes
        .iter()
        .map(|class| (class.fq_name_id(), class))
        .collect::<std::collections::HashMap<_, _>>();
    ir.classes
        .iter()
        .flat_map(|class| class.applied_annotations.applications())
        .map(|application| application.internal)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .filter(|&annotation| {
            local_classes.get(&annotation).is_some_and(|class| {
                class
                    .applied_annotations
                    .applications()
                    .any(|meta| meta.internal == serial_info)
            }) || classifiers
                .classifier_annotations(annotation)
                .is_some_and(|annotations| {
                    annotations
                        .iter()
                        .any(|meta| meta.annotation == serial_info)
                })
        })
        .collect()
}

/// Publish the value classes this file's fields refer to without declaring into common IR's one
/// value-class fact table. Follow underlying types transitively so a field of `Outer`, whose sole
/// property is sibling-file `Inner`, never depends on an unrelated direct mention of `Inner`.
fn record_external_value_classes(
    ir: &mut IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
) {
    fn collect(ty: Ty, out: &mut Vec<TypeName>) {
        match ty {
            Ty::Obj(name, arguments) => {
                out.push(name);
                for argument in arguments {
                    collect(*argument, out);
                }
            }
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => collect(*inner, out),
            Ty::InProjection(inner) | Ty::OutProjection(inner) | Ty::StarProjection(inner) => {
                collect(*inner, out)
            }
            Ty::Fun(signature) => {
                signature
                    .params
                    .iter()
                    .copied()
                    .for_each(|parameter| collect(parameter, out));
                collect(signature.ret, out);
            }
            Ty::TyParam(_, bound) => collect(*bound, out),
            _ => {}
        }
    }
    let declared = ir
        .classes
        .iter()
        .map(|class| {
            (
                class.fq_name,
                if class.is_value {
                    class.fields.first().map(|field| field.ty)
                } else {
                    None
                },
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut referenced = Vec::new();
    for class in &ir.classes {
        for field in &class.fields {
            collect(field.ty, &mut referenced);
        }
    }
    let mut visited = std::collections::HashSet::new();
    let mut resolved = Vec::new();
    while let Some(name) = referenced.pop() {
        if !visited.insert(name) {
            continue;
        }
        if let Some(underlying) = declared.get(&name) {
            if let Some(underlying) = underlying {
                collect(*underlying, &mut referenced);
            }
            continue;
        }
        let Some(underlying) = classifiers.classifier_value_underlying(name) else {
            continue;
        };
        collect(underlying, &mut referenced);
        resolved.push((name, underlying));
    }
    for (name, underlying) in resolved {
        ir.insert_external_value_class_name(name, underlying);
    }
}

/// Field classifiers this file does not declare, normalized through the single checked provider.
/// Which of the runtime-dependent builtin serializers the ACTIVE artifact carries. Only classes that
/// are not present in every supported kotlinx.serialization version need listing: the long-standing
/// primitives are always there, so probing them would cost lookups for no decision.
fn runtime_serializers(
    classifiers: &dyn crate::types::ClassifierFactSource,
) -> std::collections::HashSet<TypeName> {
    serialization::element_serializer::runtime_dependent_serializers()
        .filter(|&serializer| classifiers.classifier_annotations(serializer).is_some())
        .collect()
}

fn external_serializers(
    ir: &IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
) -> std::collections::HashMap<TypeName, serialization::ExternalSerializer> {
    let serializable = crate::types::type_name(serialization::SERIALIZABLE_FQ);
    external_classifier_candidates(ir)
        .filter_map(|classifier| {
            let annotations = classifiers.classifier_annotations(classifier)?;
            let application = annotations
                .iter()
                .find(|annotation| annotation.annotation == serializable);
            // Annotation values cross this boundary only as checked, named semantic facts. An
            // unnamed positional value is retained source reconstruction, not a stable contract.
            let custom = application.and_then(|annotation| {
                annotation
                    .arguments
                    .iter()
                    .filter(|(name, _)| name == "with")
                    .find_map(|(_, value)| match value {
                        crate::types::AnnotationValue::Class(serializer) => {
                            serializer.kotlin_class_internal()
                        }
                        _ => None,
                    })
            });
            let mut serializer = match (custom, application) {
                (Some(custom), _) if classifiers.classifier_is_object(custom) == Some(true) => {
                    serialization::ExternalSerializer::Singleton(custom)
                }
                // A serializer CLASS is reached through the classifier's generated companion
                // `serializer(…)`, which constructs it; the accessor check below confirms the
                // provider published that exact accessor.
                (Some(custom), _) if classifiers.classifier_is_object(custom) == Some(false) => {
                    let mut declaration = classifiers.classifier_declaration(classifier)?;
                    if declaration.companion.is_none() {
                        declaration.companion = classifiers.serialization_companion(classifier);
                    }
                    serialization::custom_class_serializer(&declaration)?
                }
                (Some(_), _) => return None,
                // What the declaration's compilation generated depends on its kind; a classifier
                // whose facts cannot name it has no entry, and an element of it is underivable.
                (None, Some(_)) => {
                    let mut declaration = classifiers.classifier_declaration(classifier)?;
                    if declaration.companion.is_none() {
                        declaration.companion = classifiers.serialization_companion(classifier);
                    }
                    match serialization::generated_external_serializer(
                        classifier,
                        &declaration,
                        &annotations,
                    ) {
                        Some(serializer) => serializer,
                        None if declaration.kind
                            == crate::types::ClassifierDeclarationKind::Class
                            && !declaration.is_abstract
                            && declaration.own_type_parameter_count == 0 =>
                        {
                            let generated =
                                classifiers.generated_serializer_singleton(classifier)?;
                            serialization::ExternalSerializer::Singleton(generated)
                        }
                        None => return None,
                    }
                }
                (None, None) => return None,
            };
            if let serialization::ExternalSerializer::Object {
                serial_info_unsupported,
                ..
            } = &mut serializer
            {
                let serial_info = crate::types::type_name("kotlinx/serialization/SerialInfo");
                *serial_info_unsupported = annotations.iter().any(|application| {
                    classifiers
                        .classifier_annotations(application.annotation)
                        .map_or(true, |meta| {
                            meta.iter()
                                .any(|annotation| annotation.annotation == serial_info)
                        })
                });
            }
            if let serialization::ExternalSerializer::Companion {
                companion,
                type_parameters,
                ..
            } = &serializer
            {
                if !classifiers.has_serialization_serializer_accessor(*companion, *type_parameters)
                {
                    return None;
                }
            }
            Some((classifier, serializer))
        })
        .collect()
}

fn external_classifier_candidates(ir: &IrFile) -> impl Iterator<Item = TypeName> + '_ {
    let declared: std::collections::HashSet<TypeName> =
        ir.classes.iter().map(|class| class.fq_name_id()).collect();
    let mut candidates: Vec<Ty> = ir
        .classes
        .iter()
        .flat_map(|class| class.fields.iter().map(|field| field.ty))
        .collect();
    // A reified type argument names a classifier the plugin may have to build a serializer for
    // without it appearing as any field's type — `element<JsonElement>("v")` inside a hand-written
    // `KSerializer` is exactly that, and its serializer is the `@Serializable(with = …)` one the
    // dependency declares.
    candidates.extend(
        ir.reified_call_subst
            .values()
            .flat_map(|substitutions| substitutions.iter().map(|(_, ty)| *ty)),
    );
    let mut seen = std::collections::HashSet::new();
    let mut external = Vec::new();
    for expression in &ir.exprs {
        let crate::ir::IrExpr::PluginPlaceholder {
            plugin: "serialization",
            data,
            types,
            ..
        } = expression
        else {
            continue;
        };
        for &classifier in data {
            if seen.insert(classifier) && !declared.contains(&classifier) {
                external.push(classifier);
            }
        }
        // A planned operation's type operands are what it needs a serializer FOR: the frontend
        // plans `element<JsonElement>("v")` with `JsonElement` here, and nowhere else names it.
        candidates.extend(types.iter().copied());
    }
    while let Some(ty) = candidates.pop() {
        // A type argument carries its own element serializer (`List<Inner>` needs `Inner`'s).
        candidates.extend(ty.non_null().type_args().iter().copied());
        let Some(classifier) = ty.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(classifier) || declared.contains(&classifier) {
            continue;
        }
        external.push(classifier);
    }
    external.into_iter()
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

    pub fn publish_frontend_generated_classifiers(
        &self,
        ctx: &FrontendClassContext<'_>,
        classifiers: &mut Vec<crate::types::GeneratedClassifierFact>,
    ) {
        for plugin in &self.plugins {
            plugin.publish_frontend_generated_classifiers(ctx, classifiers);
        }
    }

    /// The first plugin selection of a named class's constructor, with every plugin's diagnostics.
    pub fn select_named_class_constructor(
        &self,
        ctx: &FrontendNamedClassContext<'_>,
    ) -> (
        Option<FrontendNamedClassConstruction>,
        Vec<FrontendPluginDiagnostic>,
    ) {
        let mut diagnostics = Vec::new();
        let mut selected = None;
        for plugin in &self.plugins {
            let selection = plugin.select_named_class_constructor(ctx, &mut diagnostics);
            selected = selected.or(selection);
        }
        (selected, diagnostics)
    }

    pub fn check_frontend_class(
        &self,
        ctx: &FrontendClassCheckContext<'_>,
    ) -> Vec<FrontendPluginDiagnostic> {
        let mut diagnostics = Vec::new();
        for plugin in &self.plugins {
            plugin.check_frontend_class(ctx, &mut diagnostics);
        }
        diagnostics
    }

    pub fn plan_frontend_expressions(
        &self,
        ctx: &FrontendExpressionContext<'_>,
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

    #[derive(Default)]
    struct FakeClassifierFacts {
        annotations: std::collections::HashMap<
            crate::types::TypeName,
            Vec<crate::types::ResolvedAnnotation>,
        >,
        value_underlyings: std::collections::HashMap<crate::types::TypeName, Ty>,
        declarations: std::collections::HashMap<
            crate::types::TypeName,
            crate::types::ClassifierDeclarationFacts,
        >,
        objects: std::collections::HashSet<crate::types::TypeName>,
    }

    impl crate::types::ClassifierFactSource for FakeClassifierFacts {
        fn classifier_annotations(
            &self,
            classifier: crate::types::TypeName,
        ) -> Option<Vec<crate::types::ResolvedAnnotation>> {
            self.annotations.get(&classifier).cloned()
        }

        fn classifier_value_underlying(&self, classifier: TypeName) -> Option<Ty> {
            self.value_underlyings.get(&classifier).copied()
        }

        fn classifier_declaration(
            &self,
            classifier: TypeName,
        ) -> Option<crate::types::ClassifierDeclarationFacts> {
            self.declarations.get(&classifier).cloned()
        }

        fn classifier_is_object(&self, classifier: TypeName) -> Option<bool> {
            self.objects.contains(&classifier).then_some(true)
        }
    }

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
    fn serial_info_roles_come_from_resolved_classifier_facts() {
        let stamp = crate::types::type_name("fixtures/SchemaStamp");
        let serial_info = crate::types::type_name("kotlinx/serialization/SerialInfo");
        let mut object = synthetic_class("sample/StampedObject");
        object.applied_annotations =
            crate::ir::DeclarationAnnotations::new(vec![crate::ir::RetainedAnnotation {
                retention: crate::types::AnnotationRetention::Runtime,
                annotation: crate::ir::AppliedAnnotation {
                    internal: stamp,
                    values: vec![],
                },
                facts: Default::default(),
            }]);
        let mut ir = IrFile::default();
        ir.add_class(object);
        let facts = FakeClassifierFacts {
            annotations: std::collections::HashMap::from([(
                stamp,
                vec![crate::types::ResolvedAnnotation {
                    annotation: serial_info,
                    arguments: vec![],
                    facts: Default::default(),
                }],
            )]),
            ..FakeClassifierFacts::default()
        };

        assert_eq!(
            serial_info_annotations(&ir, &facts),
            std::collections::HashSet::from([stamp])
        );
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
    fn external_value_class_facts_are_exact_and_transitively_shared_with_ir() {
        let local = crate::types::type_name("fixture/LocalId");
        let left = crate::types::type_name("left/Id");
        let right = crate::types::type_name("right/Id");
        let unrelated = crate::types::type_name("other/Id");
        let mut ir = IrFile::default();
        let mut local_class = synthetic_class("fixture/LocalId");
        local_class.is_value = true;
        local_class.fields.push(crate::ir::IrField::new(
            "value".to_string(),
            Ty::obj_name(left),
        ));
        ir.add_class(local_class);
        let mut holder = synthetic_class("fixture/Holder");
        holder.fields.push(crate::ir::IrField::new(
            "id".to_string(),
            Ty::obj_name(local),
        ));
        ir.add_class(holder);
        let facts = FakeClassifierFacts {
            value_underlyings: std::collections::HashMap::from([
                (left, Ty::obj_name(right)),
                (right, Ty::Int),
                (unrelated, Ty::String),
            ]),
            ..FakeClassifierFacts::default()
        };

        record_external_value_classes(&mut ir, &facts);

        assert_eq!(
            ir.external_value_class_name(left),
            Some(&Ty::obj_name(right))
        );
        assert_eq!(ir.external_value_class_name(right), Some(&Ty::Int));
        assert_eq!(ir.external_value_class_name(local), None);
        assert_eq!(ir.external_value_class_name(unrelated), None);
    }

    #[test]
    fn checked_ir_context_needs_no_source_for_class_or_file_annotations() {
        let retained = |name: &str, values| crate::ir::RetainedAnnotation {
            retention: crate::types::AnnotationRetention::Runtime,
            annotation: crate::ir::AppliedAnnotation {
                internal: crate::types::type_name(name),
                values,
            },
            facts: Default::default(),
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
                crate::ir::AnnoValue::Array(vec![crate::ir::AnnoValue::Class(Ty::obj(
                    "demo/External",
                ))]),
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
    /// A classifier named ONLY by a reified type argument is still offered to the external-serializer
    /// map.
    ///
    /// `element<JsonElement>("v")` inside a hand-written `KSerializer` names a dependency type that
    /// is no field's type and appears in no plugin placeholder, so it reached neither candidate
    /// source and the plugin could not build its serializer — which left the reified `element` call
    /// on the splice path, and that refusal bails the whole file.
    #[test]
    fn a_reified_type_argument_is_an_external_classifier_candidate() {
        let mut ir = IrFile::default();
        ir.reified_call_subst.insert(
            7,
            vec![(
                "T".to_owned(),
                Ty::obj("kotlinx/serialization/json/JsonElement"),
            )],
        );
        let candidates: Vec<crate::types::TypeName> = external_classifier_candidates(&ir).collect();
        assert!(
            candidates.contains(&crate::types::type_name(
                "kotlinx/serialization/json/JsonElement"
            )),
            "a reified type argument must be a candidate: {candidates:?}"
        );
    }

    /// The frontend plans `element<JsonElement>("v")` as a plugin operation whose TYPE operand is
    /// the element type; no reified substitution and no name payload carries it. It, and the type
    /// arguments inside it, must still reach the classifier-fact lookup.
    #[test]
    fn a_planned_operation_type_is_an_external_classifier_candidate() {
        let mut ir = IrFile::default();
        ir.add_expr(crate::ir::IrExpr::PluginPlaceholder {
            plugin: "serialization",
            kind: "descriptorElementDefaultBoth",
            exprs: Vec::new(),
            data: Vec::new(),
            types: vec![Ty::obj_args(
                "kotlin/collections/List",
                &[Ty::obj("kotlinx/serialization/json/JsonElement")],
            )],
        });
        let candidates: Vec<crate::types::TypeName> = external_classifier_candidates(&ir).collect();
        assert!(
            candidates.contains(&crate::types::type_name(
                "kotlinx/serialization/json/JsonElement"
            )),
            "a planned operation's type operand must be a candidate: {candidates:?}"
        );
    }

    /// The production provider boundary, not a classifier spelling, decides which serializer a
    /// reified-only dependency type uses. Nest the payload in another user type so this also proves
    /// candidate traversal reaches type arguments rather than consulting only the outer classifier.
    #[test]
    fn reified_only_classifier_uses_its_metadata_named_serializer() {
        let payload = crate::types::type_name("fixtures/ExternalPayload");
        let serializer = crate::types::type_name("fixtures/PayloadCodec");
        let mut ir = IrFile::default();
        ir.reified_call_subst.insert(
            11,
            vec![(
                "Payload".to_owned(),
                Ty::obj_args("fixtures/Envelope", &[Ty::obj_name(payload)]),
            )],
        );
        let annotations = FakeClassifierFacts {
            annotations: std::collections::HashMap::from([(
                payload,
                vec![crate::types::ResolvedAnnotation {
                    annotation: crate::types::type_name(serialization::SERIALIZABLE_FQ),
                    arguments: vec![(
                        "with".to_owned(),
                        crate::types::AnnotationValue::Class(Ty::obj_name(serializer)),
                    )],
                    facts: Default::default(),
                }],
            )]),
            objects: std::collections::HashSet::from([serializer]),
            ..FakeClassifierFacts::default()
        };

        let resolved = external_serializers(&ir, &annotations);

        assert_eq!(
            resolved,
            std::collections::HashMap::from([(
                payload,
                serialization::ExternalSerializer::Singleton(serializer)
            )])
        );
        let context = PluginContext::default().with_external_serializers(resolved);
        assert_eq!(
            context.external_serializer(payload),
            Some(&serialization::ExternalSerializer::Singleton(serializer))
        );
        assert_eq!(
            context.external_serializer(crate::types::type_name("fixtures/Envelope")),
            None
        );
    }

    #[test]
    fn external_object_with_serial_info_is_marked_unsupported() {
        let object = crate::types::type_name("fixtures/Singleton");
        let payload = crate::types::type_name("fixtures/Payload");
        let mut ir = IrFile::default();
        let mut holder = synthetic_class("fixtures/Holder");
        holder.fields.push(crate::ir::IrField::new(
            "value".to_owned(),
            Ty::obj_name(object),
        ));
        ir.add_class(holder);
        let facts = FakeClassifierFacts {
            annotations: std::collections::HashMap::from([
                (
                    object,
                    vec![
                        crate::types::ResolvedAnnotation {
                            annotation: crate::types::type_name(serialization::SERIALIZABLE_FQ),
                            arguments: Vec::new(),
                            facts: Default::default(),
                        },
                        crate::types::ResolvedAnnotation {
                            annotation: payload,
                            arguments: vec![(
                                "value".to_owned(),
                                crate::types::AnnotationValue::string("kept"),
                            )],
                            facts: Default::default(),
                        },
                    ],
                ),
                (
                    payload,
                    vec![crate::types::ResolvedAnnotation {
                        annotation: crate::types::type_name("kotlinx/serialization/SerialInfo"),
                        arguments: Vec::new(),
                        facts: Default::default(),
                    }],
                ),
            ]),
            declarations: std::collections::HashMap::from([(
                object,
                crate::types::ClassifierDeclarationFacts {
                    kind: crate::types::ClassifierDeclarationKind::Object,
                    is_abstract: false,
                    own_type_parameter_count: 0,
                    companion: None,
                    qualified_name: Some(Box::from("fixtures.Singleton")),
                    source: false,
                },
            )]),
            ..FakeClassifierFacts::default()
        };

        assert_eq!(
            external_serializers(&ir, &facts).get(&object),
            Some(&serialization::ExternalSerializer::Object {
                serial_name: crate::kt_string::KtString::from("fixtures.Singleton"),
                serial_info_unsupported: true,
            })
        );
    }

    #[test]
    fn an_external_object_with_unavailable_annotation_facts_fails_closed() {
        let object = crate::types::type_name("fixtures/Singleton");
        let unknown = crate::types::type_name("fixtures/UnknownMarker");
        let serializable = crate::types::type_name(serialization::SERIALIZABLE_FQ);
        let mut ir = IrFile::default();
        let mut holder = synthetic_class("fixtures/Holder");
        holder.fields.push(crate::ir::IrField::new(
            "value".to_owned(),
            Ty::obj_name(object),
        ));
        ir.add_class(holder);
        let facts = FakeClassifierFacts {
            annotations: std::collections::HashMap::from([
                (
                    object,
                    vec![
                        crate::types::ResolvedAnnotation {
                            annotation: serializable,
                            arguments: Vec::new(),
                            facts: Default::default(),
                        },
                        crate::types::ResolvedAnnotation {
                            annotation: unknown,
                            arguments: Vec::new(),
                            facts: Default::default(),
                        },
                    ],
                ),
                (serializable, Vec::new()),
            ]),
            declarations: std::collections::HashMap::from([(
                object,
                crate::types::ClassifierDeclarationFacts {
                    kind: crate::types::ClassifierDeclarationKind::Object,
                    is_abstract: false,
                    own_type_parameter_count: 0,
                    companion: None,
                    qualified_name: Some(Box::from("fixtures.Singleton")),
                    source: false,
                },
            )]),
            ..FakeClassifierFacts::default()
        };

        assert_eq!(
            external_serializers(&ir, &facts).get(&object),
            Some(&serialization::ExternalSerializer::Object {
                serial_name: crate::kt_string::KtString::from("fixtures.Singleton"),
                serial_info_unsupported: true,
            })
        );
    }

    /// A type the module DECLARES is not external, however it is named.
    #[test]
    fn a_declared_classifier_is_not_an_external_candidate() {
        let mut ir = IrFile::default();
        ir.add_class(synthetic_class("demo/Own"));
        ir.reified_call_subst
            .insert(7, vec![("T".to_owned(), Ty::obj("demo/Own"))]);
        let candidates: Vec<crate::types::TypeName> = external_classifier_candidates(&ir).collect();
        assert!(
            !candidates.contains(&crate::types::type_name("demo/Own")),
            "a declared classifier must not be external: {candidates:?}"
        );
    }
}

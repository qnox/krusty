//! The stability kotlinc's `ClassStabilityTransformer` records on every public or internal class of
//! a Compose module, composable or not:
//!
//! ```text
//! @StabilityInferred(parameters = 1)
//! class Point(val x: Int, val y: Int) {
//!     @JvmField public static final int $stable = 0;
//! }
//! ```
//!
//! `$stable` holds the stability bits of the class's instances (`0` stable, `8` unstable), which a
//! composable that takes one compares instead of the instance itself; `@StabilityInferred`, visible
//! in Kotlin metadata too, says whether the result depends on nothing (`1`) or is unknown (`0`). A
//! class marked `@Stable` or `@Immutable` gets the stable field and no annotation.
//!
//! The inference is kotlinc's `StabilityInferencer` over the classes of the file: a class is stable
//! when it is final and every backing field is a `val` of a stable type, a `var` makes it unstable,
//! and an interface or a class that is not final is of unknown stability. Unknown parts drop out
//! once there are several, so two interface-typed fields leave a class stable, as kotlinc infers.
//! What the inference cannot decide here is declined at the class: a type parameter, a value class,
//! and a class of another file or a Compose-compiled library, whose `$stable` kotlinc reads at run
//! time.

use crate::ir::{
    AnnoRetention, AnnoValue, AppliedAnnotation, DeclarationAnnotations, FieldAnnotations,
    IrAccessorLines, IrConst, IrExpr, IrFile, IrStatic, IrStaticAccessors, RetainedAnnotation,
};
use crate::plugins::runtime_declarations::{PluginClassifier, PluginRuntime};
use crate::plugins::{PluginRejection, PluginSourceView};
use crate::types::{type_name, ClassifierDeclarationKind, Ty, TyKind, TypeName, Visibility};

const STABILITY_INFERRED: &str = "androidx/compose/runtime/internal/StabilityInferred";
const STABLE_MARKER: &str = "androidx/compose/runtime/StableMarker";
const JVM_FIELD: &str = "kotlin/jvm/JvmField";
/// The stability bits of slot 0: `StabilityBits.STABLE` and `StabilityBits.UNSTABLE`.
const STABLE: i32 = 0b000;
const UNSTABLE: i32 = 0b100 << 1;

/// The classes kotlinc's `KnownStableConstructs` treats as stable, each with the type parameters
/// whose arguments the stability also depends on.
const KNOWN_STABLE_TYPES: &[(&str, u32)] = &[
    ("kotlin/Pair", 0b11),
    ("kotlin/Triple", 0b111),
    ("java/util/Comparator", 0b1),
    ("kotlin/Result", 0b1),
    ("kotlin/ranges/ClosedRange", 0b1),
    ("kotlin/ranges/ClosedFloatingPointRange", 0b1),
    ("com/google/common/collect/ImmutableList", 0b1),
    ("com/google/common/collect/ImmutableEnumMap", 0b11),
    ("com/google/common/collect/ImmutableMap", 0b11),
    ("com/google/common/collect/ImmutableEnumSet", 0b1),
    ("com/google/common/collect/ImmutableSet", 0b1),
    ("kotlinx/collections/immutable/ImmutableCollection", 0b1),
    ("kotlinx/collections/immutable/ImmutableList", 0b1),
    ("kotlinx/collections/immutable/ImmutableSet", 0b1),
    ("kotlinx/collections/immutable/ImmutableMap", 0b11),
    ("kotlinx/collections/immutable/PersistentCollection", 0b1),
    ("kotlinx/collections/immutable/PersistentList", 0b1),
    ("kotlinx/collections/immutable/PersistentSet", 0b1),
    ("kotlinx/collections/immutable/PersistentMap", 0b11),
    ("dagger/Lazy", 0b1),
    ("kotlin/coroutines/EmptyCoroutineContext", 0),
    ("java/math/BigInteger", 0),
    ("java/math/BigDecimal", 0),
    ("java/util/Locale", 0),
];

/// The annotations kotlinc's `KnownStableConstructs` treats as stable markers besides those the
/// runtime's `@StableMarker` annotates.
const KNOWN_STABLE_MARKERS: &[&str] = &["com/google/errorprone/annotations/Immutable"];

/// The superclasses of a generated protobuf message, which kotlinc treats as stable.
const PROTOBUF_MESSAGES: &[&str] = &[
    "com/google/protobuf/GeneratedMessageLite",
    "com/google/protobuf/GeneratedMessage",
];

/// kotlinc's `Stability`, without the run-time and type-parameter parts the pass declines.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stability {
    Certain(bool),
    Unknown,
    Combined(Vec<Stability>),
}

impl Stability {
    const STABLE: Self = Self::Certain(true);
    const UNSTABLE: Self = Self::Certain(false);

    /// kotlinc's `Stability.plus`: a stable part adds nothing and an unstable one decides.
    fn plus(self, other: Self) -> Self {
        match (self, other) {
            (this, Self::Certain(true)) => this,
            (_, other @ Self::Certain(false)) => other,
            (Self::Certain(true), other) => other,
            (this @ Self::Certain(false), _) => this,
            (this, other) => Self::Combined(vec![this, other]),
        }
    }

    /// kotlinc's `normalize`: an unstable part decides, and unknown parts of a combination drop out.
    fn normalize(self) -> Self {
        let Self::Combined(elements) = self else {
            return self;
        };
        let mut pending = elements;
        while let Some(element) = pending.pop() {
            match element {
                Self::Combined(nested) => pending.extend(nested),
                Self::Certain(false) => return Self::UNSTABLE,
                Self::Certain(true) | Self::Unknown => {}
            }
        }
        Self::Combined(Vec::new())
    }

    fn known_stable(&self) -> bool {
        match self {
            Self::Certain(stable) => *stable,
            Self::Unknown => false,
            Self::Combined(elements) => elements.iter().all(Self::known_stable),
        }
    }
}

/// Give every public or internal class of the file its `$stable` field and `@StabilityInferred`,
/// or decline the class when its stability is not decided in the file.
pub(super) fn declare(
    ir: &mut IrFile,
    source: &PluginSourceView<'_>,
    runtime: &dyn PluginRuntime,
    rejections: &mut Vec<PluginRejection>,
) {
    let mut inference = Inference {
        ir,
        runtime,
        analyzed: Default::default(),
    };
    let mut markers = Vec::new();
    for (index, class) in ir.classes.iter().enumerate() {
        if !records_stability(ir, class) {
            continue;
        }
        let marker = match inference.marker(class) {
            Ok(marker) => marker,
            Err(what) => {
                rejections.push(super::source_facts::class_rejection(source, class, what));
                continue;
            }
        };
        if marker {
            markers.push((index, STABLE, None));
            continue;
        }
        if !class.type_params.is_empty() {
            rejections.push(super::source_facts::class_rejection(
                source,
                class,
                "a generic class, whose stability depends on its type arguments",
            ));
            continue;
        }
        match inference.of_class(class.fq_name, &mut Vec::new()) {
            Ok(stability) => {
                let stability = stability.normalize();
                let stable = stability.known_stable();
                let bits = if stable { STABLE } else { UNSTABLE };
                markers.push((index, bits, Some(i32::from(stable))));
            }
            Err(what) => rejections.push(super::source_facts::class_rejection(source, class, what)),
        }
    }
    for (index, bits, parameters) in markers {
        mark(ir, index, bits, parameters);
    }
}

/// Whether kotlinc records the stability of `class`: a public or internal class declared in
/// source that is not an enum, an interface, an annotation, an anonymous, local, inner or companion
/// object or a value class.
fn records_stability(ir: &IrFile, class: &crate::ir::IrClass) -> bool {
    let visibility = ir
        .class_visibilities
        .get(&class.fq_name)
        .copied()
        .unwrap_or(Visibility::Public);
    class.is_source_declared
        && matches!(visibility, Visibility::Public | Visibility::Internal)
        && !(class.is_enum
            || class.is_enum_entry
            || class.is_interface
            || class.is_annotation
            || class.is_anonymous_object
            || class.is_local_class
            || class.is_inner_class
            || class.is_companion
            || class.is_value)
}

/// Add `@JvmField public static final int $stable = bits` and, unless the class carries a stable
/// marker, `@StabilityInferred(parameters)`.
fn mark(ir: &mut IrFile, index: usize, bits: i32, parameters: Option<i32>) {
    let init = ir.add_expr(IrExpr::Const(IrConst::Int(bits)));
    ir.mark_positionless(init);
    let class = &mut ir.classes[index];
    let owner = class.fq_name;
    let mut field_annotations = DeclarationAnnotations::default();
    field_annotations.push(binary(JVM_FIELD, Vec::new()));
    class.field_annotations.push(FieldAnnotations {
        field: "$stable".to_string(),
        annotations: field_annotations,
    });
    if let Some(parameters) = parameters {
        class.applied_annotations.push_metadata_visible(binary(
            STABILITY_INFERRED,
            vec![(
                "parameters".to_string(),
                AnnoValue::Const(IrConst::Int(parameters)),
            )],
        ));
    }
    ir.statics.push(IrStatic {
        name: "$stable".to_string(),
        ty: Ty::Int,
        init: Some(init),
        is_var: false,
        is_const: false,
        is_lateinit: false,
        owner: Some(owner),
        visibility: Visibility::Public,
        getter_jvm_name: None,
        setter_jvm_name: None,
        erased_declared_ty: None,
        accessors: IrStaticAccessors::ABSENT,
        line: 0,
        accessor_lines: IrAccessorLines::default(),
        source_order: u32::MAX,
        property_delegate: false,
    });
}

fn binary(annotation: &str, values: Vec<(String, AnnoValue)>) -> RetainedAnnotation {
    RetainedAnnotation {
        retention: AnnoRetention::Binary,
        annotation: AppliedAnnotation {
            internal: type_name(annotation),
            values,
        },
        facts: Default::default(),
    }
}

/// kotlinc's `StabilityInferencer` over one file, whose classes it infers and caches.
struct Inference<'a> {
    ir: &'a IrFile,
    runtime: &'a dyn PluginRuntime,
    analyzed: crate::fx_hash::HashMap<TypeName, Stability>,
}

/// A classifier the inference reads: one of the file's classes, or one the compilation declares.
enum Classifier<'a> {
    File(&'a crate::ir::IrClass),
    Declared(PluginClassifier),
}

impl Inference<'_> {
    fn classifier(&self, name: TypeName) -> Option<Classifier<'_>> {
        match self.ir.classes.iter().find(|class| class.fq_name == name) {
            Some(class) => Some(Classifier::File(class)),
            None => self.runtime.classifier(name).map(Classifier::Declared),
        }
    }

    /// Whether the class applies a stable marker itself (kotlinc's `hasStableMarker`).
    fn marker(&self, class: &crate::ir::IrClass) -> Result<bool, &'static str> {
        for annotation in class.applied_annotations.applications() {
            if self.is_stable_marker(annotation.internal)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether `annotation` is a stable marker: annotated with the runtime's `@StableMarker`, or a
    /// marker kotlinc knows.
    fn is_stable_marker(&self, annotation: TypeName) -> Result<bool, &'static str> {
        if KNOWN_STABLE_MARKERS
            .iter()
            .any(|&marker| annotation == type_name(marker))
        {
            return Ok(true);
        }
        let stable_marker = type_name(STABLE_MARKER);
        Ok(match self.classifier(annotation) {
            Some(Classifier::File(class)) => class
                .applied_annotations
                .applications()
                .any(|applied| applied.internal == stable_marker),
            Some(Classifier::Declared(declared)) => declared
                .annotations
                .iter()
                .any(|applied| applied.annotation == stable_marker),
            None => return Err("a class annotation the compilation does not declare"),
        })
    }

    /// Whether `name` or one of its supertypes other than `Any` applies a stable marker (kotlinc's
    /// `hasStableMarkedDescendant`).
    fn has_stable_marked_descendant(&self, name: TypeName) -> Result<bool, &'static str> {
        let (annotations, supertypes): (Vec<TypeName>, Vec<TypeName>) = match self.classifier(name)
        {
            Some(Classifier::File(class)) => (
                class
                    .applied_annotations
                    .applications()
                    .map(|annotation| annotation.internal)
                    .collect(),
                std::iter::once(class.superclass)
                    .chain(class.interfaces.iter())
                    .collect(),
            ),
            Some(Classifier::Declared(declared)) => (
                declared
                    .annotations
                    .iter()
                    .map(|annotation| annotation.annotation)
                    .collect(),
                declared.supertypes,
            ),
            None => return Err("a class whose supertypes the compilation does not declare"),
        };
        for annotation in annotations {
            if self.is_stable_marker(annotation)? {
                return Ok(true);
            }
        }
        for supertype in supertypes {
            if !is_any(supertype) && self.has_stable_marked_descendant(supertype)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// kotlinc's `stabilityOf(IrClass)` for a class without type arguments.
    fn of_class(
        &mut self,
        name: TypeName,
        analyzing: &mut Vec<TypeName>,
    ) -> Result<Stability, &'static str> {
        if analyzing.contains(&name) {
            return Ok(Stability::UNSTABLE);
        }
        if let Some(stability) = self.analyzed.get(&name) {
            return Ok(stability.clone());
        }
        if self.has_stable_marked_descendant(name)? {
            return Ok(Stability::STABLE);
        }
        let ir = self.ir;
        let Some(class) = ir.classes.iter().find(|class| class.fq_name == name) else {
            return self.of_declared(name, &[], analyzing);
        };
        if class.is_enum || class.is_enum_entry || class.is_object {
            return Ok(Stability::STABLE);
        }
        if !(class.is_open || class.is_abstract || class.is_sealed)
            && is_protobuf_message(class.superclass)
        {
            return Ok(Stability::STABLE);
        }
        if class.is_interface {
            return Ok(Stability::Unknown);
        }
        if !class.type_params.is_empty() {
            return Err("a class whose stability depends on a generic class");
        }
        analyzing.push(name);
        let stability = self.of_members(class, analyzing);
        analyzing.pop();
        let stability = stability?;
        self.analyzed.insert(name, stability.clone());
        Ok(stability)
    }

    /// A file class's own stability: final or unknown, plus its backing fields and superclass.
    fn of_members(
        &mut self,
        class: &crate::ir::IrClass,
        analyzing: &mut Vec<TypeName>,
    ) -> Result<Stability, &'static str> {
        let mut stability = if class.is_open || class.is_abstract || class.is_sealed {
            Stability::Unknown
        } else {
            Stability::STABLE
        };
        let mut property_fields = crate::fx_hash::HashSet::default();
        for property in &class.properties {
            let Some(field) = property.delegate_field.or(property.backing_field) else {
                continue;
            };
            property_fields.insert(field);
            if property.is_var && property.delegate_field.is_none() {
                return Ok(Stability::UNSTABLE);
            }
        }
        for field in &class.fields {
            stability = stability.plus(self.of_type(field.ty, analyzing)?);
        }
        if !is_any(class.superclass) {
            let superclass = self.of_class(class.superclass, analyzing)?;
            if superclass != Stability::Unknown {
                stability = stability.plus(superclass);
            }
        }
        Ok(stability)
    }

    /// kotlinc's `stabilityOf(IrClass)` for a classifier no class of the file declares.
    fn of_declared(
        &mut self,
        name: TypeName,
        arguments: &[Ty],
        analyzing: &mut Vec<TypeName>,
    ) -> Result<Stability, &'static str> {
        let Some(declared) = self.runtime.classifier(name) else {
            return Err("a class whose field types the compilation does not declare");
        };
        if matches!(
            declared.kind,
            ClassifierDeclarationKind::Enum | ClassifierDeclarationKind::Object
        ) {
            return Ok(Stability::STABLE);
        }
        if self.declared_protobuf_message(&declared) {
            return Ok(Stability::STABLE);
        }
        if let Some(&(_, mask)) = KNOWN_STABLE_TYPES
            .iter()
            .find(|&&(known, _)| name == type_name(known))
        {
            return self.with_type_arguments(mask, arguments, analyzing);
        }
        if !declared.is_kotlin {
            return Ok(Stability::UNSTABLE);
        }
        if declared.kind == ClassifierDeclarationKind::Interface {
            return Ok(Stability::Unknown);
        }
        let stability_inferred = type_name(STABILITY_INFERRED);
        if declared.in_module
            || declared
                .annotations
                .iter()
                .any(|annotation| annotation.annotation == stability_inferred)
        {
            return Err(
                "a class whose stability depends on a class of another file or a Compose library",
            );
        }
        Ok(Stability::UNSTABLE)
    }

    /// kotlinc's `applyTypeParameterMask` over a stable classifier's type arguments.
    fn with_type_arguments(
        &mut self,
        mask: u32,
        arguments: &[Ty],
        analyzing: &mut Vec<TypeName>,
    ) -> Result<Stability, &'static str> {
        if mask == 0 || arguments.is_empty() {
            return Ok(Stability::STABLE);
        }
        let mut elements = Vec::new();
        for (index, &argument) in arguments.iter().enumerate().take(32) {
            if mask & (1 << index) != 0 {
                elements.push(self.of_argument(argument, analyzing)?);
            }
        }
        Ok(Stability::STABLE.plus(Stability::Combined(elements)))
    }

    fn of_argument(
        &mut self,
        argument: Ty,
        analyzing: &mut Vec<TypeName>,
    ) -> Result<Stability, &'static str> {
        match argument.kind() {
            TyKind::StarProjection(_) => Ok(Stability::UNSTABLE),
            TyKind::InProjection(inner) | TyKind::OutProjection(inner) => {
                self.of_type(inner, analyzing)
            }
            _ => self.of_type(argument, analyzing),
        }
    }

    /// kotlinc's `stabilityOf(IrType)`.
    fn of_type(
        &mut self,
        ty: Ty,
        analyzing: &mut Vec<TypeName>,
    ) -> Result<Stability, &'static str> {
        match ty.kind() {
            TyKind::Unit
            | TyKind::BOOLEAN
            | TyKind::BYTE
            | TyKind::SHORT
            | TyKind::INT
            | TyKind::LONG
            | TyKind::CHAR
            | TyKind::FLOAT
            | TyKind::DOUBLE
            | TyKind::STRING => Ok(Stability::STABLE),
            // A suspend function type is a `SuspendFunctionN` interface to kotlinc.
            TyKind::Fun(signature) if signature.suspend => Ok(Stability::Unknown),
            TyKind::Fun(_) => Ok(Stability::STABLE),
            TyKind::Obj(name, _) if is_kfunction(name) => Ok(Stability::STABLE),
            TyKind::Nullable(inner) | TyKind::PlatformNullable(inner) => {
                self.of_type(inner, analyzing)
            }
            TyKind::Obj(name, arguments) => {
                if self.is_value_class(name) {
                    return Err("a class with a value-class field");
                }
                if arguments.is_empty() {
                    self.of_class(name, analyzing)
                } else if self.ir.classes.iter().any(|class| class.fq_name == name) {
                    Err("a class whose stability depends on a generic class")
                } else {
                    let arguments = arguments.iter().copied().collect::<Vec<_>>();
                    if self.has_stable_marked_descendant(name)? {
                        return Ok(Stability::STABLE);
                    }
                    self.of_declared(name, &arguments, analyzing)
                }
            }
            TyKind::Nothing => Ok(Stability::UNSTABLE),
            _ => Err("a class with a field of a type kotlinc's stability inference reads apart"),
        }
    }

    fn is_value_class(&self, name: TypeName) -> bool {
        match self.classifier(name) {
            Some(Classifier::File(class)) => class.is_value,
            Some(Classifier::Declared(declared)) => declared.is_value,
            None => false,
        }
    }

    /// A Java class whose last superclass that is not an interface is a protobuf message.
    fn declared_protobuf_message(&self, declared: &PluginClassifier) -> bool {
        declared.is_final
            && declared
                .supertypes
                .iter()
                .rev()
                .find(|&&supertype| {
                    !matches!(
                        self.runtime.classifier(supertype),
                        Some(PluginClassifier {
                            kind: ClassifierDeclarationKind::Interface,
                            ..
                        })
                    )
                })
                .is_some_and(|&superclass| is_protobuf_message(superclass))
    }
}

fn is_protobuf_message(class: TypeName) -> bool {
    PROTOBUF_MESSAGES
        .iter()
        .any(|&message| class == type_name(message))
}

/// A `KFunction` type, which kotlinc's `isFunctionOrKFunction` counts as a function type.
fn is_kfunction(class: TypeName) -> bool {
    class == type_name("kotlin/reflect/KFunction")
        || (0..=22).any(|arity| class == type_name(&format!("kotlin/reflect/KFunction{arity}")))
}

fn is_any(class: TypeName) -> bool {
    class == type_name("kotlin/Any") || class == type_name("java/lang/Object")
}

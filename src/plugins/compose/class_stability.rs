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
//! generic class's annotation has a bit for each type parameter the stability depends on and the
//! bit past them when it depends on nothing. A class marked `@Stable` or `@Immutable` gets the
//! stable field and no annotation.
//!
//! The inference is kotlinc's `StabilityInferencer` over the classes of the file: a class is stable
//! when it is final and every backing field is a `val` of a stable type, a `var` makes it unstable,
//! and an interface or a class that is not final is of unknown stability. Unknown parts drop out
//! once there are several, so two interface-typed fields leave a class stable, as kotlinc infers.
//! A class of another file or a Compose-compiled library contributes its `$stable`, read at run
//! time, and a value class the stability of its underlying type. What the inference cannot decide
//! here is declined at the class: a field of a generic class or generic value class, whose type
//! arguments kotlinc substitutes.

use crate::ir::{
    AnnoRetention, AnnoValue, AppliedAnnotation, DeclarationAnnotations, ExprId, FieldAnnotations,
    IrAccessorLines, IrBinOp, IrConst, IrExpr, IrFile, IrStatic, IrStaticAccessors,
    RetainedAnnotation,
};
use crate::plugins::runtime_declarations::{PluginClassifier, PluginRuntime};
use crate::plugins::{PluginRejection, PluginSourceView};
use crate::types::{
    type_name, ClassifierDeclarationKind, Ty, TyKind, TyParamName, TypeName, Visibility,
};

const STABILITY_INFERRED: &str = "androidx/compose/runtime/internal/StabilityInferred";
const STABLE_MARKER: &str = "androidx/compose/runtime/StableMarker";
const JVM_FIELD: &str = "kotlin/jvm/JvmField";
const K_FUNCTION: &str = "kotlin/reflect/KFunction";
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

/// kotlinc's `Stability`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stability {
    Certain(bool),
    Unknown,
    /// The class's `$stable`, read when the class initializes: a class of another file, or a
    /// dependency class compiled with Compose.
    Runtime(TypeName),
    /// The stability of the type argument of the class's own type parameter at this index.
    Parameter(u32),
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

    /// kotlinc's `normalize`: an unstable part decides, unknown parts of a combination drop out, and
    /// the run-time parts remain in the order kotlinc's work stack pops them.
    fn normalize(self) -> Self {
        let Self::Combined(elements) = self else {
            return self;
        };
        let mut pending = elements;
        let mut parts = Vec::new();
        while let Some(element) = pending.pop() {
            match element {
                Self::Combined(nested) => pending.extend(nested),
                Self::Certain(false) => return Self::UNSTABLE,
                runtime @ Self::Runtime(_) => parts.push(runtime),
                parameter @ Self::Parameter(_) => {
                    if !parts.contains(&parameter) {
                        parts.push(parameter);
                    }
                }
                Self::Certain(true) | Self::Unknown => {}
            }
        }
        Self::Combined(parts)
    }

    /// kotlinc's `irStableExpression`: the `$stable` fields the class's own `$stable` ors together,
    /// none for a stable class, or nothing when the stability is not known. A type parameter reads
    /// as stable, and the JVM backend drops an `or` with that constant `0`.
    fn runtime_reads(&self) -> Option<Vec<TypeName>> {
        match self {
            Self::Certain(true) | Self::Parameter(_) => Some(Vec::new()),
            Self::Certain(false) | Self::Unknown => None,
            Self::Runtime(class) => Some(vec![*class]),
            Self::Combined(elements) => elements
                .iter()
                .map(Self::runtime_reads)
                .collect::<Option<Vec<_>>>()
                .map(|reads| reads.concat()),
        }
    }

    fn known_stable(&self) -> bool {
        match self {
            Self::Certain(stable) => *stable,
            Self::Unknown | Self::Runtime(_) | Self::Parameter(_) => false,
            Self::Combined(elements) => elements.iter().all(Self::known_stable),
        }
    }

    /// kotlinc's `@StabilityInferred(parameters)` of a class with `type_parameters`: a bit for each
    /// type parameter the stability depends on and, past them, a bit when it is known stable.
    fn parameter_mask(&self, type_parameters: usize) -> i32 {
        let mut mask = 0;
        let parts = match self {
            Self::Combined(parts) => parts.as_slice(),
            single => std::slice::from_ref(single),
        };
        for part in parts {
            if let Self::Parameter(index) = part {
                mask |= 1 << index;
            }
        }
        if self.known_stable() && type_parameters < 32 {
            mask |= 1 << type_parameters;
        }
        mask
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
        parameters: Vec::new(),
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
            markers.push((index, Some(Vec::new()), None));
            continue;
        }
        inference.parameters = ir
            .class_signature_name(class.fq_name)
            .map(|signature| {
                signature
                    .type_params
                    .iter()
                    .filter_map(|parameter| match parameter.ty().kind() {
                        TyKind::TyParam(name, _) => Some(name),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if inference.parameters.len() != class.type_params.len() {
            rejections.push(super::source_facts::class_rejection(
                source,
                class,
                "a generic class whose type parameters the file does not declare",
            ));
            continue;
        }
        match inference.of_class(class.fq_name, &mut Vec::new()) {
            Ok(stability) => {
                let stability = stability.normalize();
                let parameters = if class.type_params.is_empty() {
                    i32::from(stability.known_stable())
                } else {
                    stability.parameter_mask(class.type_params.len())
                };
                markers.push((index, stability.runtime_reads(), Some(parameters)));
            }
            Err(what) => rejections.push(super::source_facts::class_rejection(source, class, what)),
        }
    }
    for (index, reads, parameters) in markers {
        mark(ir, index, reads, parameters);
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
/// Give the class its `$stable`: unstable without `reads`, else the `$stable` fields it reads ored
/// together in kotlinc's order, or stable when it reads none.
fn mark(ir: &mut IrFile, index: usize, reads: Option<Vec<TypeName>>, parameters: Option<i32>) {
    let init = match reads.as_deref() {
        None => stable_bits(ir, UNSTABLE),
        Some([]) => stable_bits(ir, STABLE),
        Some([first, rest @ ..]) => {
            let mut value = read_stable(ir, *first);
            for &class in rest {
                let rhs = read_stable(ir, class);
                value = ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::BitOr,
                    lhs: value,
                    rhs,
                });
                ir.mark_positionless(value);
            }
            value
        }
    };
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

fn stable_bits(ir: &mut IrFile, bits: i32) -> ExprId {
    let value = ir.add_expr(IrExpr::Const(IrConst::Int(bits)));
    ir.mark_positionless(value);
    value
}

/// Read `class`'s `$stable`.
fn read_stable(ir: &mut IrFile, class: TypeName) -> ExprId {
    let value = ir.external_static_field_name(class, "$stable", "I");
    ir.mark_positionless(value);
    value
}

fn binary(annotation: &str, values: Vec<(String, AnnoValue)>) -> RetainedAnnotation {
    RetainedAnnotation {
        retention: AnnoRetention::Binary,
        annotation: AppliedAnnotation {
            written_order: None,
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
    /// The type parameters of the class whose stability is being recorded.
    parameters: Vec<TyParamName>,
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
        self.has_stable_marker(class.fq_name)
    }

    fn has_stable_marker(&self, name: TypeName) -> Result<bool, &'static str> {
        for annotation in self.annotations(name)? {
            if self.is_stable_marker(annotation)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The annotations a classifier's declaration applies, whatever their retention: kotlinc reads
    /// stable markers from the declaration, so a SOURCE-retained marker counts too.
    fn annotations(&self, name: TypeName) -> Result<Vec<TypeName>, &'static str> {
        match self.runtime.classifier(name) {
            Some(declared) => Ok(declared
                .annotations
                .iter()
                .map(|applied| applied.annotation)
                .collect()),
            None => Err("a class annotation the compilation does not declare"),
        }
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
        Ok(self
            .annotations(annotation)?
            .contains(&type_name(STABLE_MARKER)))
    }

    /// Whether `name` or one of its supertypes other than `Any` applies a stable marker (kotlinc's
    /// `hasStableMarkedDescendant`).
    fn has_stable_marked_descendant(&self, name: TypeName) -> Result<bool, &'static str> {
        let supertypes: Vec<TypeName> = match self.classifier(name) {
            Some(Classifier::File(class)) => std::iter::once(class.superclass)
                .chain(class.interfaces.iter())
                .collect(),
            Some(Classifier::Declared(declared)) => declared.supertypes,
            None => return Err("a class whose supertypes the compilation does not declare"),
        };
        for annotation in self.annotations(name)? {
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
        // Only the recorded class is analyzed without type arguments: its own parameters stay
        // parameters. Another generic class needs its arguments substituted.
        let generic = !class.type_params.is_empty();
        if generic && !analyzing.is_empty() {
            return Err("a class whose stability depends on a generic class");
        }
        analyzing.push(name);
        let stability = self.of_members(class, analyzing);
        analyzing.pop();
        let stability = stability?;
        if !generic {
            self.analyzed.insert(name, stability.clone());
        }
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
            if declared.type_parameters > 0 {
                return Err(
                    "a class whose stability depends on a generic class of another file or \
                     a Compose library",
                );
            }
            if declared.in_module
                && (declared.is_inner || declared.kind == ClassifierDeclarationKind::Annotation)
            {
                return Err(
                    "a class whose stability depends on another file's class without \
                     `$stable`",
                );
            }
            return Ok(Stability::Runtime(name));
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
            // kotlinc counts `KFunction` and its numbered `KFunctionN` subtypes as function types.
            TyKind::Obj(name, _) if name == type_name(K_FUNCTION) => Ok(Stability::STABLE),
            TyKind::Obj(name, _) if self.extends_k_function(name) => {
                Err("a class with a field of a numbered reflection function type")
            }
            TyKind::Nullable(inner) | TyKind::PlatformNullable(inner) => {
                self.of_type(inner, analyzing)
            }
            TyKind::Obj(name, arguments) => {
                if self.is_value_class(name) {
                    return self.of_value_class(name, arguments.len(), analyzing);
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
            TyKind::TyParam(name, _) => match self.parameters.iter().position(|&own| own == name) {
                Some(index) => Ok(Stability::Parameter(index as u32)),
                None => Err("a class with a field of another declaration's type parameter"),
            },
            TyKind::Nothing => Ok(Stability::UNSTABLE),
            _ => Err("a class with a field of a type kotlinc's stability inference reads apart"),
        }
    }

    /// Whether `name` declares `KFunction` as a direct supertype, as the numbered `KFunctionN` and
    /// `KSuspendFunctionN` types do.
    fn extends_k_function(&self, name: TypeName) -> bool {
        self.runtime
            .classifier(name)
            .is_some_and(|declared| declared.supertypes.contains(&type_name(K_FUNCTION)))
    }

    /// kotlinc's `stabilityOf` for an inline class type: stable when the value class applies a
    /// stable marker itself, else the stability of its underlying type, wherever it is declared.
    fn of_value_class(
        &mut self,
        name: TypeName,
        arguments: usize,
        analyzing: &mut Vec<TypeName>,
    ) -> Result<Stability, &'static str> {
        let (underlying, generic) = match self.classifier(name) {
            Some(Classifier::File(class)) => (
                class.fields.first().map(|field| field.ty),
                !class.type_params.is_empty(),
            ),
            Some(Classifier::Declared(declared)) => match declared.value_class {
                Some(value_class) => (
                    Some(value_class.underlying),
                    !value_class.type_parameters.is_empty(),
                ),
                None => (None, false),
            },
            None => (None, false),
        };
        let Some(underlying) = underlying else {
            return Err(
                "a class with a field of a value class whose underlying type the \
                        compilation does not declare",
            );
        };
        if self.has_stable_marker(name)? {
            return Ok(Stability::STABLE);
        }
        // kotlinc substitutes the type's arguments into the underlying type; one that names no
        // type parameter, like `Result`'s `Any?`, needs none.
        if (arguments > 0 || generic) && mentions_type_parameter(underlying) {
            return Err("a class with a field of a generic value class");
        }
        self.of_type(underlying, analyzing)
    }

    fn is_value_class(&self, name: TypeName) -> bool {
        match self.classifier(name) {
            Some(Classifier::File(class)) => class.is_value,
            Some(Classifier::Declared(declared)) => declared.value_class.is_some(),
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

/// Whether `ty` refers to a type parameter anywhere.
fn mentions_type_parameter(ty: Ty) -> bool {
    match ty.kind() {
        TyKind::TyParam(..) => true,
        TyKind::Obj(_, arguments) | TyKind::Intersection(arguments) => arguments
            .iter()
            .any(|&argument| mentions_type_parameter(argument)),
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner)
        | TyKind::InProjection(inner)
        | TyKind::OutProjection(inner)
        | TyKind::StarProjection(inner)
        | TyKind::RawProjection(inner)
        | TyKind::DefinitelyNotNull(inner) => mentions_type_parameter(inner),
        TyKind::Fun(signature) => signature
            .params
            .iter()
            .chain(std::iter::once(&signature.ret))
            .any(|&part| mentions_type_parameter(part)),
        _ => false,
    }
}

fn is_protobuf_message(class: TypeName) -> bool {
    PROTOBUF_MESSAGES
        .iter()
        .any(|&message| class == type_name(message))
}

fn is_any(class: TypeName) -> bool {
    class == type_name("kotlin/Any") || class == type_name("java/lang/Object")
}

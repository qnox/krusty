//! Target-neutral semantic declarations decoded from Kotlin metadata.
//!
//! Wire validation lives in [`crate::metadata::decode`]. This module owns the Kotlin declaration
//! shape that every dependency provider consumes, plus the one conversion from decoded Kotlin
//! types to the compiler's semantic [`Ty`](crate::types::Ty). Target adapters may add physical
//! realization facts after this boundary; they must not decode the declaration a second way.

use std::collections::HashMap;

use crate::libraries::TypeKind;
use crate::metadata::decode::{
    decode_module_optional_annotations, decode_package_fragment, strip_builtins_header,
    ClassAnnotationProtocol,
};
use crate::types::{Ty, Visibility};

pub use crate::metadata::decode::PackageFragmentDecodeError;

mod klib;

/// Decode one dependency-owned KLIB fragment atomically into common Kotlin declarations.
pub fn parse_package_fragment_checked(
    bytes: &[u8],
) -> Result<KotlinPackage, PackageFragmentDecodeError> {
    klib::parse(decode_package_fragment(bytes)?)
}

/// Decode a `.kotlin_builtins` resource through the same semantic boundary as a KLIB fragment.
pub fn parse_builtins(data: &[u8]) -> Result<KotlinPackage, PackageFragmentDecodeError> {
    let fragment = strip_builtins_header(data).ok_or_else(|| PackageFragmentDecodeError {
        offset: 0,
        detail: "truncated Kotlin builtins version header".to_string(),
    })?;
    let mut decoded = decode_package_fragment(fragment)?;
    decoded.class_annotations = ClassAnnotationProtocol::BuiltIns;
    klib::parse(decoded)
}

/// Decode the optional annotation classes of a JVM `META-INF/<module>.kotlin_module` file through
/// the same semantic boundary as a KLIB fragment's classes.
pub fn parse_module_optional_annotations(
    bytes: &[u8],
) -> Result<KotlinPackage, PackageFragmentDecodeError> {
    klib::parse(decode_module_optional_annotations(bytes)?)
}

/// A type decoded from Kotlin metadata before any target representation is chosen.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KotlinFunctionTypeShape {
    pub receiver: bool,
    pub context_count: usize,
    pub suspend: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KotlinType {
    Class {
        internal: String,
        args: Vec<KotlinType>,
        nullable: bool,
        shape: KotlinFunctionTypeShape,
    },
    /// A reference to a type parameter in scope. `id` is its declaration's identity; `name` is
    /// its spelling, kept for rendering and for consumers that scope parameters by name.
    Param {
        name: String,
        id: KotlinTypeParameterId,
        nullable: bool,
    },
    InProjection(Box<KotlinType>),
    OutProjection(Box<KotlinType>),
    /// The star projection `*` of a type argument.
    Star,
}

impl KotlinType {
    pub fn class(internal: impl Into<String>) -> Self {
        Self::Class {
            internal: internal.into(),
            args: Vec::new(),
            nullable: false,
            shape: KotlinFunctionTypeShape::default(),
        }
    }

    pub fn internal(&self) -> Option<&str> {
        match self {
            Self::Class { internal, .. } => Some(internal),
            Self::Param { .. } | Self::InProjection(_) | Self::OutProjection(_) | Self::Star => {
                None
            }
        }
    }

    pub fn nullable(&self) -> bool {
        match self {
            Self::Class { nullable, .. } | Self::Param { nullable, .. } => *nullable,
            Self::InProjection(_) | Self::OutProjection(_) | Self::Star => false,
        }
    }

    /// Diagnostic-only rendering of the decoded Kotlin type.
    pub fn render(&self) -> String {
        let (base, args, nullable) = match self {
            Self::Class {
                internal,
                args,
                nullable,
                ..
            } => (internal.clone(), args.as_slice(), *nullable),
            Self::Param { name, nullable, .. } => (name.clone(), &[][..], *nullable),
            Self::InProjection(inner) => return format!("in {}", inner.render()),
            Self::OutProjection(inner) => return format!("out {}", inner.render()),
            Self::Star => return "*".to_string(),
        };
        let mut out = base;
        if !args.is_empty() {
            let inner: Vec<String> = args.iter().map(Self::render).collect();
            out.push('<');
            out.push_str(&inner.join(","));
            out.push('>');
        }
        if nullable {
            out.push('?');
        }
        out
    }
}

pub(crate) fn project_kotlin_type(
    projection: crate::metadata::decode::ParsedProjection,
    ty: KotlinType,
) -> KotlinType {
    use crate::metadata::decode::ParsedProjection;
    match projection {
        ParsedProjection::In => KotlinType::InProjection(Box::new(ty)),
        ParsedProjection::Out => KotlinType::OutProjection(Box::new(ty)),
        ParsedProjection::Invariant => ty,
    }
}

pub struct KotlinMember {
    pub name: String,
    /// The extension receiver of a member extension.
    pub receiver: Option<KotlinType>,
    /// Context parameter types, in declaration order. They are not part of [`Self::params`].
    pub context_params: Vec<KotlinType>,
    pub visibility: Visibility,
    pub params: Vec<KotlinType>,
    pub ret: KotlinType,
    pub is_property: bool,
    /// A `var` property; never set on a function.
    pub is_var: bool,
    pub is_expect: bool,
    pub is_operator: bool,
    pub is_infix: bool,
    pub is_abstract: bool,
    /// Whether metadata declares this member static for KLIB signature mangling.
    pub is_static: bool,
    pub return_value_status: crate::types::ReturnValueStatus,
    pub formals: Vec<KotlinTypeParameter>,
    pub ret_nullable: bool,
    pub constant: Option<crate::libraries::LibConst>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
    /// Qualified identities of annotations declared on this member.
    pub annotations: Vec<crate::types::TypeName>,
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
}

/// One Kotlin function: a top-level one, or a member of the class that lists it.
pub struct KotlinFunction {
    pub name: String,
    pub receiver: Option<KotlinType>,
    pub params: Vec<KotlinType>,
    pub ret: KotlinType,
    pub formals: Vec<KotlinTypeParameter>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
    pub visibility: Visibility,
    /// Always [`KotlinModality::Final`] for a top-level function.
    pub modality: KotlinModality,
    pub is_inline: bool,
    pub has_reified_type_params: bool,
    pub is_suspend: bool,
    pub is_operator: bool,
    pub is_infix: bool,
    pub is_expect: bool,
    /// Whether metadata declares this function static: a companion extension
    /// (`companion fun C.name`), which KLIB signature mangling marks static.
    pub is_static: bool,
    pub context_count: usize,
    /// The role of each leading context parameter, one per context parameter: a legacy context
    /// receiver (`context(String)`), an anonymous context parameter (`context(_: String)`) or a
    /// named one.
    pub context_kinds: Vec<crate::types::ContextParameterKind>,
    /// Qualified identities of annotations declared on this function.
    pub annotations: Vec<crate::types::TypeName>,
    pub return_value_status: crate::types::ReturnValueStatus,
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
}

#[derive(Default)]
pub struct KotlinPackage {
    pub classes: HashMap<String, KotlinClass>,
    /// The keys of `classes` in the order the fragment declares them.
    pub class_order: Vec<String>,
    pub functions: Vec<KotlinFunction>,
    pub properties: Vec<KotlinProperty>,
    pub type_aliases: Vec<KotlinTypeAlias>,
}

/// One Kotlin type-alias declaration, before a provider publishes its qualified identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KotlinTypeAlias {
    pub name: String,
    pub formals: Vec<KotlinTypeParameter>,
    pub expansion: KotlinType,
    pub visibility: Visibility,
}

/// One Kotlin property: a top-level one, or a member of the class that lists it.
pub struct KotlinProperty {
    pub name: String,
    pub receiver: Option<KotlinType>,
    /// Context parameter types, in declaration order; `context_count` is their number.
    pub context_params: Vec<KotlinType>,
    pub ty: KotlinType,
    pub formals: Vec<KotlinTypeParameter>,
    pub visibility: Visibility,
    /// Always [`KotlinModality::Final`] for a top-level property.
    pub modality: KotlinModality,
    /// The setter's own visibility (`var x: Int private set`); the property's visibility when
    /// metadata records no setter flags. Meaningful only for a `var`.
    pub setter_visibility: Visibility,
    /// Source name of an explicitly declared setter value parameter; `None` for a default setter.
    pub setter_parameter_name: Option<String>,
    pub is_var: bool,
    /// A `const val`, whose compile-time value is `constant`.
    pub is_const: bool,
    pub is_expect: bool,
    /// Whether metadata declares this property static: a companion extension property
    /// (`companion val C.name`), which KLIB signature mangling marks static.
    pub is_static: bool,
    pub context_count: usize,
    /// Source names parallel to `context_params`; empty for a legacy context receiver.
    pub context_param_names: Vec<String>,
    /// The role of each context parameter, parallel to `context_params`.
    pub context_kinds: Vec<crate::types::ContextParameterKind>,
    pub constant: Option<crate::libraries::LibConst>,
    /// Qualified identities of annotations declared on this property.
    pub annotations: Vec<crate::types::TypeName>,
    pub return_value_status: crate::types::ReturnValueStatus,
}

pub struct KotlinConstructor {
    /// The class's primary constructor rather than a secondary one.
    pub is_primary: bool,
    pub params: Vec<KotlinType>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
    pub visibility: Visibility,
}

/// A type parameter's declaration identity: metadata's `TypeParameter.id`, unique along the chain
/// of declarations whose type parameters are in scope.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct KotlinTypeParameterId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KotlinTypeParameter {
    pub id: KotlinTypeParameterId,
    pub name: String,
    pub bounds: Vec<KotlinType>,
    pub variance: crate::types::TypeVariance,
    pub only_input: bool,
    pub reified: bool,
}

pub struct KotlinClass {
    pub supertypes: Vec<String>,
    pub supertype_tys: Vec<KotlinType>,
    /// Every function and property the class declares, in the condensed shape the JVM builtins
    /// records consume.
    pub members: Vec<KotlinMember>,
    /// The functions the class declares, as complete declarations in metadata order. These are
    /// the same declarations as the function [`Self::members`]; a provider that publishes the class
    /// normalizes and signs these.
    pub functions: Vec<KotlinFunction>,
    /// The properties the class declares, as complete declarations in metadata order.
    pub properties: Vec<KotlinProperty>,
    /// Type aliases declared in this classifier's namespace, in metadata order.
    pub type_aliases: Vec<KotlinTypeAlias>,
    pub constructors: Vec<KotlinConstructor>,
    pub companion_name: Option<String>,
    pub type_params: Vec<KotlinTypeParameter>,
    pub kind: TypeKind,
    pub is_fun_interface: bool,
    pub visibility: Visibility,
    pub is_expect: bool,
    pub enum_entries: Vec<String>,
    /// Whether this enum's metadata declares the implicit `entries` property.
    pub has_enum_entries: bool,
    pub sealed_subclasses: Vec<String>,
    pub inline_class_property: Option<String>,
    pub modality: KotlinModality,
    pub is_nested: bool,
    /// An `inner` class, which captures an instance of its enclosing class.
    pub is_inner: bool,
    /// Original Kotlin metadata flags retained for adapters that must preserve an external ABI.
    pub metadata_flags: u64,
    /// The class's own annotations, with the arguments a declaration-level reader needs.
    pub annotations: Vec<AnnotationApplication>,
    pub nullable_member_returns: Vec<(String, usize)>,
}

/// One annotation recorded on a metadata declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationApplication {
    pub identity: crate::types::TypeName,
    /// Arguments in recorded order, by parameter name.
    pub arguments: Vec<(String, AnnotationArgument)>,
}

/// A recorded annotation argument. Only the enum shape (and arrays of it) is decoded: that is what
/// `kotlin.annotation.Retention` and `kotlin.annotation.Target` carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationArgument {
    Enum {
        class: crate::types::TypeName,
        entry: String,
    },
    Array(Vec<AnnotationArgument>),
    Other,
}

impl AnnotationApplication {
    pub fn argument(&self, name: &str) -> Option<&AnnotationArgument> {
        self.arguments
            .iter()
            .find(|(parameter, _)| parameter == name)
            .map(|(_, value)| value)
    }
}

/// Kotlin declaration modality, independent of a target's access flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KotlinModality {
    Final,
    Open,
    Abstract,
    Sealed,
}

impl KotlinModality {
    pub fn is_abstract(self) -> bool {
        matches!(self, Self::Abstract | Self::Sealed)
    }

    pub fn is_extensible(self) -> bool {
        matches!(self, Self::Open | Self::Abstract)
    }
}

pub(crate) fn class_kind(flags: u64) -> TypeKind {
    match (flags >> 6) & 0x7 {
        1 => TypeKind::Interface,
        2 => TypeKind::Enum,
        4 => TypeKind::Annotation,
        5 | 6 => TypeKind::Object,
        _ => TypeKind::Class,
    }
}

pub(crate) fn declaration_visibility(flags: u64) -> Visibility {
    match (flags >> 1) & 0x7 {
        1 | 4 => Visibility::Private,
        2 => Visibility::Protected,
        3 => Visibility::Public,
        _ => Visibility::Internal,
    }
}

pub(crate) fn declaration_modality(flags: u64) -> KotlinModality {
    match (flags >> 4) & 0x3 {
        1 => KotlinModality::Open,
        2 => KotlinModality::Abstract,
        3 => KotlinModality::Sealed,
        _ => KotlinModality::Final,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum QualifiedNameResolutionError {
    AbsentEntry { index: usize },
    AbsentSegment { entry: usize, string: usize },
    InvalidParent { entry: usize, parent: i64 },
    InvalidKind { entry: usize, kind: u64 },
    Cycle { entry: usize },
}

impl std::fmt::Display for QualifiedNameResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AbsentEntry { index } => {
                write!(formatter, "references absent qualified name {index}")
            }
            Self::AbsentSegment { entry, string } => write!(
                formatter,
                "qualified-name entry {entry} references absent string {string}"
            ),
            Self::InvalidParent { entry, parent } => write!(
                formatter,
                "qualified-name entry {entry} has invalid parent {parent}"
            ),
            Self::InvalidKind { entry, kind } => {
                write!(
                    formatter,
                    "qualified-name entry {entry} has invalid kind {kind}"
                )
            }
            Self::Cycle { entry } => {
                write!(formatter, "qualified-name chain cycles at entry {entry}")
            }
        }
    }
}

pub(crate) fn resolve_qname(
    qnames: &[crate::metadata::decode::QName],
    strings: &[String],
    mut index: usize,
) -> Result<String, QualifiedNameResolutionError> {
    let mut packages = Vec::new();
    let mut classifiers = Vec::new();
    let mut remaining = qnames.len();
    loop {
        let qname = qnames
            .get(index)
            .ok_or(QualifiedNameResolutionError::AbsentEntry { index })?;
        if remaining == 0 {
            return Err(QualifiedNameResolutionError::Cycle { entry: index });
        }
        remaining -= 1;
        let segment =
            strings
                .get(qname.short)
                .ok_or(QualifiedNameResolutionError::AbsentSegment {
                    entry: index,
                    string: qname.short,
                })?;
        if qname.kind > 2 {
            return Err(QualifiedNameResolutionError::InvalidKind {
                entry: index,
                kind: qname.kind,
            });
        }
        if qname.kind == 1 {
            packages.insert(0, segment.as_str());
        } else {
            classifiers.insert(0, segment.as_str());
        }
        if qname.parent == -1 {
            break;
        }
        let parent = usize::try_from(qname.parent).map_err(|_| {
            QualifiedNameResolutionError::InvalidParent {
                entry: index,
                parent: qname.parent,
            }
        })?;
        if parent >= qnames.len() {
            return Err(QualifiedNameResolutionError::InvalidParent {
                entry: index,
                parent: qname.parent,
            });
        }
        index = parent;
    }
    let classifier = classifiers.join(".");
    if packages.is_empty() {
        Ok(classifier)
    } else {
        Ok(format!("{}/{classifier}", packages.join("/")))
    }
}

fn kotlin_primitive(internal: &str) -> Option<Ty> {
    Some(match internal {
        "kotlin/Int" => Ty::Int,
        "kotlin/Long" => Ty::Long,
        "kotlin/Short" => Ty::Short,
        "kotlin/Byte" => Ty::Byte,
        "kotlin/Double" => Ty::Double,
        "kotlin/Float" => Ty::Float,
        "kotlin/Boolean" => Ty::Boolean,
        "kotlin/Char" => Ty::Char,
        _ => return None,
    })
}

pub(crate) fn is_kotlin_function_classifier(internal: &str) -> bool {
    internal
        .strip_prefix("kotlin/Function")
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// A decoded Kotlin classifier and arguments as the compiler's target-neutral semantic type.
pub(crate) fn kotlin_type(internal: &str, mut args: Vec<Ty>, shape: KotlinFunctionTypeShape) -> Ty {
    if is_kotlin_function_classifier(internal) && !args.is_empty() {
        let ret = args.pop().expect("checked non-empty function arguments");
        let has_receiver = shape.receiver && !args.is_empty();
        let decoded = Ty::fun_with_shape(args, ret, shape.context_count, has_receiver, false);
        return if shape.suspend {
            source_suspend_function_type(decoded)
        } else {
            decoded
        };
    }
    if internal == "kotlin/Array" {
        return Ty::obj_args(
            "kotlin/Array",
            &[args.pop().unwrap_or_else(|| Ty::obj("kotlin/Any"))],
        );
    }
    if let Some(element) = internal.strip_suffix("Array").and_then(kotlin_primitive) {
        return Ty::array(element);
    }
    match kotlin_primitive(internal) {
        Some(ty) => ty,
        None => match internal {
            "kotlin/String" => Ty::String,
            "kotlin/Unit" => Ty::Unit,
            "kotlin/Nothing" => Ty::Nothing,
            _ => Ty::obj_args(internal, &args),
        },
    }
}

/// Convert the metadata carrier of a suspend function type back to its Kotlin source shape.
fn source_suspend_function_type(ty: Ty) -> Ty {
    let Ty::Fun(signature) = ty else {
        return ty;
    };
    let mut params = signature.params.clone();
    let ret = match params.last().copied().map(Ty::non_null) {
        Some(Ty::Obj(continuation, args))
            if continuation.matches("kotlin/coroutines/Continuation") =>
        {
            let ret = args
                .first()
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            params.pop();
            ret
        }
        _ => signature.ret,
    };
    Ty::fun_with_shape(
        params,
        ret,
        signature.context_count,
        signature.has_receiver,
        true,
    )
}

/// Convert a decoded Kotlin metadata type without consulting a target provider.
pub fn semantic_ty(ty: &KotlinType, bounds: &HashMap<KotlinTypeParameterId, Ty>) -> Ty {
    semantic_ty_with_identity_map(ty, bounds, None)
}

/// Convert a decoded type while replacing metadata parameter IDs with declaration-owned semantic
/// identities. Providers establish those identities once they know the exact external declaration.
pub fn semantic_ty_with_identities(
    ty: &KotlinType,
    bounds: &HashMap<KotlinTypeParameterId, Ty>,
    identities: &HashMap<KotlinTypeParameterId, &'static str>,
) -> Ty {
    semantic_ty_with_identity_map(ty, bounds, Some(identities))
}

fn semantic_ty_with_identity_map(
    ty: &KotlinType,
    bounds: &HashMap<KotlinTypeParameterId, Ty>,
    identities: Option<&HashMap<KotlinTypeParameterId, &'static str>>,
) -> Ty {
    let semantic = match ty {
        KotlinType::Class {
            internal,
            args,
            shape,
            ..
        } => {
            let args = args
                .iter()
                .map(|argument| semantic_ty_with_identity_map(argument, bounds, identities))
                .collect();
            kotlin_type(internal, args, *shape)
        }
        KotlinType::Param { name, id, .. } => {
            let bound = bounds
                .get(id)
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            let identity = match identities {
                Some(identities) => identities
                    .get(id)
                    .copied()
                    .expect("a provider-normalized type parameter has a declared identity"),
                None => name,
            };
            Ty::ty_param(identity, bound)
        }
        KotlinType::InProjection(inner) => {
            Ty::in_projection(semantic_ty_with_identity_map(inner, bounds, identities))
        }
        KotlinType::OutProjection(inner) => {
            Ty::out_projection(semantic_ty_with_identity_map(inner, bounds, identities))
        }
        KotlinType::Star => Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any"))),
    };
    if ty.nullable() {
        Ty::nullable(semantic)
    } else {
        semantic
    }
}

/// Declared primary upper bounds keyed by the metadata type-parameter identity.
pub fn semantic_bounds(
    params: &[KotlinTypeParameter],
    inherited: &HashMap<KotlinTypeParameterId, Ty>,
) -> HashMap<KotlinTypeParameterId, Ty> {
    semantic_bounds_with_identity_map(params, inherited, None)
}

pub fn semantic_bounds_with_identities(
    params: &[KotlinTypeParameter],
    inherited: &HashMap<KotlinTypeParameterId, Ty>,
    identities: &HashMap<KotlinTypeParameterId, &'static str>,
) -> HashMap<KotlinTypeParameterId, Ty> {
    semantic_bounds_with_identity_map(params, inherited, Some(identities))
}

fn semantic_bounds_with_identity_map(
    params: &[KotlinTypeParameter],
    inherited: &HashMap<KotlinTypeParameterId, Ty>,
    identities: Option<&HashMap<KotlinTypeParameterId, &'static str>>,
) -> HashMap<KotlinTypeParameterId, Ty> {
    let mut bounds = inherited.clone();
    for parameter in params {
        bounds.insert(parameter.id, Ty::nullable(Ty::obj("kotlin/Any")));
    }
    for parameter in params {
        let bound = parameter
            .bounds
            .first()
            .map(|bound| semantic_ty_with_identity_map(bound, &bounds, identities))
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        bounds.insert(parameter.id, bound);
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::decode::QName;

    fn class(internal: &str, args: Vec<KotlinType>) -> KotlinType {
        KotlinType::Class {
            internal: internal.to_string(),
            args,
            nullable: false,
            shape: KotlinFunctionTypeShape::default(),
        }
    }

    fn qname(parent: i64, short: usize, kind: u64) -> QName {
        QName {
            parent,
            short,
            kind,
        }
    }

    #[test]
    fn qualified_name_rejects_a_malformed_parent() {
        let error = resolve_qname(&[qname(-2, 0, 0)], &["Broken".to_string()], 0)
            .expect_err("only -1 is a valid negative parent sentinel");
        assert_eq!(
            error.to_string(),
            "qualified-name entry 0 has invalid parent -2"
        );

        let error = resolve_qname(&[qname(1, 0, 0)], &["Broken".to_string()], 0)
            .expect_err("a parent must address an existing qualified-name row");
        assert_eq!(
            error.to_string(),
            "qualified-name entry 0 has invalid parent 1"
        );
    }

    #[test]
    fn qualified_name_rejects_an_absent_segment() {
        let error = resolve_qname(&[qname(-1, 1, 0)], &["Present".to_string()], 0)
            .expect_err("every name-table segment must resolve");
        assert_eq!(
            error.to_string(),
            "qualified-name entry 0 references absent string 1"
        );
    }

    #[test]
    fn qualified_name_rejects_a_parent_cycle() {
        let qnames = [qname(1, 0, 0), qname(0, 1, 0)];
        let strings = ["First".to_string(), "Second".to_string()];
        let error = resolve_qname(&qnames, &strings, 0)
            .expect_err("a qualified-name chain must be acyclic");
        assert_eq!(error.to_string(), "qualified-name chain cycles at entry 0");
    }

    #[test]
    fn function_type_shape_survives_common_semantic_conversion() {
        let extension = KotlinType::Class {
            internal: "kotlin/Function2".to_string(),
            args: vec![
                class("kotlin/String", Vec::new()),
                class("kotlin/Int", Vec::new()),
                class("kotlin/Boolean", Vec::new()),
            ],
            nullable: false,
            shape: KotlinFunctionTypeShape {
                receiver: true,
                context_count: 0,
                suspend: false,
            },
        };
        let Ty::Fun(signature) = semantic_ty(&extension, &HashMap::new()) else {
            panic!("function classifier must become a semantic function type");
        };
        assert_eq!(signature.params.as_slice(), &[Ty::String, Ty::Int]);
        assert_eq!(signature.ret, Ty::Boolean);
        assert!(signature.has_receiver);
        assert_eq!(signature.context_count, 0);
        assert!(!signature.suspend);
    }

    #[test]
    fn suspend_function_metadata_carrier_restores_the_source_return() {
        let continuation = class(
            "kotlin/coroutines/Continuation",
            vec![class("kotlin/String", Vec::new())],
        );
        let suspend = KotlinType::Class {
            internal: "kotlin/Function2".to_string(),
            args: vec![
                class("kotlin/Int", Vec::new()),
                continuation,
                KotlinType::Class {
                    internal: "kotlin/Any".to_string(),
                    args: Vec::new(),
                    nullable: true,
                    shape: KotlinFunctionTypeShape::default(),
                },
            ],
            nullable: false,
            shape: KotlinFunctionTypeShape {
                suspend: true,
                ..Default::default()
            },
        };
        let Ty::Fun(signature) = semantic_ty(&suspend, &HashMap::new()) else {
            panic!("suspend classifier must become a semantic function type");
        };
        assert_eq!(signature.params.as_slice(), &[Ty::Int]);
        assert_eq!(signature.ret, Ty::String);
        assert!(signature.suspend);
    }

    #[test]
    fn type_parameter_bounds_are_selected_by_identity_not_spelling() {
        let outer = KotlinTypeParameterId(1);
        let inner = KotlinTypeParameterId(2);
        let bounds = HashMap::from([(outer, Ty::String), (inner, Ty::obj("kotlin/Number"))]);
        let parameter = |id| KotlinType::Param {
            name: "T".to_owned(),
            id,
            nullable: false,
        };

        assert_eq!(
            semantic_ty(&parameter(outer), &bounds).ty_param_bound(),
            Some(Ty::String)
        );
        assert_eq!(
            semantic_ty(&parameter(inner), &bounds).ty_param_bound(),
            Some(Ty::obj("kotlin/Number"))
        );
    }

    #[test]
    fn dependent_bounds_use_the_callers_type_parameter_identity_mode() {
        let first = KotlinTypeParameterId(11);
        let second = KotlinTypeParameterId(12);
        let parameters = [
            KotlinTypeParameter {
                id: first,
                name: "A".to_owned(),
                bounds: Vec::new(),
                variance: crate::types::TypeVariance::Invariant,
                only_input: false,
                reified: false,
            },
            KotlinTypeParameter {
                id: second,
                name: "B".to_owned(),
                bounds: vec![KotlinType::Param {
                    name: "A".to_owned(),
                    id: first,
                    nullable: false,
                }],
                variance: crate::types::TypeVariance::Invariant,
                only_input: false,
                reified: false,
            },
        ];

        let source = semantic_bounds(&parameters, &HashMap::new());
        assert_eq!(
            source[&second],
            Ty::ty_param("A", Ty::nullable(Ty::obj("kotlin/Any")))
        );

        let identities = HashMap::from([(first, "opaque-A"), (second, "opaque-B")]);
        let provider = semantic_bounds_with_identities(&parameters, &HashMap::new(), &identities);
        assert_eq!(
            provider[&second],
            Ty::ty_param("opaque-A", Ty::nullable(Ty::obj("kotlin/Any")))
        );
    }
}

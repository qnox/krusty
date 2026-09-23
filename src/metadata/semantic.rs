//! Target-neutral semantic declarations decoded from Kotlin metadata.
//!
//! Wire validation lives in [`crate::metadata::decode`]. This module owns the Kotlin declaration
//! shape that every dependency provider consumes, plus the one conversion from decoded Kotlin
//! types to the compiler's semantic [`Ty`](crate::types::Ty). Target adapters may add physical
//! realization facts after this boundary; they must not decode the declaration a second way.

use std::collections::HashMap;

use crate::libraries::TypeKind;
use crate::metadata::decode::{decode_package_fragment, strip_builtins_header};
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
    parse_package_fragment_checked(fragment)
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
    Param {
        name: String,
        nullable: bool,
    },
    InProjection(Box<KotlinType>),
    OutProjection(Box<KotlinType>),
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
            Self::Param { .. } | Self::InProjection(_) | Self::OutProjection(_) => None,
        }
    }

    pub fn nullable(&self) -> bool {
        match self {
            Self::Class { nullable, .. } | Self::Param { nullable, .. } => *nullable,
            Self::InProjection(_) | Self::OutProjection(_) => false,
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
            Self::Param { name, nullable } => (name.clone(), &[][..], *nullable),
            Self::InProjection(inner) => return format!("in {}", inner.render()),
            Self::OutProjection(inner) => return format!("out {}", inner.render()),
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
    pub params: Vec<KotlinType>,
    pub ret: KotlinType,
    pub is_property: bool,
    pub is_operator: bool,
    pub is_infix: bool,
    pub is_abstract: bool,
    pub formals: Vec<KotlinTypeParameter>,
    pub ret_nullable: bool,
    pub constant: Option<crate::libraries::LibConst>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
}

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
    pub is_inline: bool,
    pub has_reified_type_params: bool,
    pub is_suspend: bool,
    pub is_operator: bool,
    pub is_infix: bool,
    pub context_count: usize,
}

#[derive(Default)]
pub struct KotlinPackage {
    pub classes: HashMap<String, KotlinClass>,
    pub functions: Vec<KotlinFunction>,
    pub properties: Vec<KotlinProperty>,
}

/// One top-level Kotlin property. Member properties remain [`KotlinMember`]s because they have no
/// extension receiver or package namespace of their own.
pub struct KotlinProperty {
    pub name: String,
    pub receiver: Option<KotlinType>,
    pub ty: KotlinType,
    pub formals: Vec<KotlinTypeParameter>,
    pub visibility: Visibility,
    pub is_var: bool,
    pub context_count: usize,
    pub constant: Option<crate::libraries::LibConst>,
}

pub struct KotlinConstructor {
    pub params: Vec<KotlinType>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
    pub visibility: Visibility,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KotlinTypeParameter {
    pub name: String,
    pub bounds: Vec<KotlinType>,
    pub variance: crate::types::TypeVariance,
    pub only_input: bool,
}

pub struct KotlinClass {
    pub supertypes: Vec<String>,
    pub supertype_tys: Vec<KotlinType>,
    pub members: Vec<KotlinMember>,
    pub constructors: Vec<KotlinConstructor>,
    pub companion_name: Option<String>,
    pub type_params: Vec<KotlinTypeParameter>,
    pub kind: TypeKind,
    pub is_fun_interface: bool,
    pub visibility: Visibility,
    pub is_expect: bool,
    pub enum_entries: Vec<String>,
    pub sealed_subclasses: Vec<String>,
    pub inline_class_property: Option<String>,
    pub modality: KotlinModality,
    pub is_nested: bool,
    /// Original Kotlin metadata flags retained for adapters that must preserve an external ABI.
    pub metadata_flags: u64,
    pub nullable_member_returns: Vec<(String, usize)>,
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
pub fn semantic_ty(ty: &KotlinType, bounds: &HashMap<String, Ty>) -> Ty {
    let semantic = match ty {
        KotlinType::Class {
            internal,
            args,
            shape,
            ..
        } => {
            let args = args
                .iter()
                .map(|argument| semantic_ty(argument, bounds))
                .collect();
            kotlin_type(internal, args, *shape)
        }
        KotlinType::Param { name, .. } => {
            let bound = bounds
                .get(name)
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(name, bound)
        }
        KotlinType::InProjection(inner) => Ty::in_projection(semantic_ty(inner, bounds)),
        KotlinType::OutProjection(inner) => Ty::out_projection(semantic_ty(inner, bounds)),
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
    inherited: &HashMap<String, Ty>,
) -> HashMap<String, Ty> {
    let mut bounds = inherited.clone();
    for parameter in params {
        let bound = parameter
            .bounds
            .first()
            .map(|bound| semantic_ty(bound, &HashMap::new()))
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        bounds.insert(parameter.name.clone(), bound);
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
}

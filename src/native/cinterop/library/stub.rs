//! The Kotlin declarations an interop library is made of, before they are written as metadata:
//! classes, type aliases, functions and properties with the flags and annotations
//! Kotlin/Native's cinterop gives them.

use super::kotlin_type::{named, KotlinType};
use crate::types::TypeName;

/// `Flags` of the declarations cinterop writes, as Kotlin metadata encodes them.
pub(super) mod flags {
    /// A public final class with annotations.
    pub(in super::super) const CLASS: u64 = 7;
    /// A public final companion object with annotations.
    pub(in super::super) const COMPANION: u64 = 391;
    /// A public final companion object without annotations.
    pub(in super::super) const PLAIN_COMPANION: u64 = 390;
    /// A public final enum class with annotations.
    pub(in super::super) const ENUM_CLASS: u64 = 32903;
    /// A private constructor.
    pub(in super::super) const PRIVATE_CONSTRUCTOR: u64 = 2;
    /// A public final external function with annotations.
    pub(in super::super) const EXTERNAL_FUNCTION: u64 = 4103;
    /// Added to a function whose parameter names are made up (`arg0`).
    pub(in super::super) const NON_STABLE_PARAMETER_NAMES: u64 = 32768;
    /// A public final function with annotations.
    pub(in super::super) const FUNCTION: u64 = 7;
    /// A member `var` with a getter and setter.
    pub(in super::super) const MEMBER_VAR: u64 = 1798;
    /// A member `val` with a getter: the default, which is not written.
    pub(in super::super) const MEMBER_VAL: u64 = 518;
    /// A top-level `var` with annotations.
    pub(in super::super) const GLOBAL_VAR: u64 = 1799;
    /// A top-level `val` with annotations.
    pub(in super::super) const GLOBAL_VAL: u64 = 519;
    /// A top-level `const val` with annotations.
    pub(in super::super) const CONST_VAL: u64 = 10759;
    /// An external accessor with annotations.
    pub(in super::super) const ANNOTATED_ACCESSOR: u64 = 199;
    /// An external accessor.
    pub(in super::super) const EXTERNAL_ACCESSOR: u64 = 198;
    /// A default accessor.
    pub(in super::super) const DEFAULT_ACCESSOR: u64 = 6;
    /// A public final type alias with annotations.
    pub(in super::super) const TYPE_ALIAS: u64 = 7;
    /// A value parameter with annotations.
    pub(in super::super) const ANNOTATED_PARAMETER: u64 = 1;
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Annotation {
    pub(super) class: TypeName,
    pub(super) arguments: Vec<(&'static str, Value)>,
}

impl Annotation {
    pub(super) fn new(class: &str, arguments: Vec<(&'static str, Value)>) -> Self {
        Self {
            class: named(class),
            arguments,
        }
    }

    /// `@ExperimentalForeignApi`, on every declaration of the library.
    pub(super) fn experimental() -> Self {
        Self::new("kotlinx/cinterop/ExperimentalForeignApi", Vec::new())
    }

    /// `@Deprecated(message, ReplaceWith(""), WARNING)`.
    pub(super) fn deprecated(message: &str) -> Self {
        Self::new(
            "kotlin/Deprecated",
            vec![
                ("message", Value::String(message.to_string())),
                (
                    "replaceWith",
                    Value::Annotation(Self::new(
                        "kotlin/ReplaceWith",
                        vec![
                            ("imports", Value::Array(Vec::new())),
                            ("expression", Value::String(String::new())),
                        ],
                    )),
                ),
                (
                    "level",
                    Value::Enum(named("kotlin/DeprecationLevel"), "WARNING"),
                ),
            ],
        )
    }

    /// `@CCall.Direct(name)`: the C symbol a function or getter calls.
    pub(super) fn direct(symbol: &str) -> Self {
        Self::new(
            "kotlinx/cinterop/internal/CCall.Direct",
            vec![("name", Value::String(symbol.to_string()))],
        )
    }
}

/// An annotation argument or a constant's value.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Value {
    Integer {
        kind: IntegerKind,
        value: i64,
        unsigned: bool,
    },
    Float(f32),
    Double(f64),
    String(String),
    Enum(TypeName, &'static str),
    Annotation(Annotation),
    Array(Vec<Value>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IntegerKind {
    Byte,
    Short,
    Int,
    Long,
}

impl Value {
    pub(super) fn long(value: u64) -> Self {
        Self::Integer {
            kind: IntegerKind::Long,
            value: value as i64,
            unsigned: false,
        }
    }

    pub(super) fn int(value: u64) -> Self {
        Self::Integer {
            kind: IntegerKind::Int,
            value: value as i64,
            unsigned: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Parameter {
    pub(super) name: String,
    pub(super) ty: KotlinType,
    pub(super) vararg_element: Option<KotlinType>,
    pub(super) annotations: Vec<Annotation>,
}

impl Parameter {
    pub(super) fn new(name: &str, ty: KotlinType) -> Self {
        Self {
            name: name.to_string(),
            ty,
            vararg_element: None,
            annotations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Function {
    pub(super) name: String,
    pub(super) flags: u64,
    pub(super) result: KotlinType,
    pub(super) parameters: Vec<Parameter>,
    pub(super) annotations: Vec<Annotation>,
}

/// A property: a struct field, a global, a constant, an enum's value.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Property {
    pub(super) name: String,
    pub(super) flags: u64,
    pub(super) ty: KotlinType,
    pub(super) getter_flags: u64,
    /// A `var`'s setter, whose parameter is `value` of the property's type.
    pub(super) setter_flags: Option<u64>,
    pub(super) annotations: Vec<Annotation>,
    pub(super) getter_annotations: Vec<Annotation>,
    pub(super) setter_annotations: Vec<Annotation>,
    pub(super) constant: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Constructor {
    /// `None` for the default flags, a public constructor.
    pub(super) flags: Option<u64>,
    pub(super) parameters: Vec<Parameter>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct EnumEntry {
    pub(super) name: String,
    pub(super) annotations: Vec<Annotation>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Class {
    pub(super) name: TypeName,
    /// `None` for the default flags, a public final class without annotations.
    pub(super) flags: Option<u64>,
    pub(super) companion: Option<&'static str>,
    pub(super) supertypes: Vec<KotlinType>,
    pub(super) nested: Vec<&'static str>,
    pub(super) constructors: Vec<Constructor>,
    pub(super) functions: Vec<Function>,
    pub(super) properties: Vec<Property>,
    pub(super) entries: Vec<EnumEntry>,
    pub(super) annotations: Vec<Annotation>,
}

impl Class {
    pub(super) fn new(name: TypeName, flags: Option<u64>) -> Self {
        Self {
            name,
            flags,
            companion: None,
            supertypes: Vec::new(),
            nested: Vec::new(),
            constructors: Vec::new(),
            functions: Vec::new(),
            properties: Vec::new(),
            entries: Vec::new(),
            annotations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TypeAlias {
    pub(super) name: String,
    pub(super) underlying: KotlinType,
}

/// Everything one package of the library declares, in the order the fragments list it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Package {
    /// Each top-level class with the classes nested in it, outermost first.
    pub(super) classes: Vec<Vec<Class>>,
    pub(super) aliases: Vec<TypeAlias>,
    pub(super) functions: Vec<Function>,
    pub(super) properties: Vec<Property>,
}

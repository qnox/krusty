//! Typed annotation argument values published by checked sources and metadata providers.

use super::{Ty, TypeName, WrittenAnnotation};

/// One frontend-checked annotation argument. Every classifier identity is resolved before this
/// crosses into common lowering; backends only choose the physical encoding of the recorded value.
/// Its `Debug` form distinguishes every floating value, NaN payloads included.
#[derive(Clone, PartialEq)]
pub enum AnnotationValue {
    Int(i32),
    /// A `byte`/`short` element. The JVM writes these with their own element tags (`B`, `S`), so a
    /// narrower element cannot borrow `Int`: reading such an annotation back throws
    /// `AnnotationTypeMismatchException`.
    Byte(i8),
    Short(i16),
    Long(i64),
    /// Unsigned constants keep their semantic width and complete bit pattern.
    UByte(u8),
    UShort(u16),
    UInt(u32),
    ULong(u64),
    Float(f32),
    Double(f64),
    Boolean(bool),
    Char(u16),
    String(crate::kt_string::KtString),
    Enum(TypeName, String),
    /// The exact semantic type represented by a class literal. Arrays retain every dimension and
    /// their component type; collapsing `Array<String>::class` to the `kotlin/Array` classifier
    /// loses the class-file descriptor before lowering can encode it.
    Class(Ty),
    /// A nested annotation. `written` is how the source wrote it, `None` for one read from a
    /// class file or made by a compiler plugin.
    Annotation {
        internal: TypeName,
        values: Vec<(String, AnnotationValue)>,
        written: Option<Box<WrittenAnnotation>>,
    },
    Array(Vec<AnnotationValue>),
}

impl std::fmt::Debug for AnnotationValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(value) => f.debug_tuple("Int").field(value).finish(),
            Self::Byte(value) => f.debug_tuple("Byte").field(value).finish(),
            Self::Short(value) => f.debug_tuple("Short").field(value).finish(),
            Self::Long(value) => f.debug_tuple("Long").field(value).finish(),
            Self::UByte(value) => f.debug_tuple("UByte").field(value).finish(),
            Self::UShort(value) => f.debug_tuple("UShort").field(value).finish(),
            Self::UInt(value) => f.debug_tuple("UInt").field(value).finish(),
            Self::ULong(value) => f.debug_tuple("ULong").field(value).finish(),
            Self::Float(value) => f
                .debug_tuple("Float")
                .field(&ExactFloat::from(*value))
                .finish(),
            Self::Double(value) => f
                .debug_tuple("Double")
                .field(&ExactFloat::from(*value))
                .finish(),
            Self::Boolean(value) => f.debug_tuple("Boolean").field(value).finish(),
            Self::Char(value) => f.debug_tuple("Char").field(value).finish(),
            Self::String(value) => f.debug_tuple("String").field(value).finish(),
            Self::Enum(class, entry) => f.debug_tuple("Enum").field(class).field(entry).finish(),
            Self::Class(ty) => f.debug_tuple("Class").field(ty).finish(),
            Self::Annotation {
                internal,
                values,
                written,
            } => f
                .debug_struct("Annotation")
                .field("internal", internal)
                .field("values", values)
                .field("written", written)
                .finish(),
            Self::Array(values) => f.debug_tuple("Array").field(values).finish(),
        }
    }
}

/// A floating value whose `Debug` form is distinct for distinct bits. Rust prints every NaN as
/// `NaN`, but a class file can carry any NaN payload and the compiler emits the bits it read, so a
/// NaN also shows its bits. Every other value already prints distinctly, `-0.0` included.
pub(crate) enum ExactFloat {
    Single(f32),
    Double(f64),
}

impl From<f32> for ExactFloat {
    fn from(value: f32) -> Self {
        Self::Single(value)
    }
}

impl From<f64> for ExactFloat {
    fn from(value: f64) -> Self {
        Self::Double(value)
    }
}

impl std::fmt::Debug for ExactFloat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Single(value) if value.is_nan() => write!(f, "NaN({:#010x})", value.to_bits()),
            Self::Double(value) if value.is_nan() => write!(f, "NaN({:#018x})", value.to_bits()),
            Self::Single(value) => write!(f, "{value:?}"),
            Self::Double(value) => write!(f, "{value:?}"),
        }
    }
}

impl AnnotationValue {
    /// A `String` element holding `value`.
    pub fn string(value: &str) -> Self {
        Self::String(crate::kt_string::KtString::from(value))
    }
}

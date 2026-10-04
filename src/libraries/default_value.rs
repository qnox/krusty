//! File-independent closed default values attached to callable metadata.

use crate::types::{Ty, TypeName};

/// A source default value captured without an AST/file identity. Providers attach these directly to
/// callable metadata so any selected source callable can be lowered without looking its declaration
/// up again. Unrepresentable defaults remain `None` and therefore cannot be silently miscompiled.
#[derive(Clone, Debug, PartialEq)]
pub enum DefaultValue {
    Int(i64),
    Long(i64),
    Double(f64),
    Float(f32),
    Bool(bool),
    Char(u16),
    Str(crate::kt_string::KtString),
    Null,
    Object(TypeName),
    /// An enum entry (`E.B`).
    EnumEntry {
        classifier: TypeName,
        name: String,
    },
    /// An unbound class literal (`B::class`, `Int::class`, or `Array<String>::class`).
    ///
    /// The full semantic type is the literal's identity. Restricting this to an object classifier
    /// loses primitive and array literals before a backend can choose their representation.
    KClass(Ty),
    /// A closed array: `arrayOf`, a primitive array factory, or a collection literal.
    Array {
        array_type: Ty,
        elements: Vec<DefaultValue>,
    },
    /// A nested annotation instance whose every member value is present.
    Annotation {
        classifier: TypeName,
        members: Vec<(String, Ty)>,
        values: Vec<DefaultValue>,
    },
}

impl DefaultValue {
    pub fn fills_param_ty(&self, ty: Ty) -> bool {
        match self {
            Self::Int(_) => ty.int_arithmetic_repr() == Ty::Int,
            Self::Long(_) => ty == Ty::Long,
            Self::Double(_) => ty == Ty::Double,
            Self::Float(_) => ty == Ty::Float,
            Self::Bool(_) => ty == Ty::Boolean,
            Self::Char(_) => ty == Ty::Char,
            Self::Str(_) => ty == Ty::String,
            Self::Null => ty.is_reference(),
            Self::Object(_)
            | Self::EnumEntry { .. }
            | Self::KClass(_)
            | Self::Array { .. }
            | Self::Annotation { .. } => false,
        }
    }
}

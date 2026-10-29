//! Compiler-owned storage roles for authenticated platform exception declarations.

use crate::types::{type_name, TypeName};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeExceptionKind {
    Throwable,
    Exception,
    RuntimeException,
    IndexOutOfBounds,
    NullPointer,
}

impl RuntimeExceptionKind {
    /// The language-runtime declaration this storage operation requires. A classifier at this
    /// identity still needs the authenticated role; the identity alone grants no representation.
    pub fn classifier(self) -> TypeName {
        type_name(match self {
            Self::Throwable => "kotlin/Throwable",
            Self::Exception => "kotlin/Exception",
            Self::RuntimeException => "kotlin/RuntimeException",
            Self::IndexOutOfBounds => "kotlin/IndexOutOfBoundsException",
            Self::NullPointer => "kotlin/NullPointerException",
        })
    }
}

/// Storage authority is separate from executable constructor authority. The parent comes from
/// the selected declaration's metadata, never from the exception's role or source spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeExceptionLayout {
    pub kind: RuntimeExceptionKind,
    pub parent: Option<TypeName>,
}

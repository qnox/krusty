//! Compiler-owned storage roles for authenticated platform exception declarations.

use crate::types::{type_name, TypeName};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeExceptionKind {
    Throwable,
    Exception,
    RuntimeException,
    IndexOutOfBounds,
    NullPointer,
    IllegalState,
    IllegalArgument,
    UnsupportedOperation,
    Arithmetic,
    NumberFormat,
    ClassCast,
    NoSuchElement,
    ConcurrentModification,
    UninitializedPropertyAccess,
}

impl RuntimeExceptionKind {
    const ALL: [Self; 14] = [
        Self::Throwable,
        Self::Exception,
        Self::RuntimeException,
        Self::IndexOutOfBounds,
        Self::NullPointer,
        Self::IllegalState,
        Self::IllegalArgument,
        Self::UnsupportedOperation,
        Self::Arithmetic,
        Self::NumberFormat,
        Self::ClassCast,
        Self::NoSuchElement,
        Self::ConcurrentModification,
        Self::UninitializedPropertyAccess,
    ];

    /// The role whose language-runtime declaration is `classifier`, if any.
    pub fn of(classifier: TypeName) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.classifier() == classifier)
    }

    /// The language-runtime declaration this storage operation requires. A classifier at this
    /// identity still needs the authenticated role; the identity alone grants no representation.
    pub fn classifier(self) -> TypeName {
        type_name(match self {
            Self::Throwable => "kotlin/Throwable",
            Self::Exception => "kotlin/Exception",
            Self::RuntimeException => "kotlin/RuntimeException",
            Self::IndexOutOfBounds => "kotlin/IndexOutOfBoundsException",
            Self::NullPointer => "kotlin/NullPointerException",
            Self::IllegalState => "kotlin/IllegalStateException",
            Self::IllegalArgument => "kotlin/IllegalArgumentException",
            Self::UnsupportedOperation => "kotlin/UnsupportedOperationException",
            Self::Arithmetic => "kotlin/ArithmeticException",
            Self::NumberFormat => "kotlin/NumberFormatException",
            Self::ClassCast => "kotlin/ClassCastException",
            Self::NoSuchElement => "kotlin/NoSuchElementException",
            Self::ConcurrentModification => "kotlin/ConcurrentModificationException",
            Self::UninitializedPropertyAccess => "kotlin/UninitializedPropertyAccessException",
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

#[cfg(test)]
mod tests {
    use super::RuntimeExceptionKind;

    /// Every role is found through its own classifier. The match is exhaustive, so a new role
    /// does not compile here until it gets a position, and then fails until `ALL` lists it.
    #[test]
    fn every_role_is_found_by_its_classifier() {
        let position = |kind: RuntimeExceptionKind| match kind {
            RuntimeExceptionKind::Throwable => 0,
            RuntimeExceptionKind::Exception => 1,
            RuntimeExceptionKind::RuntimeException => 2,
            RuntimeExceptionKind::IndexOutOfBounds => 3,
            RuntimeExceptionKind::NullPointer => 4,
            RuntimeExceptionKind::IllegalState => 5,
            RuntimeExceptionKind::IllegalArgument => 6,
            RuntimeExceptionKind::UnsupportedOperation => 7,
            RuntimeExceptionKind::Arithmetic => 8,
            RuntimeExceptionKind::NumberFormat => 9,
            RuntimeExceptionKind::ClassCast => 10,
            RuntimeExceptionKind::NoSuchElement => 11,
            RuntimeExceptionKind::ConcurrentModification => 12,
            RuntimeExceptionKind::UninitializedPropertyAccess => 13,
        };
        let positions: Vec<usize> = RuntimeExceptionKind::ALL
            .into_iter()
            .map(position)
            .collect();
        assert_eq!(positions, (0..14).collect::<Vec<_>>());
        for kind in RuntimeExceptionKind::ALL {
            assert_eq!(RuntimeExceptionKind::of(kind.classifier()), Some(kind));
        }
    }
}

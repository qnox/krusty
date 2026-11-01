//! Closed annotation defaults and the declaration-owned shape a checked construction retains.

use super::{FirConstant, ResolvedTy, Ty, TypeName};
use crate::libraries::AnnotationArgumentProvenance;

/// Compact declaration default retained only for an annotation-construction plan. Ordinary source
/// defaults remain checked retained bodies; dependency metadata contributes only these closed values.
#[derive(Clone, Debug, PartialEq)]
pub enum FirAnnotationDefaultValue {
    Constant(FirConstant),
    Singleton(TypeName),
    EnumEntry {
        classifier: TypeName,
        name: Box<str>,
    },
    KClass(Ty),
    Array {
        array_type: Ty,
        elements: Vec<Self>,
    },
    Annotation {
        classifier: TypeName,
        members: Vec<(String, Ty)>,
        values: Vec<Self>,
        arguments: AnnotationArgumentProvenance,
    },
}

/// Complete checked declaration shape needed to realize an annotation value. This is Kotlin
/// semantics rather than a platform implementation detail: a backend may allocate a concrete value,
/// synthesize an implementation, or use a native annotation representation without new lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct FirAnnotationConstruction {
    pub members: Box<[(Box<str>, ResolvedTy)]>,
    pub defaults: Box<[Option<FirAnnotationDefaultValue>]>,
    pub arguments: AnnotationArgumentProvenance,
}

impl FirAnnotationConstruction {
    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.members.len() * std::mem::size_of::<(Box<str>, ResolvedTy)>()
            + self
                .members
                .iter()
                .map(|(name, _)| name.len())
                .sum::<usize>()
            + self.defaults.len() * std::mem::size_of::<Option<FirAnnotationDefaultValue>>()
            + self
                .defaults
                .iter()
                .flatten()
                .map(FirAnnotationDefaultValue::storage_payload_bytes)
                .sum::<usize>()
            + self.arguments.storage_payload_bytes()
    }
}

impl FirAnnotationDefaultValue {
    fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::Constant(FirConstant::String(value)) => value.len_utf16() * 2,
            Self::EnumEntry { name, .. } => name.len(),
            Self::Array { elements, .. } => {
                elements.len() * std::mem::size_of::<Self>()
                    + elements
                        .iter()
                        .map(Self::storage_payload_bytes)
                        .sum::<usize>()
            }
            Self::Annotation {
                members,
                values,
                arguments,
                ..
            } => {
                members.len() * std::mem::size_of::<(String, Ty)>()
                    + members.iter().map(|(name, _)| name.len()).sum::<usize>()
                    + values.len() * std::mem::size_of::<Self>()
                    + values
                        .iter()
                        .map(Self::storage_payload_bytes)
                        .sum::<usize>()
                    + arguments.storage_payload_bytes()
            }
            Self::Constant(_) | Self::Singleton(_) | Self::KClass(_) => 0,
        }
    }
}

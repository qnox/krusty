//! Checked property targets and inline-accessor splice contracts.

use crate::types::TypeName;

use super::{CallableId, DeclarationId, PropertyId, ResolvedTy};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirPropertyDispatch {
    Ordinary,
    /// Non-virtual dispatch selected through `super`. This says nothing about physical storage:
    /// the target backend still decides whether the exact property is a field or an accessor.
    Super {
        owner: TypeName,
        interface: bool,
    },
}

/// A checker-selected inline accessor and the type arguments already fixed for this use.
/// Later expansion consumes this record; it does not look the accessor or its type parameters up again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineAccessorSplice {
    pub accessor: DeclarationId,
    /// Stable callable identity used to retain/materialize the selected accessor body across
    /// source-file lowering units.
    pub callable: CallableId,
    pub substitutions: Box<[FirInlineTypeSubstitution]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineTypeSubstitution {
    pub name: Box<str>,
    pub reified: bool,
    pub value: ResolvedTy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirPropertyTarget {
    Module {
        property: PropertyId,
        /// `Some` when this use's getter or setter was selected as `inline`.
        inline_splice: Option<Box<FirInlineAccessorSplice>>,
    },
    External {
        property: crate::fir::ExternalPropertyId,
        receiver: Option<ResolvedTy>,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
        /// Parameter slot occupied by the extension receiver when this accessor belongs to a
        /// member-extension property. Ordinary members and top-level extensions use `None`.
        extension_receiver_parameter: Option<u32>,
        dispatch: FirPropertyDispatch,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirPropertyReferenceTarget {
    Module(PropertyId),
    /// A source-module property after callable-reference selection and generic specialization.
    /// `property` is the stable declaration identity; the remaining fields are the final callable
    /// view consumed by an adapted function value. Lowering must not reconstruct these types from
    /// a symbolic declaration signature.
    SpecializedModule {
        property: PropertyId,
        /// Exact classifier the reference was written on for an instance member. This is selected
        /// with the property and controls reflection substitution; a backend must not reconstruct
        /// it from the specialized receiver type or the declaring accessor owner.
        reflection_owner: Option<TypeName>,
        /// Accessor declaration spellings carried by the provider-selected property candidate.
        /// These are declaration facts, not names reconstructed from the property spelling by a
        /// later backend pass.
        getter_name: Box<str>,
        setter_name: Option<Box<str>>,
        receiver: Option<ResolvedTy>,
        extension_receiver: bool,
        property_type: ResolvedTy,
        /// The extension receiver and property type the declaration itself declares. Its
        /// accessors are compiled once against these, so the reference's specialized receiver
        /// and result cross into and out of them.
        declared_receiver: Option<ResolvedTy>,
        declared_property_type: ResolvedTy,
        /// Exact checked getter splice for this reference use. A function-valued property
        /// reference materializes a read later, but lowering must not rediscover whether the
        /// accessor is inline or reconstruct its type arguments. The substitutions mirror the
        /// expression's authoritative substitution list, which remains the inline-plan mutation
        /// inventory; this record adds the selected accessor identity.
        getter_inline_splice: Option<Box<FirInlineAccessorSplice>>,
    },
    Classifier {
        owner: TypeName,
        property: FirClassifierProperty,
        property_type: ResolvedTy,
    },
    /// A dependency property after callable-reference selection.
    ///
    /// It carries no name of its own. The property's is the provider's, decoded from the
    /// declaration's metadata and reachable through `getter`'s [`crate::fir::ExternalPropertyId`].
    /// A copy here was the reference site's spelling, which is a different fact: a lookup may reach
    /// a declaration under an import alias, and the accessor cannot stand in for it either because
    /// its name is a physical call target a JVM realization may rename or value-class-mangle.
    External {
        reflection_owner: Option<ResolvedTy>,
        getter: Box<FirPropertyTarget>,
        setter: Option<Box<FirPropertyTarget>>,
        extension_receiver: bool,
        property_type: ResolvedTy,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirClassifierProperty {
    EnumEntries,
}

impl From<PropertyId> for FirPropertyReferenceTarget {
    fn from(target: PropertyId) -> Self {
        Self::Module(target)
    }
}

impl FirPropertyReferenceTarget {
    pub const fn module(&self) -> Option<PropertyId> {
        match self {
            Self::Module(target) => Some(*target),
            Self::SpecializedModule { property, .. } => Some(*property),
            Self::Classifier { .. } | Self::External { .. } => None,
        }
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::Module(_) | Self::Classifier { .. } => 0,
            Self::SpecializedModule {
                getter_inline_splice,
                ..
            } => getter_inline_splice.as_ref().map_or(0, |splice| {
                splice.substitutions.len() * std::mem::size_of::<FirInlineTypeSubstitution>()
            }),
            Self::External { getter, setter, .. } => {
                getter.storage_payload_bytes()
                    + setter
                        .as_deref()
                        .map_or(0, FirPropertyTarget::storage_payload_bytes)
            }
        }
    }
}

impl From<PropertyId> for FirPropertyTarget {
    fn from(target: PropertyId) -> Self {
        Self::of_module(target)
    }
}

impl FirPropertyTarget {
    pub const fn of_module(property: PropertyId) -> Self {
        Self::Module {
            property,
            inline_splice: None,
        }
    }

    pub fn module(&self) -> Option<PropertyId> {
        match self {
            Self::Module { property, .. } => Some(*property),
            Self::External { .. } => None,
        }
    }

    pub fn inline_splice(&self) -> Option<&FirInlineAccessorSplice> {
        match self {
            Self::Module { inline_splice, .. } => inline_splice.as_deref(),
            Self::External { .. } => None,
        }
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::Module { inline_splice, .. } => inline_splice.as_ref().map_or(0, |splice| {
                splice.substitutions.len() * std::mem::size_of::<FirInlineTypeSubstitution>()
            }),
            Self::External {
                receiver,
                parameters,
                ..
            } => {
                parameters.len() * std::mem::size_of::<ResolvedTy>()
                    + usize::from(receiver.is_some()) * std::mem::size_of::<ResolvedTy>()
            }
        }
    }
}

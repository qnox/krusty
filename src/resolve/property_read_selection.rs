//! Operations over an already-selected property read.
//!
//! Candidate collection and selection remain in the resolver facade. This boundary exposes only
//! the semantic type, visibility check, and external-property identity consumed after selection.

use super::{Origin, PropertyReadSelection, Ty, TypeName, Visibility};

impl PropertyReadSelection {
    pub(super) fn ty(&self) -> Ty {
        match self {
            Self::Member(member) => member.ty,
            Self::MemberExtension(property) => property.ty,
            Self::Extension(access, _) => access.property.ty,
        }
    }

    pub(super) fn access(&self) -> Option<(Visibility, TypeName)> {
        match self {
            Self::Member(member) => member.access,
            Self::MemberExtension(property) => Some((property.visibility, property.owner)),
            // ModuleSymbols is constructed for the current source file: it excludes private
            // extensions from every sibling file and admits the current file's declaration. Once
            // admitted, a second file-blind visibility check would reject the legal same-file read.
            Self::Extension(access, _)
                if matches!(access.property.getter.origin, Origin::Module { .. })
                    && (access.property.stable_declaration.is_some()
                        || access.property.source_key.is_some()) =>
            {
                None
            }
            Self::Extension(access, _) => Some((access.property.visibility, access.property.owner)),
        }
    }

    pub(super) fn external_property(&self) -> Option<crate::fir::ExternalPropertyId> {
        match self {
            Self::Member(member) => member
                .accessor
                .as_deref()
                .and_then(|accessor| accessor.external_property_identity),
            Self::MemberExtension(property) => property
                .getter
                .as_ref()
                .and_then(|getter| getter.external_property_identity),
            Self::Extension(access, _) => access.property.getter.external_property_identity,
        }
    }
}

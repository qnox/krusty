//! Operations over an already-selected property read.
//!
//! Candidate collection and selection remain in the resolver facade. This boundary exposes only
//! the semantic type, visibility check, and external-property identity consumed after selection.

use super::{
    Checker, Origin, PropertyReadSelection, ResolvedPropertyAccess, Ty, TypeName, Visibility,
};

pub(super) struct PropertyReadMemberSelection {
    pub(super) name: String,
    pub(super) ty: Ty,
    pub(super) owner: TypeName,
    pub(super) interface: bool,
    pub(super) getter: Option<crate::symbol_resolver::ResolvedMember>,
    pub(super) accessor: Option<Box<crate::libraries::LibraryCallable>>,
    /// Provider-chosen field, Kotlin accessor, or Java accessor. The platform null-check names it.
    pub(super) producer: crate::libraries::PropertyProducer,
    /// Provider-published constant-value expression fact for this selected read.
    pub(super) metadata_constant_read: bool,
    pub(super) context_access: Option<Box<ResolvedPropertyAccess>>,
    pub(super) compiler_intrinsic: Option<crate::libraries::CompilerIntrinsic>,
    pub(super) compile_time_constant: Option<crate::libraries::LibraryConst>,
    pub(super) source_member: Option<crate::libraries::SourceMember>,
    pub(super) stable_declaration: Option<crate::fir::DeclarationId>,
    pub(super) access: Option<(Visibility, TypeName)>,
    /// Whether a second read through the same receiver returns the same value.
    pub(super) stable_read: bool,
}

pub(super) enum PropertyReadAmbiguity {
    MemberExtension,
    Extension,
    MissingContext,
    /// Synthetic properties of one classifier declared by different accessor methods.
    Accessors(Vec<String>),
}

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

    /// A non-public read the visibility gate rejects. A member extension is gated on its dispatch
    /// receiver — the extension receiver is an ordinary argument — so `f.observable` inside the
    /// declaring class reads a `protected` member of `this`, not of `f`. Public reads stay visible.
    pub(super) fn hidden_from(
        &self,
        receiver: Ty,
        gate: impl FnOnce(Visibility, TypeName, Ty) -> bool,
    ) -> Option<(Visibility, TypeName)> {
        let (visibility, owner) = self.access()?;
        if visibility == Visibility::Public {
            return None;
        }
        let probed = match self {
            Self::MemberExtension(property) => property.dispatch_receiver.ty,
            Self::Member(_) | Self::Extension(..) => receiver,
        };
        (!gate(visibility, owner, probed)).then_some((visibility, owner))
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

impl Checker<'_> {
    pub(super) fn hidden_property_read(
        &self,
        selection: &PropertyReadSelection,
        receiver: Ty,
    ) -> Option<(Visibility, TypeName)> {
        selection.hidden_from(receiver, |visibility, owner, probed| {
            self.receiver_property_accessible(visibility, owner, probed)
        })
    }
}

impl Checker<'_> {
    /// The ambiguity a read reports when accessor methods of one classifier declare the same
    /// synthetic property. Candidates are listed the way kotlinc looks them up: the `isX` getter,
    /// then the `get` spellings.
    pub(super) fn competing_accessor_candidates(
        &self,
        name: &str,
        selected: &crate::symbol_resolver::SelectedMemberProperty,
    ) -> Option<Vec<String>> {
        if selected.competing_accessors.is_empty() {
            return None;
        }
        let candidates = selected
            .property
            .iter()
            .chain(&selected.competing_accessors)
            .collect::<Vec<_>>();
        Some(
            candidates
                .into_iter()
                .map(|candidate| {
                    let keyword = if candidate.setter.is_some() {
                        "var"
                    } else {
                        "val"
                    };
                    let ty = self.diagnostic_type_name(candidate.ty, &[candidate.ty]);
                    format!("{keyword} {name}: {ty}")
                })
                .collect(),
        )
    }

    pub(super) fn competing_accessor_ambiguity(
        &self,
        name: &str,
        selected: &crate::symbol_resolver::SelectedMemberProperty,
    ) -> super::PropertyReadAmbiguity {
        super::PropertyReadAmbiguity::Accessors(
            self.competing_accessor_candidates(name, selected)
                .expect("an accessor ambiguity has competing candidates"),
        )
    }

    pub(super) fn report_accessor_ambiguity(&mut self, span: super::Span, candidates: Vec<String>) {
        let mut message = "overload resolution ambiguity between candidates:".to_string();
        for candidate in candidates {
            message.push('\n');
            message.push_str(&candidate);
        }
        self.diags.error(span, message);
    }
}

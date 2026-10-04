//! A classifier's resolved header and the declaration order of its direct supertypes.
//!
//! Kotlin walks a classifier's supertypes in the order they are written, and that order decides
//! the order of inherited members and bridges. The resolved header keeps the superclass apart from
//! the interfaces, so it also records the superclass's slot among them.

use super::super::{HeaderSyntaxArena, HeaderTypeId, ResolvedTy};
use super::{
    DeclarationId, ResolvedClassifierContextParameter, ResolvedModuleIndex, UnpublishableType,
};
use crate::fir::ResolvedInterfaceDelegation;
use crate::types::{Ty, TypeName};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedClassifierHeader {
    pub declaration: DeclarationId,
    pub classifier: TypeName,
    pub superclass: Option<ResolvedTy>,
    /// How many of [`Self::interfaces`] the declaration lists before its superclass.
    interfaces_before_superclass: u32,
    pub interfaces: Box<[ResolvedTy]>,
    pub interface_delegations: Box<[ResolvedInterfaceDelegation]>,
    /// Constructor-supplied implicit receivers available to every instance body, in source order.
    pub context_parameters: Box<[ResolvedClassifierContextParameter]>,
    /// Closed direct subclass identities for a sealed classifier. Pass 1 computes this from stable
    /// declarations; common-IR lowering copies it without reopening a symbol table.
    pub sealed_subclasses: Box<[TypeName]>,
}

/// A classifier's resolved superclass and how many declared interfaces precede it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DeclaredSuperclass {
    pub(crate) ty: Ty,
    pub(crate) interfaces_before: u32,
}

impl DeclaredSuperclass {
    pub(crate) fn after(ty: Ty, interfaces_before: usize) -> Self {
        Self {
            ty,
            interfaces_before: u32::try_from(interfaces_before)
                .expect("a classifier's interface count fits u32"),
        }
    }
}

/// How many of `supertypes` are written before the superclass: `base` when it is written with a
/// constructor call, else the one at ordinal `listed`. An implicit superclass comes first.
pub(crate) fn superclass_slot(
    syntax: &HeaderSyntaxArena,
    supertypes: &[HeaderTypeId],
    base: Option<HeaderTypeId>,
    listed: Option<usize>,
) -> Option<usize> {
    let Some(base) = base else {
        return Some(listed.unwrap_or(0));
    };
    let base_start = syntax.ty(base)?.span.lo;
    supertypes.iter().try_fold(0, |before, supertype| {
        Some(before + usize::from(syntax.ty(*supertype)?.span.lo < base_start))
    })
}

impl ResolvedClassifierHeader {
    /// The interfaces the declaration lists before its superclass.
    pub(crate) fn interfaces_written_before_superclass(&self) -> &[ResolvedTy] {
        &self.interfaces[..self.interfaces_before_superclass as usize]
    }

    /// The direct supertypes in declaration order: the superclass in its written slot among the
    /// interfaces.
    pub(crate) fn declared_supertypes(&self) -> impl Iterator<Item = ResolvedTy> + '_ {
        let (before, after) = self
            .interfaces
            .split_at(self.interfaces_written_before_superclass().len());
        before
            .iter()
            .copied()
            .chain(self.superclass)
            .chain(after.iter().copied())
    }
}

impl ResolvedModuleIndex {
    pub(crate) fn publish_classifier_header(
        &mut self,
        declaration: DeclarationId,
        classifier: TypeName,
        superclass: Option<DeclaredSuperclass>,
        interfaces: impl IntoIterator<Item = Ty>,
        interface_delegations: impl IntoIterator<Item = ResolvedInterfaceDelegation>,
        context_parameters: impl IntoIterator<
            Item = (Option<Box<str>>, crate::types::ContextParameterKind, Ty),
        >,
        sealed_subclasses: impl IntoIterator<Item = TypeName>,
    ) -> Result<(), UnpublishableType> {
        let interfaces_before_superclass = superclass.map_or(0, |parent| parent.interfaces_before);
        let superclass = superclass
            .map(|parent| ResolvedTy::new(parent.ty))
            .transpose()?;
        let interfaces = interfaces
            .into_iter()
            .map(ResolvedTy::new)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        assert!(
            interfaces_before_superclass as usize <= interfaces.len(),
            "a superclass slot must lie within the declared interfaces"
        );
        let interface_delegations = interface_delegations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let context_parameters = context_parameters
            .into_iter()
            .map(|(name, kind, ty)| {
                Ok(ResolvedClassifierContextParameter {
                    name,
                    kind,
                    ty: ResolvedTy::new(ty)?,
                })
            })
            .collect::<Result<Vec<_>, UnpublishableType>>()?
            .into_boxed_slice();
        let sealed_subclasses = sealed_subclasses
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.publish_classifier_identity(declaration, classifier);
        assert!(
            self.classifiers
                .insert(
                    declaration,
                    ResolvedClassifierHeader {
                        declaration,
                        classifier,
                        superclass,
                        interfaces_before_superclass,
                        interfaces,
                        interface_delegations,
                        context_parameters,
                        sealed_subclasses,
                    },
                )
                .is_none(),
            "a stable classifier may publish only one semantic header"
        );
        Ok(())
    }
}

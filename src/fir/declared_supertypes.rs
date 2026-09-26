//! Declaration order of a classifier's direct supertypes.
//!
//! Kotlin walks a classifier's supertypes in the order they are written, and that order decides
//! the order of inherited members and bridges. The resolved header keeps the superclass apart from
//! the interfaces, so it also records the superclass's slot among them.

use super::{HeaderSyntaxArena, HeaderTypeId, ResolvedClassifierHeader, ResolvedTy};
use crate::types::Ty;

/// A classifier's resolved superclass and how many declared interfaces precede it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclaredSuperclass {
    pub ty: Ty,
    pub interfaces_before: u32,
}

impl DeclaredSuperclass {
    pub fn after(ty: Ty, interfaces_before: usize) -> Self {
        Self {
            ty,
            interfaces_before: u32::try_from(interfaces_before)
                .expect("a classifier's interface count fits u32"),
        }
    }
}

/// How many of `supertypes` are written before the superclass: `base` when it is written with a
/// constructor call, else the one at ordinal `listed`. An implicit superclass comes first.
pub fn superclass_slot(
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
    /// The direct supertypes in declaration order: the superclass in its written slot among the
    /// interfaces.
    pub fn declared_supertypes(&self) -> impl Iterator<Item = ResolvedTy> + '_ {
        let (before, after) = self
            .interfaces
            .split_at(self.interfaces_before_superclass as usize);
        before
            .iter()
            .copied()
            .chain(self.superclass)
            .chain(after.iter().copied())
    }
}

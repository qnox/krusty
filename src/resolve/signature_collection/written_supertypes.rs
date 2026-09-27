//! The written supertype list of one source classifier, split into its superclass entry and its
//! nominal interfaces in declaration order.
//!
//! kotlinc walks a classifier's direct supertypes in the order they are written, superclass
//! included, so the split keeps the superclass's slot among the interfaces rather than only its
//! identity.

use crate::ast::{ClassDecl, CtorDelegation, TypeRef};
use crate::resolve::StreamedClassifierHeader;

pub(in crate::resolve) struct WrittenSupertypes<'a> {
    /// `: Base(..)`, or a parenless base the declaration's constructors make a superclass.
    pub(in crate::resolve) superclass: Option<&'a TypeRef>,
    /// The nominal interfaces, in written order.
    pub(in crate::resolve) interfaces: Vec<&'a TypeRef>,
    /// How many of [`Self::interfaces`] the declaration lists before [`Self::superclass`].
    pub(in crate::resolve) interfaces_before_superclass: u32,
}

impl<'a> WrittenSupertypes<'a> {
    /// Splits `header`'s supertypes. `is_class` classifies a written entry that may be a parenless
    /// base; it is asked only when the declaration has no primary constructor call to its base and
    /// a secondary constructor delegates to `super`.
    pub(in crate::resolve) fn split(
        class: &ClassDecl,
        header: &'a StreamedClassifierHeader,
        mut is_class: impl FnMut(&TypeRef) -> bool,
    ) -> Self {
        // The parser can promote only a SAME-FILE base; at this all-files signature pass, the
        // caller classifies an other-file module declaration or a library classifier, and both
        // origins feed the same superclass entry.
        let parenless = if class.primary_ctor_annotations.is_some()
            || header.base.is_some()
            || !class.secondary_ctors.iter().any(|constructor| {
                matches!(
                    constructor.delegation,
                    CtorDelegation::Super(_) | CtorDelegation::None
                )
            }) {
            None
        } else {
            header
                .supertypes
                .iter()
                .find(|&supertype| is_class(supertype))
        };
        let parenless_name = parenless.map(|base| base.name.as_str());
        let interfaces = header
            .supertypes
            .iter()
            // A function supertype through the numbered semantic classifier contributes both its
            // nominal `kotlin/FunctionN` edge and its exact callable shape. Only the unmaterialized
            // `<fun>` marker used by suspend/big-arity shapes lacks a nominal classifier and must
            // stay out of hierarchy traversal.
            .filter(|t| t.name != "<fun>")
            .filter(|t| parenless_name != Some(t.name.as_str()))
            .collect::<Vec<_>>();
        let superclass = header.base.as_ref().or(parenless);
        let before = superclass.map_or(0, |base| {
            interfaces
                .iter()
                .filter(|t| t.span.lo < base.span.lo)
                .count()
        });
        Self {
            superclass,
            interfaces,
            interfaces_before_superclass: u32::try_from(before)
                .expect("a classifier's interface count fits u32"),
        }
    }
}

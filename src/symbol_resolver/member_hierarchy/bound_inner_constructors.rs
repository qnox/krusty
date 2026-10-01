//! Constructors of an `inner` classifier as members of the receiver that supplies its outer
//! instance.
//!
//! Kotlin's member level for a receiver holds its member functions together with the
//! constructors of the inner classifiers it exposes, so `outer.Inner(args)` selects over both at
//! once. Core owns this normalization so the body checker and signature inference collect the same
//! candidate family from the same declarations.

use crate::libraries::{FunctionInfo, LibraryMember, LibraryType};
use crate::symbol_resolver::{
    applied_hierarchy, classifier_bindings, member_scope_receiver, specialize_member_function,
};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// `constructors` of the inner classifier `inner` as member candidates of `receiver`. The outer
/// classifier's type parameters are specialized from the receiver's application of it, and each
/// candidate sits at the receiver rank of that outer classifier, exactly as a member function
/// declared there would.
pub(crate) fn bound_inner_constructor_candidates(
    source: &dyn SymbolSource,
    receiver: Ty,
    inner: TypeName,
    classifier: &LibraryType,
    constructors: impl IntoIterator<Item = LibraryMember>,
) -> Vec<FunctionInfo> {
    let Some(outer) = classifier.outer_instance else {
        return Vec::new();
    };
    let Some((applied_outer, rank)) = applied_hierarchy(source, member_scope_receiver(receiver))
        .into_iter()
        .find(|(owner, ..)| *owner == outer)
        .map(|(_, applied, depth)| (applied, depth))
    else {
        return Vec::new();
    };
    let rank = u32::try_from(rank).expect("receiver hierarchy depth exceeds callable rank");
    let bindings = source
        .classifier(outer)
        .map(|outer| classifier_bindings(&outer, applied_outer))
        .unwrap_or_default();
    constructors
        .into_iter()
        .filter_map(|constructor| {
            let mut candidate =
                FunctionInfo::receiver_bound_constructor(inner, classifier, constructor)?;
            specialize_member_function(source, receiver, &mut candidate, &bindings);
            candidate.receiver_rank = rank;
            Some(candidate)
        })
        .collect()
}

impl crate::symbol_resolver::SymbolResolver<'_> {
    /// The provider constructors of the inner classifier `inner` as member candidates of
    /// `receiver`.
    pub(crate) fn bound_inner_constructor_candidates(
        &self,
        receiver: Ty,
        inner: TypeName,
    ) -> Vec<FunctionInfo> {
        let Some(classifier) = self.src.classifier(inner) else {
            return Vec::new();
        };
        bound_inner_constructor_candidates(
            &self.src,
            receiver,
            inner,
            &classifier,
            classifier.constructors.iter().cloned(),
        )
    }
}

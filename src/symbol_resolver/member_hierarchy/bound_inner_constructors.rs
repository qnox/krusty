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

/// The receiver's already-selected application of the classifier that owns an inner instance.
/// Captured formals are fixed evidence, while the constructor's own formals remain inferable.
pub(crate) struct BoundInnerConstructorContext {
    outer: Ty,
    bindings: crate::symbol_resolver::GSigBinds,
    rank: u32,
}

impl BoundInnerConstructorContext {
    pub(crate) fn new(
        source: &dyn SymbolSource,
        receiver: Ty,
        classifier: &LibraryType,
    ) -> Option<Self> {
        let outer = classifier.outer_instance?;
        let (_, applied, rank) = applied_hierarchy(source, member_scope_receiver(receiver))
            .into_iter()
            .find(|(owner, ..)| *owner == outer)?;
        let outer_declaration = source.classifier(outer)?;
        Some(Self {
            outer: applied,
            bindings: classifier_bindings(&outer_declaration, applied),
            rank: u32::try_from(rank).expect("receiver hierarchy depth exceeds callable rank"),
        })
    }

    pub(crate) fn outer(&self) -> Ty {
        self.outer
    }

    pub(crate) fn bindings(&self) -> &crate::symbol_resolver::GSigBinds {
        &self.bindings
    }

    pub(crate) fn candidate(
        &self,
        source: &dyn SymbolSource,
        receiver: Ty,
        inner: TypeName,
        classifier: &LibraryType,
        constructor: LibraryMember,
    ) -> Option<FunctionInfo> {
        let mut candidate =
            FunctionInfo::receiver_bound_constructor(inner, classifier, constructor)?;
        specialize_member_function(source, receiver, &mut candidate, &self.bindings);
        if let Some(signature) = &mut candidate.generic_sig {
            // Receiver specialization changes declared bounds; refresh symbolic occurrences too
            // so ordinary argument contextualization sees the same bound as the constraint solver.
            let formals = signature
                .formals
                .iter()
                .zip(&signature.formal_bounds)
                .map(|(formal, bounds)| {
                    let bound = bounds
                        .first()
                        .copied()
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                    (formal.clone(), Ty::ty_param(formal, bound))
                })
                .collect();
            signature.params = signature
                .params
                .iter()
                .map(|parameter| crate::types::ty_subst_applied_arguments(*parameter, &formals))
                .collect();
            signature.ret = crate::types::ty_subst_applied_arguments(signature.ret, &formals);
            candidate.callable.ret = signature.ret;
            candidate.callable.params = signature.params.clone();
            candidate.ret.class = Some(signature.ret);
        }
        candidate.receiver_rank = self.rank;
        Some(candidate)
    }
}

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
    let Some(context) = BoundInnerConstructorContext::new(source, receiver, classifier) else {
        return Vec::new();
    };
    constructors
        .into_iter()
        .filter_map(|constructor| {
            context.candidate(source, receiver, inner, classifier, constructor)
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

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptySource;
    impl SymbolSource for EmptySource {}

    #[test]
    fn a_bound_constructor_publishes_the_same_complete_application_on_every_result_facet() {
        let outer = crate::types::type_name("fixtures/Outer");
        let inner = crate::types::type_name("fixtures/Outer$Inner");
        let mut classifier = LibraryType::declaration_header();
        classifier.outer_instance = Some(outer);
        classifier.own_type_parameter_count = 1;
        let number = Ty::obj("kotlin/Number");
        let captured = Ty::ty_param("outer:O", number);
        let own = Ty::ty_param("inner:T", captured);
        classifier.type_parameters = crate::types::TypeParameters::invariant(
            vec!["inner:T".into(), "outer:O".into()],
            vec![vec![captured], vec![number]],
        );
        let mut constructor = LibraryMember::new("<init>".into(), vec![own], Ty::Unit, "".into());
        constructor.generic_sig = Some(crate::libraries::GenericSig {
            formals: vec!["inner:T".into()],
            formal_bounds: vec![vec![captured]],
            receiver: None,
            params: vec![own],
            ret: Ty::obj_args_name(inner, &[own]),
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        });
        let applied_outer = Ty::obj_args_name(outer, &[Ty::Int]);
        let context = BoundInnerConstructorContext {
            outer: applied_outer,
            bindings: [("outer:O".into(), Ty::Int)].into_iter().collect(),
            rank: 0,
        };
        let candidate = context
            .candidate(&EmptySource, applied_outer, inner, &classifier, constructor)
            .expect("an inner constructor");
        let expected = Ty::obj_args_name(inner, &[Ty::ty_param("inner:T", Ty::Int), Ty::Int]);
        assert_eq!(candidate.semantic_signature().ret, expected);
        assert_eq!(candidate.callable.ret, expected);
        assert_eq!(candidate.ret.apply(expected), expected);
        assert_eq!(
            candidate.semantic_signature().formal_bounds,
            [vec![Ty::Int]]
        );
        assert_eq!(
            candidate.semantic_signature().params,
            [Ty::ty_param("inner:T", Ty::Int)]
        );
    }
}

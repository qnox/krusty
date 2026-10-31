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
    input_bounds: crate::libraries::ReceiverInputBounds,
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
        let captured = &classifier.type_params()[classifier.own_type_parameter_count..];
        let outer_bindings = classifier_bindings(&outer_declaration, applied);
        // The inner declaration owns the captured slots. A provider renames a captured formal
        // hidden by an own formal in a complete enclosing layout. A selective capture layout
        // instead retains the captured declarations' semantic identities. An unapplied enclosing
        // type contributes only its existing bindings, without inventing argument evidence.
        let identities: crate::fx_hash::HashMap<&str, &str> =
            if captured.len() == outer_declaration.type_params().len() {
                captured
                    .iter()
                    .zip(outer_declaration.type_params())
                    .map(|(capture, formal)| (formal.as_str(), capture.as_str()))
                    .collect()
            } else {
                captured
                    .iter()
                    .map(|capture| (capture.as_str(), capture.as_str()))
                    .collect()
            };
        let bindings = identities
            .iter()
            .filter_map(|(&formal, &capture)| {
                outer_bindings
                    .get(formal)
                    .map(|&actual| (capture.to_owned(), actual))
            })
            .collect();
        let renamed = identities
            .iter()
            .map(|(&formal, &capture)| (formal, crate::types::intern(capture)))
            .collect();
        let input_bounds = crate::libraries::ReceiverInputBounds::from_declared(
            outer_declaration
                .type_params()
                .iter()
                .zip(outer_declaration.type_param_bounds())
                .filter_map(|(formal, bounds)| {
                    let capture = identities.get(formal.as_str())?;
                    Some((
                        (*capture).to_owned(),
                        bounds
                            .iter()
                            .map(|&bound| crate::types::ty_rename_params(bound, &renamed))
                            .collect(),
                    ))
                }),
        );
        Some(Self {
            outer: applied,
            bindings,
            input_bounds,
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
        specialize_member_function(
            source,
            receiver,
            &mut candidate,
            &self.bindings,
            &self.input_bounds,
        );
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
    fn captured_receiver_frame_renames_every_bound_component_with_its_formal() {
        struct OuterSource(TypeName, std::sync::Arc<LibraryType>);
        impl SymbolSource for OuterSource {
            fn classifier(&self, owner: TypeName) -> Option<std::sync::Arc<LibraryType>> {
                (owner == self.0).then(|| self.1.clone())
            }
        }
        let outer = crate::types::type_name("sample/BoundedOuter");
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let formal = Ty::ty_param("owner:T", any);
        let bound = Ty::obj_args("sample/Recursive", &[formal]);
        let marker = Ty::obj("sample/Marker");
        let mut declaration = LibraryType::declaration_header();
        declaration.own_type_parameter_count = 1;
        declaration.type_parameters = crate::types::TypeParameters::invariant(
            vec!["owner:T".into()],
            vec![vec![bound, marker]],
        );
        let source = OuterSource(outer, std::sync::Arc::new(declaration));
        let mut inner = LibraryType::declaration_header();
        inner.outer_instance = Some(outer);
        inner.own_type_parameter_count = 1;
        inner.type_parameters = crate::types::TypeParameters::invariant(
            vec!["inner:T".into(), "captured:T".into()],
            vec![vec![any], vec![any]],
        );
        let context = BoundInnerConstructorContext::new(
            &source,
            Ty::obj_args_name(outer, &[Ty::out_projection(Ty::String)]),
            &inner,
        )
        .unwrap();
        let renamed = Ty::obj_args("sample/Recursive", &[Ty::ty_param("captured:T", any)]);
        assert_eq!(
            context.input_bounds.get("captured:T"),
            Some([renamed, marker].as_slice())
        );
        assert_eq!(context.input_bounds.get("owner:T"), None);
        assert_eq!(
            context.bindings().get("captured:T"),
            Some(&Ty::out_projection(Ty::String))
        );
    }

    #[test]
    fn captured_bindings_use_the_inner_layout_when_an_own_formal_shadows_the_outer() {
        struct OuterSource {
            owner: TypeName,
            declaration: std::sync::Arc<LibraryType>,
        }
        impl SymbolSource for OuterSource {
            fn classifier(&self, owner: TypeName) -> Option<std::sync::Arc<LibraryType>> {
                (owner == self.owner).then(|| self.declaration.clone())
            }
        }
        let outer = crate::types::type_name("fixtures/ShadowOuter");
        let inner = crate::types::type_name("fixtures/ShadowOuter$Inner");
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let mut outer_declaration = LibraryType::declaration_header();
        outer_declaration.own_type_parameter_count = 1;
        outer_declaration.type_parameters =
            crate::types::TypeParameters::invariant(vec!["T".into()], vec![vec![any]]);
        let source = OuterSource {
            owner: outer,
            declaration: std::sync::Arc::new(outer_declaration),
        };
        let mut classifier = LibraryType::declaration_header();
        classifier.outer_instance = Some(outer);
        classifier.own_type_parameter_count = 1;
        classifier.type_parameters = crate::types::TypeParameters::invariant(
            vec!["T".into(), "captured:T".into()],
            vec![vec![any], vec![any]],
        );
        let receiver = Ty::obj_args_name(outer, &[Ty::String]);
        let context = BoundInnerConstructorContext::new(&source, receiver, &classifier)
            .expect("an applied enclosing instance");
        let raw = BoundInnerConstructorContext::new(&source, Ty::obj_name(outer), &classifier)
            .expect("an unapplied enclosing instance retains its partial context");
        assert_eq!(
            raw.bindings(),
            &crate::symbol_resolver::GSigBinds::default()
        );
        assert_eq!(
            context.bindings(),
            &crate::symbol_resolver::GSigBinds::from_iter([("captured:T".into(), Ty::String),])
        );
        let own = Ty::ty_param("T", any);
        let constructor = LibraryMember::new("<init>".into(), vec![own], Ty::Unit, "".into());
        let candidate = context
            .candidate(&source, receiver, inner, &classifier, constructor)
            .expect("a normalized constructor");
        assert_eq!(candidate.semantic_signature().formals, ["T"]);
        assert_eq!(candidate.semantic_signature().params, [own]);
        assert_eq!(
            candidate.semantic_signature().ret,
            Ty::obj_args_name(inner, &[own, Ty::String])
        );
        let mut inferred = crate::symbol_resolver::GSigBinds::from_iter([("T".into(), Ty::Int)]);
        inferred.extend(context.bindings().clone());
        assert_eq!(
            inferred,
            crate::symbol_resolver::GSigBinds::from_iter([
                ("T".into(), Ty::Int),
                ("captured:T".into(), Ty::String),
            ])
        );
        let mut selective = classifier.clone();
        selective.type_parameters =
            crate::types::TypeParameters::invariant(vec!["inner:T".into()], vec![vec![any]]);
        let context = BoundInnerConstructorContext::new(&source, receiver, &selective)
            .expect("an inner declaration may omit an unused enclosing formal");
        assert_eq!(
            context.bindings(),
            &crate::symbol_resolver::GSigBinds::default()
        );
        let mut outer_declaration = LibraryType::declaration_header();
        outer_declaration.own_type_parameter_count = 2;
        outer_declaration.type_parameters = crate::types::TypeParameters::invariant(
            vec!["A".into(), "B".into()],
            vec![vec![any], vec![any]],
        );
        let source = OuterSource {
            owner: outer,
            declaration: std::sync::Arc::new(outer_declaration),
        };
        selective.type_parameters = crate::types::TypeParameters::invariant(
            vec!["inner:T".into(), "B".into()],
            vec![vec![any], vec![any]],
        );
        let context = BoundInnerConstructorContext::new(
            &source,
            Ty::obj_args_name(outer, &[Ty::Int, Ty::String]),
            &selective,
        )
        .expect("a selective capture keeps its original declaration identity");
        assert_eq!(
            context.bindings(),
            &crate::symbol_resolver::GSigBinds::from_iter([("B".into(), Ty::String),])
        );
    }

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
            input_bounds: crate::libraries::ReceiverInputBounds::from_declared([(
                "outer:O".into(),
                vec![number],
            )]),
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

//! The identity boundary between a dependency constructor and its constructed classifier.
//!
//! A constructor inference signature owns every variable used while checking its parameters and
//! bounds. Its classifier-result variables are the declaration-order prefix; any remaining
//! variables belong only to the constructor. The constructed type and constructor evidence,
//! however, are classifier facts. This module performs that one positional handoff without
//! comparing source spellings.

use crate::fx_hash::{HashMap, HashSet};
use crate::libraries::GenericSig;
use crate::symbol_resolver::GSigBinds;
use crate::types::{Ty, TypeName};

pub(super) struct LibraryConstructorTypeParameters<'a> {
    classifier_formals: &'a [String],
    signature_formals: &'a [String],
    signature_bounds: &'a [Vec<Ty>],
}

impl<'a> LibraryConstructorTypeParameters<'a> {
    pub(super) fn new(classifier_formals: &'a [String], signature: &'a GenericSig) -> Self {
        let count = classifier_formals.len();
        assert!(
            signature.formals.len() >= count && signature.formal_bounds.len() >= count,
            "a dependency constructor signature must retain its classifier-formal prefix"
        );
        Self {
            classifier_formals,
            signature_formals: &signature.formals[..count],
            signature_bounds: &signature.formal_bounds[..count],
        }
    }

    pub(super) fn signature_formals(&self) -> &[String] {
        self.signature_formals
    }

    pub(super) fn signature_bounds(&self) -> &[Vec<Ty>] {
        self.signature_bounds
    }

    /// Fix written classifier arguments in the constructor signature's own constraint space.
    pub(super) fn bind_explicit(&self, explicit: &[Ty], bindings: &mut GSigBinds) {
        for (formal, explicit) in self.signature_formals.iter().zip(explicit.iter().copied()) {
            if !explicit.mentions_error() && !explicit.mentions_pending() {
                bindings.insert(formal.clone(), explicit);
            }
        }
    }

    /// Complete classifier variables that supplied context to a postponed producer. Bounds and
    /// substitutions stay in the constructor signature's identity space until result publication.
    pub(super) fn complete_enclosing_context(
        &self,
        enclosing_context_formals: &HashSet<String>,
        bindings: &mut GSigBinds,
    ) {
        for (index, formal) in self.signature_formals.iter().enumerate() {
            if !enclosing_context_formals.contains(formal)
                || bindings
                    .get(formal)
                    .is_some_and(|binding| *binding != Ty::Error)
            {
                continue;
            }
            let known = self
                .signature_formals
                .iter()
                .take(index)
                .filter_map(|known_formal| {
                    bindings
                        .get(known_formal)
                        .copied()
                        .filter(|binding| *binding != Ty::Error)
                        .map(|binding| (known_formal.clone(), binding))
                })
                .collect::<HashMap<_, _>>();
            if let Some(bound) = self
                .signature_bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .filter(|bound| *bound != Ty::Error)
            {
                bindings.insert(
                    formal.clone(),
                    crate::symbol_resolver::ty_subst_keep_unbound(bound, &known),
                );
            }
        }
    }

    /// Produce the constructed classifier's arguments and remove constructor-owned identities
    /// from any still-symbolic completed bound before that application is published.
    pub(super) fn completed_result_arguments(
        &self,
        owner: TypeName,
        bindings: &GSigBinds,
    ) -> Vec<Ty> {
        let completed = crate::symbol_resolver::completed_constructor_result_arguments(
            owner,
            self.signature_formals,
            self.signature_bounds,
            bindings,
        );
        let renames = self.signature_to_classifier_renames();
        completed
            .into_iter()
            .map(|argument| crate::types::ty_rename_params(argument, &renames))
            .collect()
    }

    /// Re-home inferred classifier-variable bindings for classifier-owned evidence and later
    /// consumers. Constructor-only suffix variables deliberately do not cross this boundary.
    pub(super) fn classifier_bindings(&self, bindings: &GSigBinds) -> GSigBinds {
        let renames = self.signature_to_classifier_renames();
        self.classifier_formals
            .iter()
            .zip(self.signature_formals)
            .filter_map(|(classifier, signature)| {
                bindings.get(signature).copied().map(|binding| {
                    (
                        classifier.clone(),
                        crate::types::ty_rename_params(binding, &renames),
                    )
                })
            })
            .collect()
    }

    fn signature_to_classifier_renames(&self) -> crate::fx_hash::HashMap<&str, &'static str> {
        self.signature_formals
            .iter()
            .zip(self.classifier_formals)
            .map(|(signature, classifier)| {
                (
                    signature.as_str(),
                    crate::types::intern(classifier.as_str()),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::GenericReturnPolicy;

    fn signature() -> GenericSig {
        let class = "dependency-callable-7-class-T".to_owned();
        let constructor = "dependency-callable-7-constructor-T".to_owned();
        GenericSig {
            formals: vec![class.clone(), constructor.clone()],
            formal_bounds: vec![
                vec![Ty::nullable(Ty::obj("kotlin/Any"))],
                vec![Ty::obj("kotlin/Number")],
            ],
            receiver: None,
            params: vec![Ty::ty_param(&class, Ty::nullable(Ty::obj("kotlin/Any")))],
            ret: Ty::obj_args(
                "fixture/Box",
                &[Ty::ty_param(&class, Ty::nullable(Ty::obj("kotlin/Any")))],
            ),
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    #[test]
    fn classifier_coordinate_is_the_signature_prefix_not_a_same_spelled_suffix() {
        let signature = signature();
        let classifier = vec!["classifier-T".to_owned()];
        let parameters = LibraryConstructorTypeParameters::new(&classifier, &signature);

        assert_eq!(
            parameters.signature_formals(),
            ["dependency-callable-7-class-T"]
        );
    }

    #[test]
    fn explicit_classifier_argument_binds_the_signature_prefix() {
        let signature = signature();
        let classifier = vec!["classifier-T".to_owned()];
        let parameters = LibraryConstructorTypeParameters::new(&classifier, &signature);
        let mut bindings = GSigBinds::default();

        parameters.bind_explicit(&[Ty::String], &mut bindings);

        assert_eq!(
            bindings.get("dependency-callable-7-class-T"),
            Some(&Ty::String)
        );
        assert!(!bindings.contains_key("dependency-callable-7-constructor-T"));
        assert!(!bindings.contains_key("classifier-T"));
    }

    #[test]
    fn expected_result_seeds_the_signature_prefix() {
        let signature = signature();
        let classifier = vec!["classifier-T".to_owned()];
        let parameters = LibraryConstructorTypeParameters::new(&classifier, &signature);
        let owner = crate::types::type_name("fixture/Box");
        let mut bindings = GSigBinds::default();

        crate::symbol_resolver::seed_unbound_constructor_result_from_symbols(
            &crate::libraries::EmptySymbolSource,
            owner,
            parameters.signature_formals(),
            parameters.signature_bounds(),
            Some(Ty::obj_args_name(owner, &[Ty::String])),
            &mut bindings,
        );

        assert_eq!(
            bindings.get("dependency-callable-7-class-T"),
            Some(&Ty::String)
        );
        assert!(!bindings.contains_key("classifier-T"));
        assert!(!bindings.contains_key("dependency-callable-7-constructor-T"));
    }

    #[test]
    fn postponed_context_completion_uses_signature_coordinates_and_bounds() {
        let class = "dependency-callable-7-class-T".to_owned();
        let dependent = "dependency-callable-7-class-U".to_owned();
        let signature = GenericSig {
            formals: vec![class.clone(), dependent.clone()],
            formal_bounds: vec![
                vec![Ty::obj("kotlin/CharSequence")],
                vec![Ty::obj_args(
                    "fixture/Base",
                    &[Ty::ty_param(&class, Ty::obj("kotlin/CharSequence"))],
                )],
            ],
            receiver: None,
            params: Vec::new(),
            ret: Ty::Unit,
            return_policy: GenericReturnPolicy::Exact,
        };
        let classifier = vec!["classifier-T".to_owned(), "classifier-U".to_owned()];
        let parameters = LibraryConstructorTypeParameters::new(&classifier, &signature);
        let mut bindings = GSigBinds::from_iter([(class, Ty::String)]);
        let contextual = HashSet::from_iter([dependent.clone()]);

        parameters.complete_enclosing_context(&contextual, &mut bindings);

        assert_eq!(
            bindings.get(&dependent),
            Some(&Ty::obj_args("fixture/Base", &[Ty::String]))
        );
    }

    #[test]
    fn completed_result_and_evidence_are_rehomed_to_classifier_identities() {
        let signature = signature();
        let classifier = vec!["classifier-T".to_owned()];
        let parameters = LibraryConstructorTypeParameters::new(&classifier, &signature);
        let signature_formal = &signature.formals[0];
        let symbolic = Ty::ty_param(signature_formal, Ty::nullable(Ty::obj("kotlin/Any")));
        let bindings = GSigBinds::from_iter([(signature_formal.clone(), symbolic)]);

        let result = parameters
            .completed_result_arguments(crate::types::type_name("fixture/Box"), &bindings);
        let published = parameters.classifier_bindings(&bindings);
        let expected = Ty::ty_param("classifier-T", Ty::nullable(Ty::obj("kotlin/Any")));

        assert_eq!(result, [expected]);
        assert_eq!(published.get("classifier-T"), Some(&expected));
        assert!(!published.contains_key(signature_formal));
    }
}

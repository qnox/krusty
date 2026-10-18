//! The identity boundary between a constructor signature and its constructed classifier.
//!
//! A constructor inference signature owns every variable used while checking its parameters and
//! bounds. Its classifier-result variables are the declaration-order prefix; any remaining
//! variables belong only to the constructor. Classifier applications and constructor evidence are
//! classifier-owned facts. This positional adapter is the only handoff between those identities;
//! source spelling does not participate.

use crate::fx_hash::{HashMap, HashSet};
use crate::types::Ty;

use super::GenericSig;

pub(crate) struct ConstructorTypeParameterCoordinates<'a> {
    classifier_formals: &'a [String],
    signature_formals: &'a [String],
    signature_bounds: &'a [Vec<Ty>],
}

impl<'a> ConstructorTypeParameterCoordinates<'a> {
    pub(crate) fn new(classifier_formals: &'a [String], signature: &'a GenericSig) -> Self {
        let count = classifier_formals.len();
        assert!(
            signature.formals.len() >= count && signature.formal_bounds.len() >= count,
            "a constructor signature must retain its classifier-formal prefix"
        );
        Self {
            classifier_formals,
            signature_formals: &signature.formals[..count],
            signature_bounds: &signature.formal_bounds[..count],
        }
    }

    pub(crate) fn signature_formals(&self) -> &[String] {
        self.signature_formals
    }

    pub(crate) fn signature_bounds(&self) -> &[Vec<Ty>] {
        self.signature_bounds
    }

    /// Fix written classifier arguments in the constructor signature's own constraint space.
    pub(crate) fn bind_explicit(&self, explicit: &[Ty], bindings: &mut HashMap<String, Ty>) {
        for (formal, explicit) in self.signature_formals.iter().zip(explicit.iter().copied()) {
            if !explicit.mentions_error() && !explicit.mentions_pending() {
                bindings.insert(formal.clone(), explicit);
            }
        }
    }

    /// Complete classifier variables that supplied context to a postponed producer. Bounds and
    /// substitutions stay in the constructor signature's identity space until result publication.
    pub(crate) fn complete_enclosing_context(
        &self,
        enclosing_context_formals: &HashSet<String>,
        bindings: &mut HashMap<String, Ty>,
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
                    crate::types::ty_subst_keep_unbound(bound, &known),
                );
            }
        }
    }

    /// Re-home types before publishing a classifier application.
    pub(crate) fn classifier_types(&self, types: impl IntoIterator<Item = Ty>) -> Vec<Ty> {
        let renames = self.signature_to_classifier_renames();
        types
            .into_iter()
            .map(|ty| crate::types::ty_rename_params(ty, &renames))
            .collect()
    }

    /// Re-home inferred classifier-variable bindings for classifier-owned results and evidence.
    /// Constructor-only suffix variables deliberately do not cross this boundary.
    pub(crate) fn classifier_bindings(
        &self,
        bindings: &HashMap<String, Ty>,
    ) -> HashMap<String, Ty> {
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

    /// Enter the signature constraint space from classifier-owned result bindings.
    pub(crate) fn signature_bindings(&self, bindings: &HashMap<String, Ty>) -> HashMap<String, Ty> {
        let renames = self.classifier_to_signature_renames();
        self.signature_formals
            .iter()
            .zip(self.classifier_formals)
            .filter_map(|(signature, classifier)| {
                bindings.get(classifier).copied().map(|binding| {
                    (
                        signature.clone(),
                        crate::types::ty_rename_params(binding, &renames),
                    )
                })
            })
            .collect()
    }

    fn signature_to_classifier_renames(&self) -> HashMap<&str, &'static str> {
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

    fn classifier_to_signature_renames(&self) -> HashMap<&str, &'static str> {
        self.classifier_formals
            .iter()
            .zip(self.signature_formals)
            .map(|(classifier, signature)| {
                (
                    classifier.as_str(),
                    crate::types::intern(signature.as_str()),
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
            formals: vec![class.clone(), constructor],
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
    fn same_spelled_constructor_suffix_is_not_a_classifier_coordinate() {
        let signature = signature();
        let classifier = vec!["classifier-T".to_owned()];
        let coordinates = ConstructorTypeParameterCoordinates::new(&classifier, &signature);

        assert_eq!(
            coordinates.signature_formals(),
            ["dependency-callable-7-class-T"]
        );
    }

    #[test]
    fn explicit_and_inferred_bindings_cross_only_by_prefix_ordinal() {
        let signature = signature();
        let classifier = vec!["classifier-T".to_owned()];
        let coordinates = ConstructorTypeParameterCoordinates::new(&classifier, &signature);
        let mut signature_bindings = HashMap::default();
        coordinates.bind_explicit(&[Ty::String], &mut signature_bindings);
        signature_bindings.insert("dependency-callable-7-constructor-T".to_owned(), Ty::Int);

        let classifier_bindings = coordinates.classifier_bindings(&signature_bindings);
        let round_trip = coordinates.signature_bindings(&classifier_bindings);
        let classifier_result = coordinates.classifier_types([Ty::ty_param(
            "dependency-callable-7-class-T",
            Ty::nullable(Ty::obj("kotlin/Any")),
        )]);
        let classifier_parameter =
            Ty::ty_param("classifier-T", Ty::nullable(Ty::obj("kotlin/Any")));

        assert_eq!(classifier_bindings.get("classifier-T"), Some(&Ty::String));
        assert_eq!(
            round_trip.get("dependency-callable-7-class-T"),
            Some(&Ty::String)
        );
        assert!(!classifier_bindings.contains_key("dependency-callable-7-constructor-T"));
        assert!(!round_trip.contains_key("dependency-callable-7-constructor-T"));
        assert_eq!(classifier_result, [classifier_parameter]);
    }

    #[test]
    fn postponed_context_completion_stays_in_signature_coordinates() {
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
        let coordinates = ConstructorTypeParameterCoordinates::new(&classifier, &signature);
        let mut bindings = HashMap::from_iter([(class, Ty::String)]);
        let contextual = HashSet::from_iter([dependent.clone()]);

        coordinates.complete_enclosing_context(&contextual, &mut bindings);

        assert_eq!(
            bindings.get(&dependent),
            Some(&Ty::obj_args("fixture/Base", &[Ty::String]))
        );
        assert!(!bindings.contains_key("classifier-U"));
    }
}

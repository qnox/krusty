//! Common code types an `expect` class actualized by an `actual typealias` as its own classifier.
//! Every target realizes the classifier the alias names instead, and Kotlin metadata records each
//! declared occurrence as that classifier abbreviated by the expect class, as kotlinc writes it.

use crate::fx_hash::HashMap;
use crate::types::TypeName;

impl super::IrFile {
    /// Name the classifier `expects` maps each actualized expect class to, keeping the expect class
    /// as the abbreviation of every declared occurrence, and the classifier `nested_expects` maps
    /// each classifier nested in one to. The class records must already be built.
    pub(crate) fn actualize_expects(
        &mut self,
        expects: &HashMap<TypeName, TypeName>,
        nested_expects: &HashMap<TypeName, TypeName>,
    ) {
        if expects.is_empty() {
            return;
        }
        for record in self.class_declarations.values_mut().flatten() {
            record.abbreviate_actualized_expects(expects);
        }
        for function in &mut self.package_functions {
            let spellings = &mut function.spellings;
            if let Some(receiver) = function.receiver {
                spellings
                    .receiver
                    .abbreviate_actualized_expects(receiver, expects);
            }
            crate::metadata::class_declarations::abbreviate_actualized_expects(
                function.params.iter().map(|(_, ty)| *ty),
                &mut spellings.params,
                expects,
            );
            spellings
                .ret
                .abbreviate_actualized_expects(function.ret, expects);
        }
        for property in &mut self.package_properties {
            let spellings = &mut property.spellings;
            if let Some(receiver) = property.receiver {
                spellings
                    .receiver
                    .abbreviate_actualized_expects(receiver, expects);
            }
            spellings
                .ret
                .abbreviate_actualized_expects(property.ty, expects);
            spellings
                .getter_ret
                .abbreviate_actualized_expects(property.ty, expects);
        }
        if nested_expects.is_empty() {
            self.remap_classifier_identities(expects);
        } else {
            let mut names = expects.clone();
            names.extend(
                nested_expects
                    .iter()
                    .map(|(&expect, &target)| (expect, target)),
            );
            self.remap_classifier_identities(&names);
        }
    }
}

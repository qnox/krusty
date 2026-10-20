//! Per-compilation policy for declaration-use positions migrating to errors.

use super::{DeprecationWarningFormat, LangFeatures};

/// A declaration-use position kotlinc is migrating from a warning to the declaration's level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeprecationMigration {
    ClassifierInImplicitInvoke,
    OuterImportedClass,
    CompanionInDelegation,
}

/// The selected policy for one migrating declaration-use position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeprecationMigrationPolicy {
    pub(crate) completed: bool,
    pub(crate) warning_format: Option<DeprecationWarningFormat>,
}

/// The three declaration-use migration policies selected for one source file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeprecationMigrations {
    classifiers_in_implicit_invokes: DeprecationMigrationPolicy,
    outer_imported_classes: DeprecationMigrationPolicy,
    companion_in_delegation: DeprecationMigrationPolicy,
}

impl DeprecationMigrations {
    pub(crate) fn from_features(features: &LangFeatures) -> Self {
        let policy = |feature| DeprecationMigrationPolicy {
            completed: features.has(feature),
            warning_format: features.deprecation_warning_format(feature),
        };
        Self {
            classifiers_in_implicit_invokes: policy(
                "ReportDeprecationsOfClassifiersInImplicitInvokes",
            ),
            outer_imported_classes: policy("ReportDeprecationsOfOuterImportedClasses"),
            companion_in_delegation: policy("ReportDeprecatedCompanionInDelegation"),
        }
    }

    pub(crate) fn policy(&self, migration: DeprecationMigration) -> &DeprecationMigrationPolicy {
        match migration {
            DeprecationMigration::ClassifierInImplicitInvoke => {
                &self.classifiers_in_implicit_invokes
            }
            DeprecationMigration::OuterImportedClass => &self.outer_imported_classes,
            DeprecationMigration::CompanionInDelegation => &self.companion_in_delegation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin_version::KotlinVersion;
    use crate::language_version::LanguageVersion;

    fn features(release: KotlinVersion) -> LangFeatures {
        LangFeatures::for_release(release, LanguageVersion::V2_4, LanguageVersion::V2_4)
    }

    #[test]
    fn release_and_explicit_feature_select_one_compilation_local_policy() {
        for release in [KotlinVersion::V2_4_0, KotlinVersion::V2_4_10] {
            let migrations = DeprecationMigrations::from_features(&features(release));
            let policy = migrations.policy(DeprecationMigration::OuterImportedClass);
            assert!(!policy.completed, "{release}");
            assert!(policy.warning_format.is_none(), "{release}");
        }

        let mut current = features(KotlinVersion::V2_4_20);
        let migrations = DeprecationMigrations::from_features(&current);
        let policy = migrations.policy(DeprecationMigration::OuterImportedClass);
        assert!(!policy.completed);
        assert_eq!(
            policy
                .warning_format
                .as_ref()
                .map(|format| format.message("deprecated declaration.")),
            Some(
                "deprecated declaration. This will become an error in language version 2.5. See \
                 https://youtrack.jetbrains.com/issue/KTLC-397."
                    .to_string()
            )
        );

        current.enable("ReportDeprecationsOfOuterImportedClasses");
        assert!(
            DeprecationMigrations::from_features(&current)
                .policy(DeprecationMigration::OuterImportedClass)
                .completed
        );
    }
}

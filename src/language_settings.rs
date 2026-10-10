//! Per-compilation Kotlin language and API settings.
//!
//! These settings are deliberately values, not process globals: a Bazel worker or language-server
//! process may analyze modules at different language/API levels in sequence. The selected compiler
//! release in [`crate::kotlin_version`] is a separate output-compatibility concern.

use crate::features::{FeatureSetting, LangFeatures};
use crate::kotlin_version::KotlinVersion;
use crate::language_version::LanguageVersion;

/// The finalized standard Kotlin compatibility inputs for one compilation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageSettings {
    pub language_version: LanguageVersion,
    pub api_version: LanguageVersion,
    pub features: LangFeatures,
}

impl LanguageSettings {
    /// Build settings after parsing all version options and ordered feature overrides.
    ///
    /// Kotlin defaults the API level to the selected language level. An explicitly newer API is
    /// invalid before source analysis starts. `feature_settings` are kotlinc's `specificFeatures`
    /// in application order: a later setting of the same feature wins.
    pub fn new(
        language_version: LanguageVersion,
        api_version: Option<LanguageVersion>,
        feature_settings: &[FeatureSetting],
    ) -> Result<Self, String> {
        Self::for_release(
            crate::kotlin_version::target(),
            language_version,
            api_version,
            feature_settings,
        )
    }

    /// [`Self::new`] under an explicitly chosen reference release, for a driver that configures a
    /// compilation before fixing the process-wide reference version.
    pub fn for_release(
        release: KotlinVersion,
        language_version: LanguageVersion,
        api_version: Option<LanguageVersion>,
        feature_settings: &[FeatureSetting],
    ) -> Result<Self, String> {
        let api_version = api_version.unwrap_or(language_version);
        if api_version > language_version {
            return Err(format!(
                "-api-version ({api_version}) cannot be greater than -language-version ({language_version})."
            ));
        }
        let mut features = LangFeatures::for_release(release, language_version, api_version);
        for setting in feature_settings {
            features.set(&setting.feature, setting.enabled);
        }
        Ok(Self {
            language_version,
            api_version,
            features,
        })
    }
}

impl LanguageSettings {
    /// The feature settings that rebuild these settings with [`Self::for_release`] over the same
    /// language/API defaults: every explicit setting, then every other enabled feature the
    /// defaults leave off and every default the features leave off. A feature set merged from
    /// several modules ([`LangFeatures::extend`]) round-trips the same way.
    pub fn feature_settings(&self) -> Vec<FeatureSetting> {
        let defaults = LangFeatures::for_release(
            self.features.release(),
            self.language_version,
            self.api_version,
        );
        let mut settings: Vec<FeatureSetting> = self
            .features
            .explicit_settings()
            .map(|(feature, enabled)| FeatureSetting {
                feature: feature.to_string(),
                enabled,
            })
            .collect();
        let mut differences: Vec<FeatureSetting> = self
            .features
            .iter()
            .filter(|name| !defaults.has(name))
            .map(|name| (name, true))
            .chain(
                defaults
                    .iter()
                    .filter(|name| !self.features.has(name))
                    .map(|name| (name, false)),
            )
            .filter(|(name, _)| !self.features.is_explicit(name))
            .map(|(name, enabled)| FeatureSetting {
                feature: name.to_string(),
                enabled,
            })
            .collect();
        differences.sort_by(|left, right| left.feature.cmp(&right.feature));
        settings.extend(differences);
        settings
    }
}

impl Default for LanguageSettings {
    fn default() -> Self {
        Self::new(LanguageVersion::default(), None, &[])
            .expect("the default language/API versions are compatible")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_settings_rebuild_the_same_settings() {
        let mut settings = LanguageSettings::for_release(
            KotlinVersion::V2_4_20,
            LanguageVersion::V2_3,
            Some(LanguageVersion::V2_2),
            &[
                FeatureSetting {
                    feature: "WhenGuards".to_string(),
                    enabled: false,
                },
                FeatureSetting {
                    feature: "NameBasedDestructuring".to_string(),
                    enabled: true,
                },
            ],
        )
        .unwrap();
        let other = LangFeatures::for_release(
            KotlinVersion::V2_4_20,
            LanguageVersion::V2_5,
            LanguageVersion::V2_5,
        );
        settings.features.extend(&other);
        let rebuilt = LanguageSettings::for_release(
            KotlinVersion::V2_4_20,
            settings.language_version,
            Some(settings.api_version),
            &settings.feature_settings(),
        )
        .unwrap();
        assert!(rebuilt.features.enables_same_features(&settings.features));
        assert_eq!(
            rebuilt.features.is_pre_release(),
            settings.features.is_pre_release()
        );
    }

    #[test]
    fn api_defaults_to_language_and_feature_overrides_apply_after_the_baseline() {
        let settings = LanguageSettings::new(
            LanguageVersion::V2_2,
            None,
            &[FeatureSetting {
                feature: "ContextParameters".to_owned(),
                enabled: true,
            }],
        )
        .unwrap();
        assert_eq!(settings.api_version, LanguageVersion::V2_2);
        assert!(settings.features.has("ContextParameters"));
    }

    #[test]
    fn api_cannot_be_newer_than_language() {
        assert_eq!(
            LanguageSettings::new(LanguageVersion::V2_2, Some(LanguageVersion::V2_4), &[],),
            Err("-api-version (2.4) cannot be greater than -language-version (2.2).".to_owned())
        );
    }
}

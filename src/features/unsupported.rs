//! kotlinc's `UNSUPPORTED_FEATURE` message: a port of `LanguageFeatureMessageRenderer` with
//! `Type.UNSUPPORTED`, rendered from the reference release's feature table.
//!
//! kotlinc's command line prints a diagnostic message with a lowercase first letter, and krusty
//! stores messages the way they are printed, so the rendered sentence starts `the feature`.

use std::sync::Arc;

use super::{LangFeatures, LanguageFeature};
use crate::language_version::LanguageVersion;

/// The sentence kotlinc appends for a feature whose use has a workaround besides enabling it
/// (`additionalFeatureMessages`).
fn additional_message(feature: &LanguageFeature) -> Option<&'static str> {
    (feature.name == "UnitConversionsOnArbitraryExpressions")
        .then_some("You can also change the original type of this expression to (...) -> Unit")
}

/// kotlinc's message for a use of `feature` that `settings` do not support.
pub(super) fn render(feature: &LanguageFeature, settings: &LangFeatures) -> String {
    let mut message = format!("the feature \"{}\" is ", feature.presentable_name);
    if settings.has(&feature.name) && settings.language_version() < LanguageVersion::V2_0 {
        message.push_str("not supported in language versions 1.*, please use version 2.0 or later");
    } else if feature.test_only {
        message.push_str("unsupported.");
    } else if let Some(since) = feature.since_version {
        if since > settings.language_version() {
            message.push_str(&format!("only available since language version {since}"));
        } else if feature.since_api_version > settings.api_version() {
            message.push_str(&format!(
                "only available since API version {}",
                feature.since_api_version
            ));
        } else {
            message.push_str("disabled");
        }
    } else {
        let flag = feature
            .flag
            .clone()
            .unwrap_or_else(|| format!("-XXLanguage:+{}", feature.name));
        message.push_str(&format!(
            "experimental and should be enabled explicitly. This can be done by supplying the \
             compiler argument '{flag}', but note that no stability guarantees are provided."
        ));
    }
    if let Some(hint_url) = &feature.hint_url {
        message.push_str(&format!(" (see: {hint_url})"));
    }
    if let Some(additional) = additional_message(feature) {
        message.push_str(&format!(". {additional}"));
    }
    message
}

impl LangFeatures {
    /// kotlinc's `UNSUPPORTED_FEATURE` message for a use of the feature `name` under these
    /// settings. `name` must be a feature of the reference release's table.
    pub fn unsupported_feature_message(&self, name: &str) -> String {
        let feature = self
            .table()
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not a kotlinc {} feature", self.release()));
        render(feature, self)
    }

    /// The state of the feature `name` as a phase that checks its uses consumes it.
    pub fn gate(&self, name: &str) -> FeatureGate {
        FeatureGate {
            unsupported: (!self.has(name)).then(|| self.unsupported_feature_message(name).into()),
        }
    }
}

/// One language feature as the phase that checks its uses consumes it: enabled, or disabled with
/// the message kotlinc reports at each use. The default is an enabled feature.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureGate {
    unsupported: Option<Arc<str>>,
}

impl FeatureGate {
    pub fn is_enabled(&self) -> bool {
        self.unsupported.is_none()
    }

    /// The message for a use of the disabled feature; `None` while it is enabled.
    pub fn unsupported_message(&self) -> Option<&Arc<str>> {
        self.unsupported.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::BehaviorAfterSinceVersion;
    use crate::kotlin_version::KotlinVersion;

    fn feature(name: &str) -> LanguageFeature {
        LanguageFeature {
            name: name.to_string(),
            since_version: None,
            since_api_version: LanguageVersion::new(1, 0),
            progressive: false,
            forces_pre_release_binaries: false,
            forces_pre_release_binaries_before: None,
            test_only: false,
            behavior_after_since_version: BehaviorAfterSinceVersion::CannotBeDisabled,
            presentable_name: "sample feature".to_string(),
            hint_url: None,
            flag: None,
            issue: None,
        }
    }

    fn settings(language: LanguageVersion, api: LanguageVersion) -> LangFeatures {
        LangFeatures::for_release(KotlinVersion::V2_4_20, language, api)
    }

    fn default_settings() -> LangFeatures {
        settings(LanguageVersion::V2_4, LanguageVersion::V2_4)
    }

    #[test]
    fn an_unreleased_feature_names_its_argument() {
        let mut feature = feature("SampleFeature");
        feature.flag = Some("-Xsample".to_string());
        assert_eq!(
            render(&feature, &default_settings()),
            "the feature \"sample feature\" is experimental and should be enabled explicitly. \
             This can be done by supplying the compiler argument '-Xsample', but note that no \
             stability guarantees are provided."
        );
    }

    #[test]
    fn an_unreleased_feature_without_an_argument_names_the_raw_toggle() {
        assert_eq!(
            default_settings().unsupported_feature_message("UnnamedLocalVariables"),
            "the feature \"unnamed local variables\" is experimental and should be enabled \
             explicitly. This can be done by supplying the compiler argument \
             '-XXLanguage:+UnnamedLocalVariables', but note that no stability guarantees are \
             provided."
        );
    }

    #[test]
    fn a_feature_of_a_later_language_version_names_that_version() {
        assert_eq!(
            default_settings().unsupported_feature_message("NameBasedDestructuring"),
            "the feature \"name based destructuring\" is only available since language version 2.5"
        );
    }

    #[test]
    fn a_feature_of_a_later_api_version_names_that_version() {
        let mut feature = feature("SampleFeature");
        feature.since_version = Some(LanguageVersion::V2_2);
        feature.since_api_version = LanguageVersion::V2_3;
        assert_eq!(
            render(
                &feature,
                &settings(LanguageVersion::V2_4, LanguageVersion::V2_2)
            ),
            "the feature \"sample feature\" is only available since API version 2.3"
        );
    }

    #[test]
    fn a_released_feature_turned_off_is_disabled() {
        let mut settings = default_settings();
        settings.disable("WhenGuards");
        assert_eq!(
            settings.unsupported_feature_message("WhenGuards"),
            "the feature \"when guards\" is disabled"
        );
    }

    #[test]
    fn a_test_only_feature_is_unsupported() {
        let mut feature = feature("SampleFeature");
        feature.test_only = true;
        feature.since_version = Some(LanguageVersion::V2_6);
        assert_eq!(
            render(&feature, &default_settings()),
            "the feature \"sample feature\" is unsupported."
        );
    }

    #[test]
    fn an_enabled_feature_is_unsupported_before_language_version_2() {
        let mut settings = settings(LanguageVersion::new(1, 9), LanguageVersion::new(1, 9));
        settings.enable("SampleFeature");
        let mut feature = feature("SampleFeature");
        feature.test_only = true;
        assert_eq!(
            render(&feature, &settings),
            "the feature \"sample feature\" is not supported in language versions 1.*, please \
             use version 2.0 or later"
        );
    }

    #[test]
    fn a_hint_url_follows_the_reason() {
        let mut feature = feature("SampleFeature");
        feature.since_version = Some(LanguageVersion::V2_5);
        feature.hint_url = Some("https://kotl.in/sample".to_string());
        assert_eq!(
            render(&feature, &default_settings()),
            "the feature \"sample feature\" is only available since language version 2.5 \
             (see: https://kotl.in/sample)"
        );
    }

    #[test]
    fn unit_conversions_name_the_alternative() {
        let mut settings = default_settings();
        settings.disable("UnitConversionsOnArbitraryExpressions");
        assert_eq!(
            settings.unsupported_feature_message("UnitConversionsOnArbitraryExpressions"),
            "the feature \"unit conversions on arbitrary expressions\" is experimental and should \
             be enabled explicitly. This can be done by supplying the compiler argument \
             '-XXLanguage:+UnitConversionsOnArbitraryExpressions', but note that no stability \
             guarantees are provided.. You can also change the original type of this expression \
             to (...) -> Unit"
        );
    }

    #[test]
    fn a_gate_carries_the_message_only_while_disabled() {
        let mut settings = default_settings();
        assert_eq!(
            settings
                .gate("LocalTypeAliases")
                .unsupported_message()
                .map(|message| message.to_string()),
            Some(
                "the feature \"local type aliases\" is experimental and should be enabled \
                 explicitly. This can be done by supplying the compiler argument \
                 '-Xlocal-type-aliases', but note that no stability guarantees are provided."
                    .to_string()
            )
        );
        settings.enable("LocalTypeAliases");
        assert!(settings.gate("LocalTypeAliases").is_enabled());
    }
}

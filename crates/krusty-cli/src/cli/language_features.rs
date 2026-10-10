//! The language features a kotlinc command line selects, by kotlinc's own rules: the `@Enables` and
//! `@Disables` arguments (`configureCommonLanguageFeatures`), then each `-XXLanguage:` setting
//! (`LanguageSettingsParser`, `configureLanguageFeaturesFromInternalArgs`), with the notices,
//! warnings and errors kotlinc reports about them.

use krusty::features::{
    BehaviorAfterSinceVersion, FeatureSetting, FeatureTable, LangFeatures, LanguageFeature,
    LanguageVersionPolicy,
};
use krusty::language_version::LanguageVersion;

use super::{ArgumentProblem, CliWarning, WarningName};
use crate::kotlinc_arguments::{ArgumentSpec, Disposition, Origin, Recorded, ValueKind};

/// One `-XXLanguage:` argument kotlinc accepted (`ManualLanguageFeatureSetting`).
pub(super) struct ManualSetting {
    /// The whole argument, as kotlinc echoes it: `-XXLanguage:+Feature`.
    argument: String,
    setting: FeatureSetting,
    progressive: bool,
}

/// Read the `-XXLanguage` values in command-line order, keeping each feature's last setting. A
/// value kotlinc cannot use is reported in its words and skipped; a test-only feature is reported as an error and still applied, as
/// kotlinc does.
pub(super) fn parse_manual_settings(
    values: &[String],
    table: &FeatureTable,
    problems: &mut Vec<ArgumentProblem>,
) -> Vec<ManualSetting> {
    let mut settings = Vec::new();
    for value in values {
        let argument = format!("-XXLanguage:{value}");
        let enabled = match value.chars().next() {
            Some('+') => true,
            Some('-') => false,
            _ => {
                problems.push(ArgumentProblem::warning(format!(
                    "Incorrect internal argument syntax, missing modificator: {argument}"
                )));
                continue;
            }
        };
        let name = &value[1..];
        if name.is_empty() {
            problems.push(ArgumentProblem::warning(format!(
                "Empty language feature name for internal argument '{argument}'"
            )));
            continue;
        }
        let Some(feature) = table.get(name) else {
            problems.push(ArgumentProblem::warning(format!(
                "Unknown language feature '{name}' in passed internal argument '{argument}'"
            )));
            continue;
        };
        if feature.test_only {
            problems.push(ArgumentProblem::error(format!(
                "Language feature '{name}' is test-only and cannot be enabled from command line"
            )));
        }
        // A later setting replaces an earlier one for the same feature, and moves to the end.
        settings.retain(|earlier: &ManualSetting| earlier.setting.feature != name);
        settings.push(ManualSetting {
            argument,
            setting: FeatureSetting {
                feature: name.to_string(),
                enabled,
            },
            progressive: feature.progressive,
        });
    }
    settings
}

/// kotlinc's `reportUnsafeInternalArgumentsIfAny`: every `-XXLanguage` setting except one that
/// enables a progressive feature is listed in an "ATTENTION!" warning.
pub(super) fn unsafe_arguments_notice(settings: &[ManualSetting]) -> Option<ArgumentProblem> {
    let listed: Vec<&str> = settings
        .iter()
        .filter(|manual| !(manual.progressive && manual.setting.enabled))
        .map(|manual| manual.argument.as_str())
        .collect();
    (!listed.is_empty()).then(|| {
        ArgumentProblem::warning(format!(
            "ATTENTION!\nThis build uses unsafe internal compiler arguments:\n\n{}\n\n\
             This mode is not recommended for production use,\n\
             as no stability/compatibility guarantees are given on\n\
             compiler or generated code. Use it at your own risk!\n",
            listed.join("\n")
        ))
    })
}

/// The `@Enables`/`@Disables` arguments krusty applies, in first-occurrence order, each with its
/// last value: kotlinc reads the final field value of each argument.
pub(super) fn feature_arguments<'c>(
    explicit: &'c [(&'c ArgumentSpec, Recorded)],
) -> Vec<(&'c ArgumentSpec, &'c str)> {
    explicit
        .iter()
        .filter(|(spec, _)| {
            spec.changes_language_features()
                && spec.origin == Origin::Current
                && crate::kotlinc_arguments::disposition::of(&spec.name)
                    == Some(Disposition::Applied)
        })
        .filter_map(|(spec, recorded)| match recorded {
            Recorded::Scalars(values) => values.last().map(|value| (*spec, value.as_str())),
            Recorded::List(_) => None,
        })
        .collect()
}

/// Whether an `@Enables`/`@Disables` entry applies to the argument's value: a boolean argument's
/// entries apply when it is set, a string argument's when the value is the entry's.
fn toggle_applies(if_value_is: Option<&str>, value: &str) -> bool {
    match if_value_is {
        None => value == "true",
        Some(expected) => expected == value,
    }
}

/// kotlinc's `configureCommonLanguageFeatures`: the states the arguments' `@Enables` and
/// `@Disables` entries put, in argument order.
pub(super) fn common_settings(arguments: &[(&ArgumentSpec, &str)]) -> Vec<FeatureSetting> {
    let mut settings = Vec::new();
    for &(spec, value) in arguments {
        for (toggles, enabled) in [(&spec.enables, true), (&spec.disables, false)] {
            for toggle in toggles {
                if toggle_applies(toggle.if_value_is.as_deref(), value) {
                    settings.push(FeatureSetting {
                        feature: toggle.feature.clone(),
                        enabled,
                    });
                }
            }
        }
    }
    settings
}

/// The refusal of each applied `@Enables` entry naming a feature krusty does not model: its
/// accepted spelling would promise behaviour krusty does not have.
pub(super) fn unmodeled_feature_errors(arguments: &[(&ArgumentSpec, &str)]) -> Vec<String> {
    let mut errors = Vec::new();
    for &(spec, value) in arguments {
        if let Some(toggle) = spec.enables.iter().find(|toggle| {
            toggle_applies(toggle.if_value_is.as_deref(), value)
                && !LangFeatures::models(&toggle.feature)
        }) {
            errors.push(format!(
                "krusty does not implement the language feature '{}' selected by '{}'",
                toggle.feature, spec.name
            ));
        }
    }
    errors
}

/// `-progressive`: every progressive feature of the release that no `@Enables`/`@Disables`
/// argument set, enabled (kotlinc keeps an argument's explicit state).
pub(super) fn progressive_settings(
    table: &FeatureTable,
    common: &[FeatureSetting],
) -> Vec<FeatureSetting> {
    table
        .progressive()
        .filter(|feature| !common.iter().any(|setting| setting.feature == feature.name))
        .map(|feature| FeatureSetting {
            feature: feature.name.clone(),
            enabled: true,
        })
        .collect()
}

/// The settings to apply in kotlinc's `configureLanguageFeatures` order: the `@Enables`/`@Disables`
/// arguments, then `-progressive`, then `-XXLanguage`, which overrides both.
pub(super) fn ordered_settings(
    common: Vec<FeatureSetting>,
    progressive: Vec<FeatureSetting>,
    manual: &[ManualSetting],
) -> Vec<FeatureSetting> {
    let mut settings = common;
    settings.extend(progressive);
    settings.extend(manual.iter().map(|manual| manual.setting.clone()));
    settings
}

/// Whether kotlinc refuses this setting because it disables a feature whose introducing language
/// version is no longer supported.
fn cannot_be_disabled(
    manual: &ManualSetting,
    feature: &LanguageFeature,
    versions: &LanguageVersionPolicy,
) -> bool {
    !manual.setting.enabled
        && feature.since_version.is_some_and(|since| {
            versions
                .language(since)
                .is_some_and(|status| status.unsupported)
        })
        && feature.behavior_after_since_version == BehaviorAfterSinceVersion::CannotBeDisabled
}

/// The refusal of `-progressive` when a feature it turns on is one krusty does not model, named as
/// an `@Enables` argument's refusal names its feature. `before` is the state the defaults and the
/// `@Enables`/`@Disables` arguments leave; a progressive feature already on there is no change.
pub(super) fn unmodeled_progressive_error(
    progressive: &[FeatureSetting],
    before: &LangFeatures,
) -> Option<String> {
    progressive
        .iter()
        .find(|setting| !LangFeatures::models(&setting.feature) && !before.has(&setting.feature))
        .map(|setting| {
            format!(
                "krusty does not implement the language feature '{}' selected by '-progressive'",
                setting.feature
            )
        })
}

/// kotlinc's `configureLanguageFeaturesFromInternalArgs` checks: the error for disabling a feature
/// whose introducing language version is no longer supported, and the warning for enabling an
/// unreleased feature that forces pre-release binaries.
pub(super) fn manual_setting_checks(
    manual: &[ManualSetting],
    table: &FeatureTable,
    versions: &LanguageVersionPolicy,
    language_version: LanguageVersion,
) -> (Option<String>, Option<CliWarning>) {
    let mut pre_release = Vec::new();
    let mut cannot_disable = Vec::new();
    for manual in manual {
        let feature = table
            .get(&manual.setting.feature)
            .expect("a manual setting names a known feature");
        if manual.setting.enabled
            && feature.forces_pre_release_binaries_if_enabled(versions, language_version)
        {
            pre_release.push(feature.name.as_str());
        }
        if cannot_be_disabled(manual, feature, versions) {
            cannot_disable.push(feature.name.as_str());
        }
    }
    let error = (!cannot_disable.is_empty()).then(|| {
        format!(
            "the following features cannot be disabled manually, because the version they first \
             appeared in is no longer supported:\n{}",
            cannot_disable.join(", ")
        )
    });
    let warning = (!pre_release.is_empty()).then(|| CliWarning {
        name: None,
        message: format!(
            "following manually enabled features will force generation of pre-release binaries: {}",
            pre_release.join(", ")
        ),
    });
    (error, warning)
}

/// kotlinc's `checkRedundantArguments`: an `@Enables` argument that changes no default is
/// redundant, and one that disables a feature on by default is reported as such.
pub(super) fn redundancy_warnings(
    arguments: &[(&ArgumentSpec, &str)],
    defaults: &LangFeatures,
    api_version: LanguageVersion,
) -> Vec<CliWarning> {
    let language_version = defaults.language_version();
    // `checkNecessity`: the entry applies to the value and the state it puts is not the default.
    let necessary = |if_value_is: Option<&str>, value: &str, feature: &str, enabled: bool| {
        toggle_applies(if_value_is, value)
            && enabled != defaults.is_enabled_by_default(feature, api_version)
    };
    let mut warnings = Vec::new();
    for &(spec, value) in arguments {
        if spec
            .enables
            .iter()
            .any(|toggle| necessary(toggle.if_value_is.as_deref(), value, &toggle.feature, true))
        {
            continue;
        }
        let rendered = match spec.kind {
            ValueKind::String => format!("{}={value}", spec.name),
            ValueKind::Bool | ValueKind::Array => spec.name.clone(),
        };
        let disables_stable = spec
            .disables
            .iter()
            .any(|toggle| necessary(toggle.if_value_is.as_deref(), value, &toggle.feature, false));
        warnings.push(if disables_stable {
            CliWarning {
                name: Some(WarningName::CliArgDisablesStableFeature),
                message: format!(
                    "the argument '{rendered}' disables a stable language feature for the current \
                     language version {language_version}. Future support for this mode is not \
                     guaranteed."
                ),
            }
        } else {
            CliWarning {
                name: Some(WarningName::RedundantCliArg),
                message: format!(
                    "the argument '{rendered}' is redundant for the current language version \
                     {language_version}."
                ),
            }
        });
    }
    warnings
}

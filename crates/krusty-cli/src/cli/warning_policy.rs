//! Typed global and per-diagnostic warning policy accepted by the kotlinc-compatible CLI.

use std::collections::BTreeMap;

use krusty::diag::names::{self, DiagnosticSeverity};
use krusty::diag::DiagnosticName;
use krusty::kotlin_version::KotlinVersion;

/// The warnings krusty reports under their kotlinc names, the ones `-Xwarning-level` may
/// configure. A name enters this list only when krusty can emit that diagnostic; accepting any
/// other spelling would promise a policy the compiler cannot apply. Whether the selected release
/// declares the name at all is that release's own diagnostic table.
const REPORTED_WARNINGS: &[DiagnosticName] = &[
    names::CLASS_LITERAL_LHS_NOT_A_CLASS_WARNING,
    names::CLI_ARG_DISABLES_STABLE_FEATURE,
    names::COMPILER_ARGUMENTS_WARNING,
    names::DATA_CLASS_COPY_VISIBILITY_WILL_BE_CHANGED_WARNING,
    names::DATA_CLASS_INVISIBLE_COPY_USAGE_WARNING,
    names::DEPRECATED_CLI_ARG,
    names::DEPRECATED_LANGUAGE_VERSION,
    names::DEPRECATED_MODIFIER_FOR_TARGET,
    names::DEPRECATION,
    names::DEPRECATION_ERROR_MIGRATION_PERIOD_WARNING,
    names::EXPERIMENTAL_LANGUAGE_VERSION,
    names::EXPLICIT_TYPE_ARGUMENTS_IN_PROPERTY_ACCESS_WARNING,
    names::INAPPLICABLE_OPERATOR_MODIFIER_WARNING,
    names::MISSING_DEPENDENCY_SUPERCLASS_WARNING,
    names::NO_EXPLICIT_RETURN_TYPE_IN_API_MODE_WARNING,
    names::NO_EXPLICIT_VISIBILITY_IN_API_MODE_WARNING,
    names::OPT_IN_REQUIREMENT_MARKER_IS_UNRESOLVED,
    names::OPT_IN_USAGE,
    names::PARENTHESIZED_PACKAGE_QUALIFIER_WARNING,
    names::REDUNDANT_ANNOTATION,
    names::REDUNDANT_CLI_ARG,
    names::REMOVED_CLI_ARG,
    names::TYPE_ARGUMENTS_NOT_ALLOWED_IN_PACKAGE_QUALIFIER_WARNING,
    names::TYPE_ARGUMENTS_NOT_ALLOWED_WARNING,
    names::TYPE_INTERSECTION_AS_REIFIED_DEPRECATION_WARNING,
    names::TYPE_INTERSECTION_AS_REIFIED_WARNING,
    names::TYPE_PARAMETER_AS_REIFIED_DEPRECATION_WARNING,
    names::UNSUPPORTED_ARRAY_OF_NOTHING_IN_CLASS_LITERAL_LHS,
    names::VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING,
    names::WRONG_NUMBER_OF_TYPE_ARGUMENTS_IN_GET_CLASS_WARNING,
];

/// The warning `name` selects in `release`: one the release declares as a warning and krusty
/// reports.
fn warning_name(name: &str, release: KotlinVersion) -> Option<DiagnosticName> {
    let name = DiagnosticName::parse(name)?;
    let declared = matches!(
        name.severity(release),
        Some(DiagnosticSeverity::Warning | DiagnosticSeverity::StrongWarning)
    );
    (declared && REPORTED_WARNINGS.contains(&name)).then_some(name)
}

/// The three severities accepted by kotlinc's warning-level option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarningLevel {
    Error,
    Warning,
    Disabled,
}

impl WarningLevel {
    fn parse(level: &str) -> Option<Self> {
        Some(match level {
            "error" => Self::Error,
            "warning" => Self::Warning,
            "disabled" => Self::Disabled,
            _ => return None,
        })
    }
}

/// What the driver does with one warning after global and named policy have been combined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarningDisposition {
    Disabled,
    Warning,
    Error,
    WarningAndFail,
}

#[derive(Clone, Debug)]
pub struct WarningPolicy {
    compiler_default: WarningLevel,
    configured: BTreeMap<DiagnosticName, WarningLevel>,
}

impl Default for WarningPolicy {
    fn default() -> Self {
        Self {
            compiler_default: WarningLevel::Warning,
            configured: BTreeMap::new(),
        }
    }
}

impl WarningPolicy {
    pub(super) fn configure(&mut self, value: &str, release: KotlinVersion) -> Result<(), String> {
        let Some((name, severity)) = value.split_once(':') else {
            return Err(format!(
                "invalid value '{value}' for -Xwarning-level: expected <NAME>:<error|warning|disabled>"
            ));
        };
        let Some(name) = warning_name(name, release) else {
            let name = value.split_once(':').map_or(value, |(name, _)| name);
            return Err(format!("warning with name \"{name}\" does not exist"));
        };
        let Some(severity) = WarningLevel::parse(severity) else {
            return Err(format!(
                "invalid severity '{severity}' in -Xwarning-level={value}; supported severities: error, warning, disabled"
            ));
        };
        if self.configured.contains_key(&name) {
            let name = value.split_once(':').map_or(value, |(name, _)| name);
            return Err(format!(
                "warning with name \"{name}\" has already been configured"
            ));
        }
        self.configured.insert(name, severity);
        Ok(())
    }

    /// Apply kotlinc's global `-Werror` policy. It wins over `-nowarn` regardless of argument
    /// order; an explicit per-diagnostic level still wins over this default.
    pub(super) fn promote_warnings(&mut self) {
        self.compiler_default = WarningLevel::Error;
    }

    /// Apply kotlinc's global `-nowarn` policy. `-Werror` wins when both are present, independent
    /// of argument order, as measured against kotlinc 2.4.20.
    pub(super) fn suppress_compiler_warnings(&mut self) {
        if self.compiler_default != WarningLevel::Error {
            self.compiler_default = WarningLevel::Disabled;
        }
    }

    /// The configured level for a diagnostic, before global policy is considered.
    pub(super) fn configured_level(&self, name: DiagnosticName) -> Option<WarningLevel> {
        self.configured.get(&name).copied()
    }

    /// Command-line configuration warnings are not hidden by plain `-nowarn`. An explicit named
    /// level still wins, while global `-Werror` retains warning severity and fails afterward. A
    /// warning without a configurable identity follows only the global policy.
    pub fn command_line_disposition(&self, name: Option<DiagnosticName>) -> WarningDisposition {
        match name.and_then(|name| self.configured_level(name)) {
            Some(WarningLevel::Disabled) => WarningDisposition::Disabled,
            Some(WarningLevel::Warning) => WarningDisposition::Warning,
            Some(WarningLevel::Error) => WarningDisposition::Error,
            None if self.compiler_default == WarningLevel::Error => {
                WarningDisposition::WarningAndFail
            }
            None => WarningDisposition::Warning,
        }
    }

    /// Policy for a compiler warning. A named level wins over the global default exactly as it
    /// does for command-line warnings; `None` is reserved for genuinely unnamed diagnostics and
    /// module warnings.
    pub fn compiler_disposition(&self, name: Option<DiagnosticName>) -> WarningDisposition {
        if let Some(level) = name.and_then(|name| self.configured_level(name)) {
            return match level {
                WarningLevel::Disabled => WarningDisposition::Disabled,
                WarningLevel::Warning => WarningDisposition::Warning,
                WarningLevel::Error => WarningDisposition::Error,
            };
        }
        match self.compiler_default {
            WarningLevel::Disabled => WarningDisposition::Disabled,
            WarningLevel::Warning => WarningDisposition::Warning,
            WarningLevel::Error => WarningDisposition::WarningAndFail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_compiler_warning_overrides_global_policy() {
        for (level, expected) in [
            ("disabled", WarningDisposition::Disabled),
            ("warning", WarningDisposition::Warning),
            ("error", WarningDisposition::Error),
        ] {
            let mut policy = WarningPolicy::default();
            policy.promote_warnings();
            policy
                .configure(&format!("DEPRECATION:{level}"), KotlinVersion::V2_4_20)
                .unwrap();
            assert_eq!(
                policy.compiler_disposition(Some(names::DEPRECATION)),
                expected
            );
            assert_eq!(
                policy.compiler_disposition(None),
                WarningDisposition::WarningAndFail
            );
        }
    }

    /// Measured with kotlinc 2.4.0: of the reified warnings it declares only
    /// `TYPE_INTERSECTION_AS_REIFIED_WARNING`; the two deprecation warnings are new in 2.4.20.
    #[test]
    fn reified_warning_names_follow_the_selected_release_catalog() {
        for release in KotlinVersion::supported() {
            for (name, diagnostic, since) in [
                (
                    "TYPE_INTERSECTION_AS_REIFIED_WARNING",
                    names::TYPE_INTERSECTION_AS_REIFIED_WARNING,
                    KotlinVersion::V2_4_0,
                ),
                (
                    "TYPE_INTERSECTION_AS_REIFIED_DEPRECATION_WARNING",
                    names::TYPE_INTERSECTION_AS_REIFIED_DEPRECATION_WARNING,
                    KotlinVersion::V2_4_20,
                ),
                (
                    "TYPE_PARAMETER_AS_REIFIED_DEPRECATION_WARNING",
                    names::TYPE_PARAMETER_AS_REIFIED_DEPRECATION_WARNING,
                    KotlinVersion::V2_4_20,
                ),
            ] {
                let mut policy = WarningPolicy::default();
                if release < since {
                    assert_eq!(
                        policy.configure(&format!("{name}:error"), release),
                        Err(format!("warning with name \"{name}\" does not exist")),
                        "{release}: {name}"
                    );
                    continue;
                }
                assert_eq!(
                    policy.configure(&format!("{name}:error"), release),
                    Ok(()),
                    "{release}: {name}"
                );
                assert_eq!(
                    policy.compiler_disposition(Some(diagnostic)),
                    WarningDisposition::Error,
                    "{release}: {name}"
                );
            }
        }
    }

    /// Every warning krusty reports is one the newest release declares as a warning.
    #[test]
    fn every_reported_warning_is_declared_by_the_newest_release() {
        for &name in REPORTED_WARNINGS {
            assert_eq!(
                warning_name(name.as_str(), KotlinVersion::newest()),
                Some(name),
                "{name}"
            );
        }
        assert!(REPORTED_WARNINGS.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// kotlinc declares `UNRESOLVED_REFERENCE`, but as an error, which has no warning level.
    #[test]
    fn an_error_is_not_a_configurable_warning() {
        assert_eq!(
            warning_name("UNRESOLVED_REFERENCE", KotlinVersion::V2_4_20),
            None
        );
    }

    #[test]
    fn compiler_warning_names_follow_the_selected_release_catalog() {
        const NAME: &str = "VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING:warning";
        for release in [KotlinVersion::V2_4_0, KotlinVersion::V2_4_10] {
            assert_eq!(
                WarningPolicy::default().configure(NAME, release),
                Err(
                    "warning with name \
                     \"VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING\" does not exist"
                        .to_string()
                ),
                "{release}"
            );
        }
        for (level, expected_level, expected_disposition) in [
            (
                "disabled",
                WarningLevel::Disabled,
                WarningDisposition::Disabled,
            ),
            (
                "warning",
                WarningLevel::Warning,
                WarningDisposition::Warning,
            ),
            ("error", WarningLevel::Error, WarningDisposition::Error),
        ] {
            let mut current = WarningPolicy::default();
            assert_eq!(
                current.configure(
                    &format!("VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING:{level}"),
                    KotlinVersion::V2_4_20,
                ),
                Ok(())
            );
            assert_eq!(
                current.configured_level(
                    names::VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING
                ),
                Some(expected_level),
                "{level}"
            );
            assert_eq!(
                current.compiler_disposition(Some(
                    names::VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING,
                )),
                expected_disposition,
                "{level}"
            );
        }
    }

    #[test]
    fn data_class_copy_warning_names_follow_the_selected_release_catalog() {
        for (name, identity) in [
            (
                "DATA_CLASS_COPY_VISIBILITY_WILL_BE_CHANGED_WARNING",
                names::DATA_CLASS_COPY_VISIBILITY_WILL_BE_CHANGED_WARNING,
            ),
            (
                "DATA_CLASS_INVISIBLE_COPY_USAGE_WARNING",
                names::DATA_CLASS_INVISIBLE_COPY_USAGE_WARNING,
            ),
        ] {
            for release in KotlinVersion::supported() {
                let mut policy = WarningPolicy::default();
                assert_eq!(
                    policy.configure(&format!("{name}:disabled"), release),
                    Ok(()),
                    "{release}: {name}"
                );
                assert_eq!(
                    policy.configured_level(identity),
                    Some(WarningLevel::Disabled),
                    "{release}: {name}"
                );
            }
        }
    }

    #[test]
    fn operator_modifier_warning_name_follows_the_selected_release_catalog() {
        const NAME: &str = "INAPPLICABLE_OPERATOR_MODIFIER_WARNING";
        // Measured: kotlinc 2.4.0 and 2.4.10 already declare it, so both accept the level.
        for release in [KotlinVersion::V2_4_0, KotlinVersion::V2_4_10] {
            assert_eq!(
                WarningPolicy::default().configure(&format!("{NAME}:warning"), release),
                Ok(()),
                "{release}"
            );
        }
        for (level, expected_level, expected_disposition) in [
            (
                "disabled",
                WarningLevel::Disabled,
                WarningDisposition::Disabled,
            ),
            (
                "warning",
                WarningLevel::Warning,
                WarningDisposition::Warning,
            ),
            ("error", WarningLevel::Error, WarningDisposition::Error),
        ] {
            let mut current = WarningPolicy::default();
            assert_eq!(
                current.configure(&format!("{NAME}:{level}"), KotlinVersion::V2_4_20),
                Ok(())
            );
            assert_eq!(
                current.configured_level(names::INAPPLICABLE_OPERATOR_MODIFIER_WARNING),
                Some(expected_level),
                "{level}"
            );
            assert_eq!(
                current.compiler_disposition(Some(names::INAPPLICABLE_OPERATOR_MODIFIER_WARNING,)),
                expected_disposition,
                "{level}"
            );
        }
    }
}

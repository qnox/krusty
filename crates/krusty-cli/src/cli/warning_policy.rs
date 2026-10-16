//! Typed global and per-diagnostic warning policy accepted by the kotlinc-compatible CLI.

use std::collections::BTreeMap;

use krusty::features::FeatureTable;
use krusty::kotlin_version::KotlinVersion;

/// Stable diagnostic names exposed through kotlinc's `-Xwarning-level` contract. A name enters
/// this registry only when krusty can emit that diagnostic; accepting any other spelling would
/// promise a policy the compiler cannot apply.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WarningName {
    Compiler(krusty::diag::CompilerWarningName),
    CliArgDisablesStableFeature,
    DeprecatedCliArg,
    DeprecatedLanguageVersion,
    ExperimentalLanguageVersion,
    RedundantCliArg,
    RemovedCliArg,
}

impl WarningName {
    fn parse(name: &str, release: KotlinVersion) -> Option<Self> {
        if let Some(name) = krusty::diag::CompilerWarningName::parse(name) {
            let available = match name {
                krusty::diag::CompilerWarningName::Deprecation => true,
                krusty::diag::CompilerWarningName::InapplicableOperatorModifier => {
                    FeatureTable::for_version(release).is_some_and(|table| {
                        table
                            .get("ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects")
                            .is_some()
                    })
                }
                krusty::diag::CompilerWarningName::TypeArgumentsNotAllowed => {
                    FeatureTable::for_version(release)
                        .is_some_and(|table| table.get("ForbidUselessTypeArgumentsIn25").is_some())
                }
                krusty::diag::CompilerWarningName::ValueClassCannotBeRecursiveViaTypeParameters => {
                    FeatureTable::for_version(release).is_some_and(|table| {
                        table
                            .get("ForbidValueClassRecursionViaTypeParameters")
                            .is_some()
                    })
                }
            };
            if !available {
                return None;
            }
            return Some(Self::Compiler(name));
        }
        Some(match name {
            "CLI_ARG_DISABLES_STABLE_FEATURE" => Self::CliArgDisablesStableFeature,
            "DEPRECATED_CLI_ARG" => Self::DeprecatedCliArg,
            "DEPRECATED_LANGUAGE_VERSION" => Self::DeprecatedLanguageVersion,
            "EXPERIMENTAL_LANGUAGE_VERSION" => Self::ExperimentalLanguageVersion,
            "REDUNDANT_CLI_ARG" => Self::RedundantCliArg,
            "REMOVED_CLI_ARG" => Self::RemovedCliArg,
            _ => return None,
        })
    }
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
    configured: BTreeMap<WarningName, WarningLevel>,
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
        let Some(name) = WarningName::parse(name, release) else {
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
    pub(super) fn configured_level(&self, name: WarningName) -> Option<WarningLevel> {
        self.configured.get(&name).copied()
    }

    /// Command-line configuration warnings are not hidden by plain `-nowarn`. An explicit named
    /// level still wins, while global `-Werror` retains warning severity and fails afterward. A
    /// warning without a configurable identity follows only the global policy.
    pub fn command_line_disposition(&self, name: Option<WarningName>) -> WarningDisposition {
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
    pub fn compiler_disposition(
        &self,
        name: Option<krusty::diag::CompilerWarningName>,
    ) -> WarningDisposition {
        if let Some(level) = name
            .map(WarningName::Compiler)
            .and_then(|name| self.configured_level(name))
        {
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
    use krusty::diag::CompilerWarningName;

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
                policy.compiler_disposition(Some(CompilerWarningName::Deprecation)),
                expected
            );
            assert_eq!(
                policy.compiler_disposition(None),
                WarningDisposition::WarningAndFail
            );
        }
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
                current.configured_level(WarningName::Compiler(
                    CompilerWarningName::ValueClassCannotBeRecursiveViaTypeParameters,
                )),
                Some(expected_level),
                "{level}"
            );
            assert_eq!(
                current.compiler_disposition(Some(
                    CompilerWarningName::ValueClassCannotBeRecursiveViaTypeParameters,
                )),
                expected_disposition,
                "{level}"
            );
        }
    }

    #[test]
    fn operator_modifier_warning_name_follows_the_selected_release_catalog() {
        const NAME: &str = "INAPPLICABLE_OPERATOR_MODIFIER_WARNING";
        for release in [KotlinVersion::V2_4_0, KotlinVersion::V2_4_10] {
            assert_eq!(
                WarningPolicy::default().configure(&format!("{NAME}:warning"), release),
                Err(format!("warning with name \"{NAME}\" does not exist")),
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
                current.configured_level(WarningName::Compiler(
                    CompilerWarningName::InapplicableOperatorModifier,
                )),
                Some(expected_level),
                "{level}"
            );
            assert_eq!(
                current
                    .compiler_disposition(Some(CompilerWarningName::InapplicableOperatorModifier,)),
                expected_disposition,
                "{level}"
            );
        }
    }
}

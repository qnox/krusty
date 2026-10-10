//! Typed global and per-diagnostic warning policy accepted by the kotlinc-compatible CLI.

use std::collections::BTreeMap;

/// Stable diagnostic names exposed through kotlinc's `-Xwarning-level` contract. A name enters
/// this registry only when krusty can emit that diagnostic; accepting any other spelling would
/// promise a policy the compiler cannot apply.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WarningName {
    DeprecatedLanguageVersion,
    ExperimentalLanguageVersion,
    RedundantCliArg,
}

impl WarningName {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "DEPRECATED_LANGUAGE_VERSION" => Self::DeprecatedLanguageVersion,
            "EXPERIMENTAL_LANGUAGE_VERSION" => Self::ExperimentalLanguageVersion,
            "REDUNDANT_CLI_ARG" => Self::RedundantCliArg,
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
    pub(super) fn configure(&mut self, value: &str) -> Result<(), String> {
        let Some((name, severity)) = value.split_once(':') else {
            return Err(format!(
                "invalid value '{value}' for -Xwarning-level: expected <NAME>:<error|warning|disabled>"
            ));
        };
        let Some(name) = WarningName::parse(name) else {
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
    /// level still wins, while global `-Werror` retains warning severity and fails afterward.
    pub fn command_line_disposition(&self, name: WarningName) -> WarningDisposition {
        match self.configured_level(name) {
            Some(WarningLevel::Disabled) => WarningDisposition::Disabled,
            Some(WarningLevel::Warning) => WarningDisposition::Warning,
            Some(WarningLevel::Error) => WarningDisposition::Error,
            None if self.compiler_default == WarningLevel::Error => {
                WarningDisposition::WarningAndFail
            }
            None => WarningDisposition::Warning,
        }
    }

    /// Policy for compiler and module warnings that have no configurable identity yet.
    pub fn compiler_disposition(&self) -> WarningDisposition {
        match self.compiler_default {
            WarningLevel::Disabled => WarningDisposition::Disabled,
            WarningLevel::Warning => WarningDisposition::Warning,
            WarningLevel::Error => WarningDisposition::WarningAndFail,
        }
    }
}

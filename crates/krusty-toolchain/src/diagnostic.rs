//! Problems found in a project's files, with the toolchain's severities and positions.
//!
//! A message the Kotlin Toolchain also reports uses its words (its `SchemaBundle.properties`), so
//! `krusty-toolchain` and `kotlin` can be compared problem for problem. A refusal of something the
//! toolchain accepts but krusty does not implement says so and names krusty-toolchain.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::yaml::Position;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    WeakWarning,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warning => "WARNING",
            Self::WeakWarning => "WEAK WARNING",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// The file the problem is in; `None` for a problem with the project as a whole.
    pub file: Option<PathBuf>,
    /// Where in `file`; `None` for a problem with the file as a whole.
    pub position: Option<Position>,
}

impl Diagnostic {
    pub fn error(file: &Path, position: Option<Position>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            file: Some(file.to_path_buf()),
            position,
        }
    }

    /// A problem with the project as a whole rather than one file.
    pub fn project_error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            file: None,
            position: None,
        }
    }

    pub fn warning(
        severity: Severity,
        file: &Path,
        position: Option<Position>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            message: message.into(),
            file: Some(file.to_path_buf()),
            position,
        }
    }
}

impl fmt::Display for Diagnostic {
    /// `<file>:<line>:<column>: <SEVERITY>: <message>`, the line and column omitted for a whole-file
    /// problem and the location omitted for a whole-project one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{}", file.display())?;
            if let Some(position) = self.position {
                write!(f, ":{}:{}", position.line, position.column)?;
            }
            f.write_str(": ")?;
        }
        write!(f, "{}: {}", self.severity.label(), self.message)
    }
}

/// The problems collected while reading a project, in the order they were found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics(Vec<Diagnostic>);

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.0.push(diagnostic);
    }

    pub fn has_errors(&self) -> bool {
        self.errors() > 0
    }

    pub fn errors(&self) -> usize {
        self.0
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .count()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

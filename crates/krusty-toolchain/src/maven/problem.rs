//! Why an artifact could not be (completely) read, where the reason is: the metadata file and, when
//! its parser says, the line and column in it.

use std::path::{Path, PathBuf};

use crate::diagnostic::Diagnostic;
use crate::yaml::Position;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Problem {
    /// The metadata file the problem is in; `None` when it is in no file (an artifact that no
    /// repository holds).
    pub file: Option<PathBuf>,
    /// Where in `file`; `None` for a problem with the file as a whole.
    pub position: Option<Position>,
    pub message: String,
}

impl Problem {
    pub fn in_file(file: &Path, position: Option<Position>, message: impl Into<String>) -> Self {
        Self {
            file: Some(file.to_path_buf()),
            position,
            message: message.into(),
        }
    }

    pub fn general(message: impl Into<String>) -> Self {
        Self {
            file: None,
            position: None,
            message: message.into(),
        }
    }

    /// The error a command reports for it.
    pub fn diagnostic(&self) -> Diagnostic {
        match &self.file {
            Some(file) => Diagnostic::error(file, self.position, &self.message),
            None => Diagnostic::project_error(&self.message),
        }
    }
}

/// Why a metadata file's text is not a POM or module metadata, and where, when the parser says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub position: Option<Position>,
    pub message: String,
}

impl ParseError {
    /// A problem at `line` and `column` (counted from 1; a parser's column 0, before the line's
    /// first character, is its first column), whose `message` is a parser's, with the `suffix` it
    /// appends for the same place removed.
    pub fn at(line: usize, column: usize, message: &str, suffix: &str) -> Self {
        Self {
            position: Some(Position {
                line,
                column: column.max(1),
            }),
            message: message.strip_suffix(suffix).unwrap_or(message).to_string(),
        }
    }
}

impl From<String> for ParseError {
    fn from(message: String) -> Self {
        Self {
            position: None,
            message,
        }
    }
}

impl From<&str> for ParseError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

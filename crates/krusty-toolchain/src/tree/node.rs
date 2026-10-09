//! The value tree a build file reads into, before and after refinement.

use std::path::PathBuf;

use super::contexts::Contexts;
use crate::schema::{Derivation, EnumType, ObjectType, Property};
use crate::yaml::Position;

/// A file the tree was read from, by index into [`Files`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId(u32);

/// The files a set of trees was read from.
#[derive(Debug, Default)]
pub struct Files(Vec<PathBuf>);

impl Files {
    pub fn add(&mut self, path: PathBuf) -> FileId {
        if let Some(index) = self.0.iter().position(|known| *known == path) {
            return FileId(index as u32);
        }
        self.0.push(path);
        FileId(self.0.len() as u32 - 1)
    }

    pub fn contains(&self, path: &std::path::Path) -> bool {
        self.0.iter().any(|known| known == path)
    }

    /// Every file, in the order they were added.
    pub fn ids(&self) -> impl Iterator<Item = FileId> {
        (0..self.0.len() as u32).map(FileId)
    }

    pub fn path(&self, file: FileId) -> &std::path::Path {
        &self.0[file.0 as usize]
    }

    /// The file's name, as a value's origin is printed.
    pub fn name(&self, file: FileId) -> String {
        self.path(file)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Where a value comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trace {
    /// The schema's default.
    Default,
    /// Written in a file.
    File { file: FileId, position: Position },
    /// Copied or derived from another value: how, and the file that value is written in.
    Derived {
        description: String,
        source: Option<FileId>,
    },
}

impl Trace {
    pub fn is_default(&self) -> bool {
        matches!(self, Trace::Default)
            || matches!(self, Trace::Derived { description, .. } if description.starts_with("default, "))
    }

    /// The file the value is written in, following derivations to their source.
    pub fn file(&self) -> Option<FileId> {
        match self {
            Trace::Default => None,
            Trace::File { file, .. } => Some(*file),
            Trace::Derived { source, .. } => *source,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub value: Value,
    pub trace: Trace,
    pub contexts: Contexts,
}

#[derive(Clone, Debug)]
pub enum Value {
    /// A value that could not be read; the problem was reported.
    Error,
    Null,
    Boolean(bool),
    Int(i32),
    String(String),
    Enum(&'static EnumType, &'static str),
    Path(PathBuf),
    List(Vec<Node>),
    /// An object (with its type) or a map (without one).
    Mapping {
        object: Option<&'static ObjectType>,
        entries: Vec<Entry>,
    },
    /// A default that names another property of the same object.
    Reference {
        path: &'static [&'static str],
        derivation: Option<Derivation>,
    },
    /// A value read by another phase, kept unread.
    Opaque,
}

/// One key of a mapping and its value.
#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    /// The property the key names, in an object.
    pub property: Option<&'static Property>,
    pub key_trace: Trace,
    pub value: Node,
}

impl Node {
    pub fn new(value: Value, trace: Trace, contexts: Contexts) -> Self {
        Self {
            value,
            trace,
            contexts,
        }
    }

    pub fn entry(&self, key: &str) -> Option<&Entry> {
        match &self.value {
            Value::Mapping { entries, .. } => entries.iter().find(|entry| entry.key == key),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Node> {
        self.entry(key).map(|entry| &entry.value)
    }
}

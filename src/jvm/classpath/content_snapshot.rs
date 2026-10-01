//! Content identity captured when a classpath is constructed.
//!
//! Path spelling is not a snapshot. A jar or directory at an unchanged path can be replaced, and a
//! later `Classpath` built from those same paths records the new contents.

use super::{Classpath, EntryStamp};

/// Stamps of every classpath entry at construction.
///
/// Two classpaths with equal path lists and unequal snapshots resolved different bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct ClasspathContentSnapshot {
    stamps: Vec<Option<EntryStamp>>,
}

impl Classpath {
    /// The content snapshot this classpath captured at construction.
    pub fn content_snapshot(&self) -> ClasspathContentSnapshot {
        ClasspathContentSnapshot {
            stamps: self.snapshot.clone(),
        }
    }
}

//! Reading one class from one classpath entry, and the per-entry class cache those reads fill.

use super::{Classpath, Entry};
use crate::jvm::classreader::{parse_class, ClassInfo, ReadError};
use crate::types::TypeName;
use std::sync::Arc;

impl Classpath {
    /// Read and parse class `internal` (`name` is its `.class` file name) from entry `e`, bypassing
    /// every cache. Only a class whose Kotlin metadata does not decode is an error; an absent or
    /// otherwise unreadable class is `None`.
    pub(super) fn read_entry_class(
        &self,
        e: &Entry,
        internal: &str,
        name: &str,
    ) -> Result<Option<Arc<ClassInfo>>, ReadError> {
        let bytes = match e {
            Entry::Dir(d) => std::fs::read(d.join(name)).ok(),
            Entry::Jar(j) => self.jar_entry(j, name),
            // The JDK jimage stores classes uncompressed — seek-read the class via a one-time
            // name→(offset,size) index so JDK type members (String, collections, …) resolve.
            Entry::Jimage(_) => self.jimage_bytes(internal),
            Entry::CtSym { path, release } => self.ct_sym_bytes(path, *release, internal),
        };
        // A DIRECTORY entry on a case-INSENSITIVE filesystem (macOS APFS) happily serves
        // `java/lang/error.class` for `Error.class` — verify the parsed class IS the
        // requested one (JVM names are case-sensitive; `error` must not resolve to `Error`).
        let Some(bytes) = bytes else { return Ok(None) };
        let class = match parse_class(&bytes) {
            Ok(class) => class,
            Err(error @ ReadError::BadKotlinMetadata(_)) => return Err(error),
            Err(_) => return Ok(None),
        };
        Ok(class
            .this_class_matches(internal)
            .then(|| std::sync::Arc::new(class)))
    }

    /// Class `internal` as the completely catalogued entry `entry_id` stores it, through the same
    /// process-global per-entry cache [`Self::find_name`] fills, so an index that reads a facade and
    /// the lookup that later resolves against it share one parse.
    pub(super) fn complete_entry_class(
        &self,
        entry_id: usize,
        internal: TypeName,
    ) -> Result<Option<Arc<ClassInfo>>, ReadError> {
        let (Some(e), Some(l2)) = (self.entries.get(entry_id), self.entry_caches.get(entry_id))
        else {
            return Ok(None);
        };
        l2.get_or_build(internal, || {
            let internal = crate::jvm::names::classfile_internal_name_of(internal);
            self.read_entry_class(e, internal, &format!("{internal}.class"))
        })
    }
}

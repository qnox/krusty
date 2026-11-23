//! How a multiplatform fragment's session sees a library the build names for it
//! (`-Xfragment-dependency`): kotlinc loads each one as a metadata KLIB, and what it cannot load
//! stays a plain path, which on the JVM is a class directory or jar for that fragment.

use std::path::Path;

use crate::klib::KlibArchive;

/// What a path named as a fragment's library is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FragmentLibraryKind {
    /// Nothing exists at the path.
    Missing,
    /// Something exists there that is not a KLIB.
    NotKlib,
    /// A KLIB, with the `unique_name` its manifest declares.
    Klib { unique_name: Option<String> },
}

/// Classify `path` as kotlinc's KLIB loader does: a KLIB is an archive or an unpacked directory
/// whose container and manifest read.
pub fn classify_fragment_library(path: &Path) -> FragmentLibraryKind {
    if !path.exists() {
        return FragmentLibraryKind::Missing;
    }
    match KlibArchive::open(path).and_then(|archive| archive.manifest()) {
        Ok(manifest) => FragmentLibraryKind::Klib {
            unique_name: manifest.unique_name().map(str::to_string),
        },
        Err(_) => FragmentLibraryKind::NotKlib,
    }
}

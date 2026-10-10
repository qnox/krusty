//! Reading the provider's declarations out of the KLIB containers a compilation selects.
//!
//! The caller names the libraries; nothing here knows which target they were built for. A library
//! that cannot be opened or decoded rejects the whole set: an unreadable dependency surfaces here,
//! with its path, instead of later as an unresolved reference.

use std::path::{Path, PathBuf};

use crate::klib::{KlibArchive, KlibError};
use crate::metadata::semantic::{
    parse_package_fragment_checked, KotlinPackage, PackageFragmentDecodeError,
};

use super::{KlibLibraries, KlibLibraryError};

/// Why the selected libraries could not be published.
#[derive(Debug)]
pub enum KlibLibrariesOpenError {
    /// The container itself is unreadable.
    Archive(KlibError),
    /// A metadata fragment of `library` does not decode.
    Fragment {
        library: PathBuf,
        entry: String,
        error: PackageFragmentDecodeError,
    },
    /// A decoded declaration cannot be signed.
    Declaration(KlibLibraryError),
}

impl std::fmt::Display for KlibLibrariesOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Archive(error) => error.fmt(formatter),
            Self::Fragment {
                library,
                entry,
                error,
            } => write!(
                formatter,
                "cannot decode KLIB metadata fragment {entry} of {}: {error}",
                library.display()
            ),
            Self::Declaration(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for KlibLibrariesOpenError {}

impl KlibLibraries {
    /// Publish every package fragment of the KLIBs at `libraries`, zipped or unpacked.
    pub fn open(libraries: &[PathBuf]) -> Result<Self, KlibLibrariesOpenError> {
        let mut packages = Vec::new();
        for library in libraries {
            let archive = KlibArchive::open(library).map_err(KlibLibrariesOpenError::Archive)?;
            packages.extend(package_fragments(library, &archive)?);
        }
        Self::from_packages(packages).map_err(KlibLibrariesOpenError::Declaration)
    }
}

fn package_fragments(
    library: &Path,
    archive: &KlibArchive,
) -> Result<Vec<(Vec<String>, KotlinPackage)>, KlibLibrariesOpenError> {
    archive
        .package_fragments()
        .into_iter()
        .map(|fragment| {
            let bytes = archive
                .read(&fragment.entry)
                .map_err(KlibLibrariesOpenError::Archive)?;
            let package = parse_package_fragment_checked(&bytes).map_err(|error| {
                KlibLibrariesOpenError::Fragment {
                    library: library.to_path_buf(),
                    entry: fragment.entry.clone(),
                    error,
                }
            })?;
            let segments = if fragment.package_fqname.is_empty() {
                Vec::new()
            } else {
                fragment
                    .package_fqname
                    .split('.')
                    .map(str::to_owned)
                    .collect()
            };
            Ok((segments, package))
        })
        .collect()
}

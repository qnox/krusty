//! Reading the provider's declarations out of the KLIB containers a compilation selects.
//!
//! The compilation assigns the selected platform stdlib its trusted input role; manifests validate
//! that assignment but never grant it. A library that cannot be opened or decoded rejects the whole
//! set here instead of surfacing later as an unresolved reference.

use std::path::{Path, PathBuf};

use crate::klib::{KlibArchive, KlibError};
use crate::metadata::semantic::{
    parse_package_fragment_checked, KotlinPackage, PackageFragmentDecodeError,
};

use super::inventory::PackageInventory;
use super::parameter_defaults::ParameterDefaults;
use super::{KlibLibraries, KlibLibraryError};
use crate::metadata::klib_ir::{read_declaration_trees_where, KlibIrDecodeError};

/// One compiler input KLIB and the authority the compilation assigned to it.
pub struct KlibLibraryInput {
    path: PathBuf,
    role: KlibLibraryRole,
}

#[derive(Clone, Copy)]
enum KlibLibraryRole {
    Dependency,
    PlatformStdlib,
}

impl KlibLibraryInput {
    /// An ordinary dependency, never authoritative for compiler-owned declarations.
    pub fn dependency(path: PathBuf) -> Self {
        Self {
            path,
            role: KlibLibraryRole::Dependency,
        }
    }

    /// The exact platform stdlib selected by compilation input assembly.
    pub fn platform_stdlib(path: PathBuf) -> Self {
        Self {
            path,
            role: KlibLibraryRole::PlatformStdlib,
        }
    }
}

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
    /// The IR declarations of `library` do not decode.
    Declarations {
        library: PathBuf,
        error: KlibIrDecodeError,
    },
    /// A compilation-designated platform stdlib does not carry the platform stdlib manifest.
    PlatformStdlibManifest { library: PathBuf },
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
            Self::Declarations { library, error } => write!(
                formatter,
                "cannot decode the KLIB IR declarations of {}: {error}",
                library.display()
            ),
            Self::PlatformStdlibManifest { library } => write!(
                formatter,
                "designated platform stdlib {} has no stdlib platform manifest",
                library.display()
            ),
            Self::Declaration(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for KlibLibrariesOpenError {}

impl KlibLibraries {
    /// Publish every package fragment of the typed KLIB inputs, zipped or unpacked.
    pub fn open(libraries: &[KlibLibraryInput]) -> Result<Self, KlibLibrariesOpenError> {
        let mut packages = Vec::new();
        let mut archives = Vec::with_capacity(libraries.len());
        for input in libraries {
            let library = &input.path;
            let archive = KlibArchive::open(library).map_err(KlibLibrariesOpenError::Archive)?;
            let manifest = archive
                .manifest()
                .map_err(KlibLibrariesOpenError::Archive)?;
            let compiler_builtin_owner = match input.role {
                KlibLibraryRole::Dependency => false,
                KlibLibraryRole::PlatformStdlib
                    if manifest.unique_name() == Some("stdlib")
                        && manifest.builtins_platform().is_some() =>
                {
                    true
                }
                KlibLibraryRole::PlatformStdlib => {
                    return Err(KlibLibrariesOpenError::PlatformStdlibManifest {
                        library: library.clone(),
                    });
                }
            };
            packages.extend(package_fragments(
                library,
                &archive,
                compiler_builtin_owner,
            )?);
            archives.push(archive);
        }
        let mut inventory = PackageInventory::from_packages(packages)
            .map_err(KlibLibrariesOpenError::Declaration)?;
        // Only a declaration metadata marks as defaulted has a default to read from the IR.
        let wanted = inventory.defaulted_top_levels();
        let mut defaults = ParameterDefaults::default();
        for (input, archive) in libraries.iter().zip(&archives) {
            let library = &input.path;
            let trees =
                read_declaration_trees_where(archive, |signature| wanted.contains(signature))
                    .map_err(|error| KlibLibrariesOpenError::Declarations {
                        library: library.to_path_buf(),
                        error,
                    })?;
            defaults.add_library(&trees);
        }
        inventory.attach_defaults(&defaults);
        Ok(Self::over(std::rc::Rc::new(inventory)))
    }
}

fn package_fragments(
    library: &Path,
    archive: &KlibArchive,
    compiler_builtin_owner: bool,
) -> Result<Vec<(Vec<String>, KotlinPackage, bool)>, KlibLibrariesOpenError> {
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
            Ok((segments, package, compiler_builtin_owner))
        })
        .collect()
}

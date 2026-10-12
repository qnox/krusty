//! Reading the provider's declarations out of the KLIB containers a compilation selects.
//!
//! The compilation assigns the selected platform stdlib its trusted input role. A manifest is
//! self-authored archive data and cannot grant or revoke that role. A library that cannot be opened
//! or decoded rejects the whole set here instead of surfacing later as an unresolved reference.

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

#[derive(Clone, Copy, Eq, PartialEq)]
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
    /// Compilation input assembly designated more than one archive as the platform stdlib.
    MultiplePlatformStdlibs { first: PathBuf, second: PathBuf },
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
    /// A decoded declaration cannot be signed.
    Declaration(KlibLibraryError),
}

impl std::fmt::Display for KlibLibrariesOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MultiplePlatformStdlibs { first, second } => write!(
                formatter,
                "multiple platform stdlib KLIB inputs: {} and {}",
                first.display(),
                second.display()
            ),
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
            Self::Declaration(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for KlibLibrariesOpenError {}

impl KlibLibraries {
    /// Publish every package fragment of the typed KLIB inputs, zipped or unpacked.
    pub fn open(libraries: &[KlibLibraryInput]) -> Result<Self, KlibLibrariesOpenError> {
        let mut platform_stdlib = None;
        for input in libraries {
            if input.role != KlibLibraryRole::PlatformStdlib {
                continue;
            }
            if let Some(first) = platform_stdlib {
                return Err(KlibLibrariesOpenError::MultiplePlatformStdlibs {
                    first,
                    second: input.path.clone(),
                });
            }
            platform_stdlib = Some(input.path.clone());
        }
        let mut packages = Vec::new();
        let mut archives = Vec::with_capacity(libraries.len());
        for input in libraries {
            let library = &input.path;
            let archive = KlibArchive::open(library).map_err(KlibLibrariesOpenError::Archive)?;
            let _manifest = archive
                .manifest()
                .map_err(KlibLibrariesOpenError::Archive)?;
            // Authority comes from compilation input assembly, which selected this exact archive
            // as the platform stdlib. Manifests are self-authored dependency data: Native, JS and
            // Wasm also stamp different values, so neither `unique_name` nor `builtins_platform`
            // can grant or revoke the compiler-owned role.
            let compiler_builtin_owner = matches!(input.role, KlibLibraryRole::PlatformStdlib);
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
        let wanted = inventory.default_selection();
        let mut defaults = ParameterDefaults::default();
        for (input, archive) in libraries.iter().zip(&archives) {
            let library = &input.path;
            let trees =
                read_declaration_trees_where(archive, |signature| wanted.contains(signature))
                    .map_err(|error| KlibLibrariesOpenError::Declarations {
                        library: library.to_path_buf(),
                        error,
                    })?;
            defaults.add_library(
                &trees,
                &wanted,
                input.role == KlibLibraryRole::PlatformStdlib,
            );
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

//! Reading the provider's declarations out of the KLIB containers a compilation selects.
//!
//! The compilation assigns the selected platform stdlib its trusted input role. A manifest is
//! self-authored archive data and cannot grant or revoke that role. A library that cannot be opened
//! or decoded rejects the whole set here instead of surfacing later as an unresolved reference.

use std::path::{Path, PathBuf};

use crate::klib::{KlibArchive, KlibError};
use crate::metadata::semantic::{parse_package_fragment_checked, PackageFragmentDecodeError};

use super::inventory::{ArchivePackageInput, PackageInventory};
use super::parameter_defaults::ParameterDefaults;
use super::{
    KlibBodyLibrary, KlibDeclarationBodies, KlibDeclarationBodiesError, KlibLibraries,
    KlibLibraryError,
};
use crate::metadata::klib_ir::{
    read_declaration_trees, read_inlinable_declaration_trees_where, KlibIrDecodeError,
};

/// One compiler input KLIB and the authority the compilation assigned to it.
pub struct KlibLibraryInput {
    path: PathBuf,
    role: KlibLibraryRole,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum KlibLibraryRole {
    Dependency,
    Friend,
    PlatformStdlib(KlibStdlibSemantics),
}

impl KlibLibraryRole {
    const fn snapshot_role(self) -> crate::libraries::DependencyBodyProviderRole {
        match self {
            Self::Dependency => crate::libraries::DependencyBodyProviderRole::Dependency,
            Self::Friend => crate::libraries::DependencyBodyProviderRole::Friend,
            Self::PlatformStdlib(KlibStdlibSemantics::Native) => {
                crate::libraries::DependencyBodyProviderRole::PlatformNative
            }
            Self::PlatformStdlib(KlibStdlibSemantics::Wasm) => {
                crate::libraries::DependencyBodyProviderRole::PlatformWasm
            }
            Self::PlatformStdlib(KlibStdlibSemantics::Plain) => {
                crate::libraries::DependencyBodyProviderRole::PlatformPlain
            }
        }
    }
}

/// Compiler-owned semantics carried by the selected platform stdlib KLIB.
///
/// This is deliberately a KLIB input fact rather than a compilation target. Input assembly owns
/// the mapping from its target to one of these closed semantic families; the provider therefore
/// remains reusable without depending on target selection or admitting a non-KLIB target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KlibStdlibSemantics {
    /// Kotlin/Native compiler-owned declarations.
    Native,
    /// Kotlin/Wasm compiler-owned declarations.
    Wasm,
    /// A KLIB platform stdlib with no compiler-owned intrinsic declarations realized here.
    Plain,
}

impl KlibLibraryInput {
    /// An ordinary dependency, never authoritative for compiler-owned declarations.
    pub fn dependency(path: PathBuf) -> Self {
        Self {
            path,
            role: KlibLibraryRole::Dependency,
        }
    }

    /// A dependency whose internal declarations are visible to this compilation.
    pub fn friend(path: PathBuf) -> Self {
        Self {
            path,
            role: KlibLibraryRole::Friend,
        }
    }

    /// The exact platform stdlib selected by compilation input assembly.
    pub fn platform_stdlib(path: PathBuf, semantics: KlibStdlibSemantics) -> Self {
        Self {
            path,
            role: KlibLibraryRole::PlatformStdlib(semantics),
        }
    }
}

/// One opened input before module authority selects the records visible to this compilation.
struct LibraryRecord<'a> {
    input: &'a KlibLibraryInput,
    module: String,
    archive: KlibArchive,
    compiler_builtin_owner: bool,
    internal_friend: bool,
    intrinsic_platform: Option<super::target_intrinsics::TargetIntrinsicPlatform>,
    /// The producer-declared Native interop signature family. Unlike platform-stdlib authority,
    /// this is archive format data: it affects every serialized declaration's IdSignature.
    native_interop_library: bool,
}

/// The exact opened KLIB records selected by one compilation input assembly.
///
/// Target-specific input assembly may derive backend facts from these same archives before
/// [`into_libraries`](Self::into_libraries) publishes their declarations. It must not reopen the
/// paths: every consumed channel is frozen by [`KlibArchive::open`], and the final publication
/// guard proves that the selected paths still name that generation.
pub(crate) struct OpenedKlibLibraries<'a> {
    records: Vec<LibraryRecord<'a>>,
    platform_module: Option<String>,
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
    /// The selected archives cannot be indexed as one exact module-qualified body graph.
    Bodies(KlibDeclarationBodiesError),
    /// A selected KLIB has no stable module identity for cross-archive joins.
    MissingUniqueName { library: PathBuf },
    /// A selected KLIB's consumed channels changed after this open froze and decoded them.
    ChangedDuringOpen { library: PathBuf },
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
            Self::Bodies(error) => error.fmt(formatter),
            Self::MissingUniqueName { library } => write!(
                formatter,
                "KLIB {} has no manifest unique_name",
                library.display()
            ),
            Self::ChangedDuringOpen { library } => write!(
                formatter,
                "KLIB {} changed while its declarations, bodies, and backend facts were being opened",
                library.display()
            ),
        }
    }
}

impl std::error::Error for KlibLibrariesOpenError {}

impl KlibLibraries {
    /// Publish every package fragment of the typed KLIB inputs, zipped or unpacked.
    pub fn open(libraries: &[KlibLibraryInput]) -> Result<Self, KlibLibrariesOpenError> {
        OpenedKlibLibraries::open(libraries)?.into_libraries()
    }
}

impl<'a> OpenedKlibLibraries<'a> {
    /// Open and select the typed inputs once, before any semantic or backend facts are published.
    pub(crate) fn open(libraries: &'a [KlibLibraryInput]) -> Result<Self, KlibLibrariesOpenError> {
        Self::open_snapshot(libraries, false)
    }

    /// Open Native inputs with their target import channels in the same immutable snapshot.
    pub(crate) fn open_native(
        libraries: &'a [KlibLibraryInput],
    ) -> Result<Self, KlibLibrariesOpenError> {
        Self::open_snapshot(libraries, true)
    }

    fn open_snapshot(
        libraries: &'a [KlibLibraryInput],
        native_imports: bool,
    ) -> Result<Self, KlibLibrariesOpenError> {
        let mut platform_stdlib = None;
        for input in libraries {
            let KlibLibraryRole::PlatformStdlib(_) = input.role else {
                continue;
            };
            if let Some(first) = platform_stdlib {
                return Err(KlibLibrariesOpenError::MultiplePlatformStdlibs {
                    first,
                    second: input.path.clone(),
                });
            }
            platform_stdlib = Some(input.path.clone());
        }
        let mut records = Vec::with_capacity(libraries.len());
        for input in libraries {
            let library = &input.path;
            let archive = if native_imports {
                KlibArchive::open_with_native_imports(library)
            } else {
                KlibArchive::open(library)
            }
            .map_err(KlibLibrariesOpenError::Archive)?;
            let manifest = archive
                .manifest()
                .map_err(KlibLibrariesOpenError::Archive)?;
            let module = manifest.unique_name().map(str::to_owned).ok_or_else(|| {
                KlibLibrariesOpenError::MissingUniqueName {
                    library: library.to_path_buf(),
                }
            })?;
            // Authority comes from compilation input assembly, which selected this exact archive
            // as the platform stdlib. Manifests are self-authored dependency data: Native, JS and
            // Wasm also stamp different values, so neither `unique_name` nor `builtins_platform`
            // can grant or revoke the compiler-owned role.
            let intrinsic_platform = match input.role {
                KlibLibraryRole::PlatformStdlib(semantics) => semantics.intrinsic_platform(),
                KlibLibraryRole::Dependency | KlibLibraryRole::Friend => None,
            };
            let compiler_builtin_owner = matches!(input.role, KlibLibraryRole::PlatformStdlib(_));
            let internal_friend = matches!(input.role, KlibLibraryRole::Friend);
            let native_interop_library = manifest.get("interop") == Some("true");
            records.push(LibraryRecord {
                input,
                module,
                archive,
                compiler_builtin_owner,
                internal_friend,
                intrinsic_platform,
                native_interop_library,
            });
        }
        let platform_module = records
            .iter()
            .find(|record| matches!(record.input.role, KlibLibraryRole::PlatformStdlib(_)))
            .map(|record| record.module.clone());
        // Select archive authority once, before any declaration, default, intrinsic, body, or
        // snapshot channel is published. The compilation-designated platform archive owns its
        // manifest module identity; a dependency or friend cannot contribute another facet of
        // that module merely because it appears before or after the platform input.
        let records = records
            .into_iter()
            .filter(|record| {
                platform_module.as_deref() != Some(record.module.as_str())
                    || matches!(record.input.role, KlibLibraryRole::PlatformStdlib(_))
            })
            .collect::<Vec<_>>();
        Ok(Self {
            records,
            platform_module,
        })
    }

    /// The selected archive paths and their frozen contents, in compiler input order.
    pub(crate) fn archives(&self) -> impl Iterator<Item = (&Path, &KlibArchive)> {
        self.records
            .iter()
            .map(|record| (record.input.path.as_path(), &record.archive))
    }

    /// Decode and publish the common KLIB provider from these exact opened records.
    pub(crate) fn into_libraries(self) -> Result<KlibLibraries, KlibLibrariesOpenError> {
        let Self {
            records,
            platform_module,
        } = self;
        let mut packages = Vec::new();
        for record in &records {
            packages.extend(package_fragments(
                &record.input.path,
                &record.archive,
                record.compiler_builtin_owner,
                record.internal_friend,
                &record.module,
                record.intrinsic_platform,
                record.native_interop_library,
            )?);
        }
        let mut inventory = PackageInventory::from_target_package_inputs(packages)
            .map_err(KlibLibrariesOpenError::Declaration)?;
        // Only a declaration metadata marks as defaulted has a default to read from the IR.
        let wanted = inventory.default_selection();
        let inline_wanted = inventory.inline_body_selection();
        let mut defaults = ParameterDefaults::default();
        let mut body_libraries = Vec::new();
        let mut inline_body_libraries = Vec::new();
        let mut body_provider_snapshots = Vec::new();
        for record in &records {
            let library = &record.input.path;
            let body_provider = body_provider_snapshot(record)?;
            let manifest = record
                .archive
                .manifest()
                .map_err(KlibLibrariesOpenError::Archive)?;
            // A metadata KLIB (`-Xmetadata-klib`) deliberately carries linkdata without an IR
            // channel. Its declaration-level default flags remain useful, but it publishes no
            // constant realization to attach. Once any IR entry exists, keep treating the channel
            // as an all-or-nothing serialized format so a partial/corrupt library is rejected.
            if record.archive.ir_entries().is_empty() {
                // A metadata-only archive still owns its producer-declared dependency edges. Keep
                // an empty graph node so body lookup can traverse through it to a reachable
                // body-carrying provider without treating this archive itself as one.
                let body_library = KlibBodyLibrary::from_manifest(
                    &manifest,
                    crate::metadata::klib_ir::KlibIrModuleTrees::default(),
                )
                .ok_or_else(|| KlibLibrariesOpenError::MissingUniqueName {
                    library: library.to_path_buf(),
                })?;
                body_libraries.push(body_library);
                let inline_library = KlibBodyLibrary::from_manifest(
                    &manifest,
                    crate::metadata::klib_ir::KlibIrModuleTrees::default(),
                )
                .ok_or_else(|| KlibLibrariesOpenError::MissingUniqueName {
                    library: library.to_path_buf(),
                })?;
                inline_body_libraries.push(inline_library);
                body_provider_snapshots.push((record.module.clone(), body_provider));
                continue;
            }
            // Ordinary IR remains authoritative for defaults, intrinsic bodies and general body
            // queries. Prepared-inline IR is decoded independently below: its declaration copies
            // are not allowed to masquerade as the module's complete ordinary IR.
            let main_trees = if record.archive.has_main_ir() {
                Some(read_declaration_trees(&record.archive).map_err(|error| {
                    KlibLibrariesOpenError::Declarations {
                        library: library.to_path_buf(),
                        error,
                    }
                })?)
            } else {
                None
            };
            if let Some(trees) = main_trees {
                inventory.attach_ir_intrinsics(&record.module, record.intrinsic_platform, &trees);
                defaults.add_library(
                    &record.module,
                    &trees,
                    &wanted,
                    record.compiler_builtin_owner,
                );
                let body_library =
                    KlibBodyLibrary::from_manifest(&manifest, trees).ok_or_else(|| {
                        KlibLibrariesOpenError::MissingUniqueName {
                            library: library.to_path_buf(),
                        }
                    })?;
                body_libraries.push(body_library);
            } else {
                let body_library = KlibBodyLibrary::from_manifest(
                    &manifest,
                    crate::metadata::klib_ir::KlibIrModuleTrees::default(),
                )
                .ok_or_else(|| KlibLibrariesOpenError::MissingUniqueName {
                    library: library.to_path_buf(),
                })?;
                body_libraries.push(body_library);
            }

            // Kotlin's prepared component exclusively owns cross-module inline copies. Ordinary
            // IR remains the complete module body/default channel above; absence of a prepared
            // component means this library publishes no cross-module inline plan.
            let inline_trees = if record.archive.has_inlinable_ir() {
                read_inlinable_declaration_trees_where(&record.archive, |signature| {
                    inline_wanted.contains(&record.module, signature)
                })
                .map_err(|error| KlibLibrariesOpenError::Declarations {
                    library: library.to_path_buf(),
                    error,
                })?
            } else {
                crate::metadata::klib_ir::KlibIrModuleTrees::default()
            };
            let inline_library = KlibBodyLibrary::from_manifest(&manifest, inline_trees)
                .ok_or_else(|| KlibLibrariesOpenError::MissingUniqueName {
                    library: library.to_path_buf(),
                })?;
            inline_body_libraries.push(inline_library);
            body_provider_snapshots.push((record.module.clone(), body_provider));
        }
        inventory.attach_defaults(&defaults);
        let bodies = KlibDeclarationBodies::from_body_libraries(body_libraries)
            .map_err(KlibLibrariesOpenError::Bodies)?;
        let inline_bodies = KlibDeclarationBodies::from_body_libraries(inline_body_libraries)
            .map_err(KlibLibrariesOpenError::Bodies)?;
        let body_snapshot = platform_module.map(|platform_module| {
            let reachable = bodies.body_provider_modules(&platform_module);
            crate::libraries::DependencyBodySnapshot::new(
                body_provider_snapshots
                    .into_iter()
                    .filter_map(|(module, snapshot)| {
                        reachable.contains(&module).then_some(snapshot)
                    })
                    .collect(),
            )
        });
        // Decoders read the immutable bytes frozen by `KlibArchive::open`, so they cannot combine
        // channels from two generations. Before publishing those decoded facts, prove that the
        // selected path still names that same consumed generation. A later module will make the
        // same check independently and the session snapshot will join the two fingerprints.
        for record in &records {
            validate_current_generation(record)?;
        }
        Ok(KlibLibraries::over_with_bodies_and_snapshot(
            std::rc::Rc::new(inventory),
            std::rc::Rc::new(bodies),
            std::rc::Rc::new(inline_bodies),
            body_snapshot,
        ))
    }
}

fn validate_current_generation(record: &LibraryRecord<'_>) -> Result<(), KlibLibrariesOpenError> {
    if !opened_generation_is_current(&record.input.path, &record.archive)? {
        return Err(KlibLibrariesOpenError::ChangedDuringOpen {
            library: record.input.path.clone(),
        });
    }
    Ok(())
}

/// Compare the immutable generation an open decoded with a fresh census and snapshot of the path.
/// Kept as one operation so the publication guard and its mutation regression exercise the same
/// authority check.
pub(super) fn opened_generation_is_current(
    library: &Path,
    opened: &KlibArchive,
) -> Result<bool, KlibLibrariesOpenError> {
    let current = opened
        .reopen_selected(library)
        .map_err(KlibLibrariesOpenError::Archive)?;
    let opened_fingerprint = opened
        .content_fingerprint()
        .map_err(KlibLibrariesOpenError::Archive)?;
    let current_fingerprint = current
        .content_fingerprint()
        .map_err(KlibLibrariesOpenError::Archive)?;
    Ok(opened_fingerprint == current_fingerprint)
}

/// Snapshot one active body provider. Inactive same-module records never reach this channel.
fn body_provider_snapshot(
    record: &LibraryRecord<'_>,
) -> Result<crate::libraries::DependencyBodyProviderSnapshot, KlibLibrariesOpenError> {
    let canonical_library = std::fs::canonicalize(&record.input.path).map_err(|source| {
        KlibLibrariesOpenError::Archive(KlibError::Io {
            operation: "canonicalize",
            path: record.input.path.clone(),
            source,
        })
    })?;
    let content_fingerprint = record
        .archive
        .content_fingerprint()
        .map_err(KlibLibrariesOpenError::Archive)?;
    let manifest = record
        .archive
        .manifest()
        .map_err(KlibLibrariesOpenError::Archive)?;
    Ok(crate::libraries::DependencyBodyProviderSnapshot::new(
        canonical_library,
        &record.module,
        manifest.abi_version(),
        manifest.compiler_version(),
        manifest.metadata_version(),
        content_fingerprint,
        record.input.role.snapshot_role(),
    ))
}

impl KlibStdlibSemantics {
    const fn intrinsic_platform(self) -> Option<super::target_intrinsics::TargetIntrinsicPlatform> {
        match self {
            Self::Native => Some(super::target_intrinsics::TargetIntrinsicPlatform::Native),
            Self::Wasm => Some(super::target_intrinsics::TargetIntrinsicPlatform::Wasm),
            Self::Plain => None,
        }
    }
}

fn package_fragments(
    library: &Path,
    archive: &KlibArchive,
    compiler_builtin_owner: bool,
    internal_friend: bool,
    module: &str,
    intrinsic_platform: Option<super::target_intrinsics::TargetIntrinsicPlatform>,
    native_interop_library: bool,
) -> Result<Vec<ArchivePackageInput>, KlibLibrariesOpenError> {
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
            Ok(ArchivePackageInput {
                segments,
                package,
                compiler_builtin_owner,
                internal_friend,
                module: Some(module.to_owned()),
                intrinsic_platform,
                native_interop_library,
            })
        })
        .collect()
}

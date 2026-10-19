//! Optional serialized dependency-body capability of a semantic platform.
//!
//! KLIB providers keep the body graph beside the intrinsic declarations authenticated from the
//! same compilation inputs. Generic compiler orchestration consumes this joined view without
//! downcasting a provider or reopening target artifacts.

use std::path::PathBuf;

/// Compilation-assigned authority of one archive in the dependency-body producer graph.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum DependencyBodyProviderRole {
    Dependency,
    Friend,
    PlatformNative,
    PlatformWasm,
    PlatformPlain,
}

/// One exact KLIB archive participating in the dependency-body producer graph.
///
/// `role` is assigned by compilation input assembly, not read from the manifest. The canonical
/// path makes two spellings of the same selected archive one identity; the content fingerprint
/// distinguishes an archive rebuilt at that path, and the version stamps retain the toolchain
/// identity in the explicit contract. Metadata-only archives are included because their producer
/// edges can change which body provider is reachable.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct DependencyBodyProviderSnapshot {
    archive: PathBuf,
    unique_name: String,
    abi_version: Option<String>,
    compiler_version: Option<String>,
    metadata_version: Option<String>,
    content_fingerprint: [u8; 32],
    role: DependencyBodyProviderRole,
}

impl DependencyBodyProviderSnapshot {
    pub(crate) fn new(
        archive: PathBuf,
        unique_name: &str,
        abi_version: Option<&str>,
        compiler_version: Option<&str>,
        metadata_version: Option<&str>,
        content_fingerprint: [u8; 32],
        role: DependencyBodyProviderRole,
    ) -> Self {
        Self {
            archive,
            unique_name: unique_name.to_owned(),
            abi_version: abi_version.map(str::to_owned),
            compiler_version: compiler_version.map(str::to_owned),
            metadata_version: metadata_version.map(str::to_owned),
            content_fingerprint,
            role,
        }
    }

    pub(crate) fn has_content_fingerprint(&self, fingerprint: [u8; 32]) -> bool {
        self.content_fingerprint == fingerprint
    }
}

/// The exact producer graph from which one target session may retain dependency support.
///
/// The list contains every selected archive reachable from the platform stdlib through producer
/// manifest edges, including metadata-only edge nodes. Input order is not semantic, so construction
/// sorts the already canonical provider identities before comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DependencyBodySnapshot {
    providers: Box<[DependencyBodyProviderSnapshot]>,
}

impl DependencyBodySnapshot {
    pub(crate) fn new(mut providers: Vec<DependencyBodyProviderSnapshot>) -> Self {
        providers.sort_unstable();
        Self {
            providers: providers.into_boxed_slice(),
        }
    }
}

/// Exact KLIB bodies and compiler-owned declarations selected for one compilation.
#[derive(Clone, Copy)]
pub struct DependencyBodyFacts<'a> {
    bodies: &'a crate::klib_libraries::KlibDeclarationBodies,
    inline_bodies: &'a crate::klib_libraries::KlibDeclarationBodies,
    intrinsics: &'a crate::klib_libraries::KlibIntrinsicFacts,
    snapshot: Option<&'a DependencyBodySnapshot>,
}

impl<'a> DependencyBodyFacts<'a> {
    pub(crate) fn new(
        bodies: &'a crate::klib_libraries::KlibDeclarationBodies,
        inline_bodies: &'a crate::klib_libraries::KlibDeclarationBodies,
        intrinsics: &'a crate::klib_libraries::KlibIntrinsicFacts,
        snapshot: Option<&'a DependencyBodySnapshot>,
    ) -> Self {
        Self {
            bodies,
            inline_bodies,
            intrinsics,
            snapshot,
        }
    }

    pub(crate) fn bodies(self) -> &'a crate::klib_libraries::KlibDeclarationBodies {
        self.bodies
    }

    /// The prepared-inline copies, the only bodies a call of an inline function expands from.
    pub(crate) fn inline_bodies(self) -> &'a crate::klib_libraries::KlibDeclarationBodies {
        self.inline_bodies
    }

    pub(crate) fn intrinsics(self) -> &'a crate::klib_libraries::KlibIntrinsicFacts {
        self.intrinsics
    }

    pub(crate) fn snapshot(self) -> Option<&'a DependencyBodySnapshot> {
        self.snapshot
    }
}

//! The interop libraries a Native program is compiled and linked against.
//!
//! An interop library (a KLIB that `cinterop` made, `interop=true`) contributes two things to a
//! build. Its declarations are published to the frontend after the platform's own
//! ([`crate::klib_libraries::KlibOverlay`]), and a call of one of its C functions is lowered to a
//! direct call of the C symbol the declaration names. The link facts it carries for its C
//! libraries (`cinterop::link_facts`) are the libraries the program imports those symbols from.

use std::path::PathBuf;

use crate::klib::KlibArchive;
use crate::klib_libraries::KlibOverlay;
use crate::libraries::SemanticPlatform;

use super::cinterop::imported_libraries;
use super::linker::ImportLibrary;
use super::target::NativeTarget;

/// The interop libraries of one build, for one target.
pub struct InteropLibraries {
    paths: Vec<PathBuf>,
    imports: Vec<ImportLibrary>,
}

/// Why an interop library cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteropLibraryError(String);

impl std::fmt::Display for InteropLibraryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InteropLibraryError {}

impl InteropLibraries {
    /// Open the interop libraries at `paths` (`.klib` archives or unpacked directories) for
    /// `target`. A library that is not an interop library, or was not made for `target`, is
    /// refused.
    pub fn open(paths: &[PathBuf], target: NativeTarget) -> Result<Self, InteropLibraryError> {
        let Some(konan_target) = target.konan_name() else {
            return Err(InteropLibraryError(format!(
                "Kotlin/Native has no interop libraries for {target}"
            )));
        };
        let mut imports = Vec::new();
        for path in paths {
            let what = path.display().to_string();
            let archive =
                KlibArchive::open(path).map_err(|error| InteropLibraryError(error.to_string()))?;
            let manifest = archive
                .manifest()
                .map_err(|error| InteropLibraryError(error.to_string()))?;
            if manifest.get("interop") != Some("true") {
                return Err(InteropLibraryError(format!(
                    "{what} is not an interop library; the native backend links only those"
                )));
            }
            if !manifest.targets().contains(&konan_target) {
                return Err(InteropLibraryError(format!(
                    "{what} was made for {}, not {konan_target}",
                    manifest.targets().join(", ")
                )));
            }
            imports.extend(
                imported_libraries(&archive, konan_target, &what).map_err(InteropLibraryError)?,
            );
        }
        Ok(Self {
            paths: paths.to_vec(),
            imports,
        })
    }

    /// The libraries a program built against these imports C functions from, in library order.
    pub fn imports(&self) -> &[ImportLibrary] {
        &self.imports
    }

    /// `base` with these libraries' declarations published after its own.
    pub fn over(
        &self,
        base: Box<dyn SemanticPlatform>,
    ) -> Result<KlibOverlay, InteropLibraryError> {
        KlibOverlay::open(base, &self.paths).map_err(|error| InteropLibraryError(error.to_string()))
    }
}

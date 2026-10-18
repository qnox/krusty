//! The `cinterop` tool: a `.def` file and its headers to an interop library on disk, taking the
//! options Kotlin/Native's `cinterop` takes.
//!
//! The library is the one [`super::library`] builds, in Kotlin/Native's direct-call format, plus
//! the link facts of the C libraries the `.def` file's `linkerOpts` name ([`super::link_facts`]),
//! so that a Native program built against it later links with nothing but its libraries. The
//! headers are read from the build machine's include directories, below the sysroot `/`.

use std::path::{Path, PathBuf};

use super::def_file::DefFile;
use super::library::{interop_library, CinteropError, InteropRequest};
use super::link_facts::{self, LinkerOptions};
use super::preprocessor::IncludeDir;
use crate::klib::write::KlibStamp;
use crate::kotlin_version;
use crate::language_version::LanguageVersion;
use crate::native::target::NativeTarget;

/// One `cinterop` invocation, as Kotlin/Native's tool takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CinteropOptions {
    /// `-def`: the library definition file.
    pub def: Option<PathBuf>,
    /// `-header`: headers read after the `.def` file's own.
    pub headers: Vec<String>,
    /// `-pkg`: the package of the declarations, in place of the `.def` file's `package`.
    pub package: Option<String>,
    /// `-o`/`-output`: the library to write. `.klib` is appended unless it is already there or
    /// the library is written unpacked.
    pub output: PathBuf,
    /// `-target`.
    pub target: NativeTarget,
    /// `-compiler-option` (and the words of `-compiler-options`): after the `.def` file's
    /// `compilerOpts`.
    pub compiler_options: Vec<String>,
    /// `-linker-option` (and the words of `-linker-options`): after the `.def` file's
    /// `linkerOpts`.
    pub linker_options: Vec<String>,
    /// `-nopack`: write the library as a directory instead of a `.klib` archive.
    pub no_pack: bool,
    /// `-Xmodule-name`: the library's unique name, in place of the output's file name.
    pub module_name: Option<String>,
}

impl CinteropOptions {
    /// The options of an invocation that names only `output`, for `target`.
    pub fn new(output: PathBuf, target: NativeTarget) -> Self {
        Self {
            def: None,
            headers: Vec::new(),
            package: None,
            output,
            target,
            compiler_options: Vec::new(),
            linker_options: Vec::new(),
            no_pack: false,
            module_name: None,
        }
    }
}

/// Build the interop library `options` describe and write it; returns where it was written.
pub fn build_interop_library(options: &CinteropOptions) -> Result<PathBuf, CinteropError> {
    let def =
        match &options.def {
            Some(path) => DefFile::parse(&std::fs::read_to_string(path).map_err(|error| {
                CinteropError(format!("cannot read {}: {error}", path.display()))
            })?),
            None => DefFile::default(),
        };
    let Some(konan_target) = options.target.konan_name() else {
        return Err(CinteropError(format!(
            "Kotlin/Native has no interop library format for {}",
            options.target
        )));
    };
    let stem = |path: &Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .map(|name| {
                name.strip_suffix(".klib")
                    .map(str::to_string)
                    .unwrap_or(name)
            })
    };
    let library_name = stem(&options.output)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| CinteropError(format!("`{}` names no library", options.output.display())))?;
    let unique_name = options.module_name.clone().unwrap_or(library_name.clone());
    // The package: `-pkg`, else the `.def` file's `package`, else the `.def` file's name, as
    // Kotlin/Native chooses it.
    let default_package = match (&options.package, &options.def) {
        (Some(package), _) => package.clone(),
        (None, Some(path)) if def.value("package", "linux", konan_target).is_none() => stem(path)
            .map(|name| {
                name.strip_suffix(".def")
                    .map(str::to_string)
                    .unwrap_or(name)
            })
            .unwrap_or_default(),
        (None, Some(_)) => String::new(),
        (None, None) => {
            return Err(CinteropError(
                "cinterop needs `-pkg` when no `-def` file names the package".to_string(),
            ))
        }
    };
    let mut def = def;
    if let Some(package) = &options.package {
        def.set_package(package);
    }

    let mut linker_words = def
        .list("linkerOpts", "linux", konan_target)
        .map_err(|error| CinteropError(format!("linkerOpts: {error}")))?;
    linker_words.extend(options.linker_options.iter().cloned());
    let imports = link_facts::capture(&LinkerOptions::parse(&linker_words)?, options.target)?;

    let mut writer = interop_library(InteropRequest {
        def: &def,
        unique_name: &unique_name,
        default_package: &default_package,
        target: options.target,
        system_dirs: system_include_dirs(options.target),
        sysroot: Some(PathBuf::from("/")),
        stamp: KlibStamp::new(kotlin_version::target(), LanguageVersion::V2_4),
        extra_headers: options.headers.clone(),
        extra_compiler_options: options.compiler_options.clone(),
    })?;
    if !imports.is_empty() {
        writer.add_file(
            &link_facts::entry(konan_target),
            link_facts::encode(&imports)?,
        );
    }

    let written = |error: std::io::Error, path: &Path| {
        CinteropError(format!("cannot write {}: {error}", path.display()))
    };
    if options.no_pack {
        let root = options.output.clone();
        if root.exists() {
            std::fs::remove_dir_all(&root).map_err(|error| written(error, &root))?;
        }
        writer
            .write_directory(&root)
            .map_err(|error| written(error, &root))?;
        Ok(root)
    } else {
        let path = if options.output.extension().is_some_and(|ext| ext == "klib") {
            options.output.clone()
        } else {
            let mut name = options.output.clone().into_os_string();
            name.push(".klib");
            PathBuf::from(name)
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| written(error, parent))?;
        }
        writer
            .write_archive(&path)
            .map_err(|error| written(error, &path))?;
        Ok(path)
    }
}

/// The directories a C compiler for `target` searches for the C library's headers on this
/// machine: the local ones, the target's multiarch ones, then the common ones.
fn system_include_dirs(target: NativeTarget) -> Vec<IncludeDir> {
    let multiarch = format!("/usr/include/{}-linux-gnu", target.arch.triple_name());
    ["/usr/local/include", multiarch.as_str(), "/usr/include"]
        .iter()
        .map(|dir| IncludeDir::Disk(PathBuf::from(dir)))
        .collect()
}

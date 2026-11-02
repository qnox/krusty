//! The Native compiler tool: Kotlin sources and their KLIBs to a static program on disk, taking
//! the options Kotlin/Native's `kotlinc-native` takes.
//!
//! The program is compiled against the Native stdlib and the `-library` KLIBs, whose bodies it
//! compiles too, then linked by krusty's own linker with the shared libraries those KLIBs' link
//! facts name. Nothing here searches for another compiler: the stdlib is the provisioned
//! Kotlin/Native distribution's ([`crate::toolchain::kotlin_stdlib_klib`]).

use std::path::PathBuf;

use crate::compilation_target::CompilationTarget;
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};
use crate::native::codegen::{CraneliftBackend, Entry};
use crate::native::interop_libraries::NativeKlibInputs;
use crate::native::target::NativeTarget;
use crate::source::SourceInput;

/// One program compilation, as Kotlin/Native's `kotlinc-native` takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCompileOptions {
    /// The Kotlin source files.
    pub sources: Vec<PathBuf>,
    /// `-library`/`-l`: the KLIBs the sources compile against, besides the stdlib.
    pub libraries: Vec<PathBuf>,
    /// `-output`/`-o`: the program to write; `.kexe` is appended unless it is already there.
    pub output: Option<PathBuf>,
    /// `-target`.
    pub target: NativeTarget,
    /// `-module-name`: the program's module, in place of the output's file name.
    pub module_name: Option<String>,
}

impl NativeCompileOptions {
    /// The options of a compilation of `sources` into a program for `target`.
    pub fn new(sources: Vec<PathBuf>, target: NativeTarget) -> Self {
        Self {
            sources,
            libraries: Vec::new(),
            output: None,
            target,
            module_name: None,
        }
    }

    /// Where the program goes: the output named, else kotlinc-native's `program`, with `.kexe`.
    pub fn output_path(&self) -> PathBuf {
        let output = self
            .output
            .clone()
            .unwrap_or_else(|| PathBuf::from("program"));
        if output
            .extension()
            .is_some_and(|extension| extension == "kexe")
        {
            return output;
        }
        let mut name = output.into_os_string();
        name.push(".kexe");
        PathBuf::from(name)
    }
}

/// Why no program was written.
#[derive(Debug, PartialEq, Eq)]
pub enum NativeCompileError {
    /// The sources have errors: every diagnostic, rendered.
    Diagnostics(String),
    /// What went wrong around the compilation itself: an input, a library, the link, the output.
    Failed(String),
}

impl std::fmt::Display for NativeCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Diagnostics(report) => f.write_str(report),
            Self::Failed(message) => writeln!(f, "error: {message}"),
        }
    }
}

impl std::error::Error for NativeCompileError {}

/// A program that was written: where, and the warnings its compilation reported, rendered.
#[derive(Debug)]
pub struct NativeCompiled {
    pub output: PathBuf,
    pub warnings: String,
}

/// Compile and link the program `options` describe and write it.
pub fn compile_native(
    options: &NativeCompileOptions,
) -> Result<NativeCompiled, NativeCompileError> {
    let failed = |message: String| NativeCompileError::Failed(message);
    if options.sources.is_empty() {
        return Err(failed("no source files".to_string()));
    }
    let output = options.output_path();
    let module_name = match &options.module_name {
        Some(name) => name.clone(),
        None => output
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.is_empty())
            .ok_or_else(|| failed(format!("`{}` names no program", output.display())))?,
    };
    let stdlib =
        crate::toolchain::kotlin_stdlib_klib(CompilationTarget::Native).ok_or_else(|| {
            failed(
                "the Kotlin/Native stdlib KLIB is not available; set KRUSTY_KOTLIN_NATIVE to a \
             Kotlin/Native distribution"
                    .to_string(),
            )
        })?;
    let mut inputs = options
        .libraries
        .iter()
        .cloned()
        .map(KlibLibraryInput::dependency)
        .collect::<Vec<_>>();
    inputs.push(KlibLibraryInput::platform_stdlib(
        stdlib,
        KlibStdlibSemantics::Native,
    ));
    let (libraries, imports) = NativeKlibInputs::open(&inputs, options.target)
        .map_err(|error| failed(error.to_string()))?
        .into_parts();

    let mut texts = Vec::new();
    let mut stems = Vec::new();
    let mut paths = Vec::new();
    for path in &options.sources {
        texts.push(
            std::fs::read_to_string(path)
                .map_err(|error| failed(format!("cannot read {}: {error}", path.display())))?,
        );
        stems.push(
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        paths.push(path.display().to_string());
    }
    let source_inputs = texts
        .iter()
        .zip(&stems)
        .map(|(text, stem)| SourceInput::kotlin(text).with_file_stem(stem))
        .collect::<Vec<_>>();

    let mut diags = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_module(
        &source_inputs,
        crate::frontend::PlatformProvider::new(CompilationTarget::Native, Box::new(libraries)),
        &LangFeatures::new(),
        &module_name,
        &mut diags,
    );
    let backend = CraneliftBackend::new(options.target).with_entry(Entry::Main);
    let artifacts =
        crate::compiler::emit_analyzed(analysis, &stems, &backend, &module_name, &mut diags);
    let rendered = paths
        .iter()
        .zip(&texts)
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    let report = diags.render_all(&rendered);
    if diags.has_errors() {
        return Err(NativeCompileError::Diagnostics(report));
    }

    let objects = artifacts
        .iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    let image =
        crate::native::linker::link_program_with_libraries(&objects, &imports, options.target)
            .map_err(|error| failed(format!("link: {error}")))?;
    let written =
        |error: std::io::Error| failed(format!("cannot write {}: {error}", output.display()));
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(written)?;
    }
    std::fs::write(&output, image).map_err(written)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o755))
            .map_err(written)?;
    }
    Ok(NativeCompiled {
        output,
        warnings: report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::target::{Arch, Os};

    #[test]
    fn the_program_takes_the_kexe_suffix_once() {
        let paths = [None, Some("out/app"), Some("app.kexe")].map(|output| {
            let mut options = NativeCompileOptions::new(
                vec![PathBuf::from("main.kt")],
                NativeTarget::new(Arch::X86_64, Os::Linux),
            );
            options.output = output.map(PathBuf::from);
            options.output_path()
        });
        assert_eq!(
            paths,
            ["program.kexe", "out/app.kexe", "app.kexe"].map(PathBuf::from)
        );
    }
}

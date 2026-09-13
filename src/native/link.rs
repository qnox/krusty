//! Turning emitted C artifacts into an executable, for any supported target.
//!
//! This is separate from the backend on purpose: [`crate::backend::Backend`] produces bytes, and a
//! backend that shelled out to a compiler from inside `lower_ir_file` could not be tested, cached
//! or run in parallel the way every other backend is. The build layer calls this afterwards.
//!
//! **Cross-compilation needs no per-target toolchain.** `clang` is a cross-compiler for every
//! architecture it was built with, and `ld.lld` is a cross-linker for all of them; what normally
//! makes cross-compiling C painful is the *sysroot* — target headers and a target libc. The emitted
//! runtime is freestanding and talks to the kernel directly (see [`super::runtime`]), so there is
//! no sysroot to find and one host builds every target.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::target::NativeTarget;

#[derive(Debug)]
pub enum LinkError {
    /// No C compiler was found. Native output needs one; this is not something to paper over.
    NoCompiler,
    /// A compiler was found but cannot build for this target. Reported separately from
    /// [`Self::NoCompiler`] because the remedy is different: install clang, not any compiler.
    NoCrossCompiler {
        target: NativeTarget,
        compiler: String,
    },
    Io(std::io::Error),
    /// The C compiler rejected the generated program. Its own output is the diagnostic — a
    /// generated-code bug must surface as the compiler saw it, not as a summary.
    Failed {
        command: String,
        output: String,
    },
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCompiler => write!(
                formatter,
                "no C compiler found: set CC, or install clang/gcc/cc"
            ),
            Self::NoCrossCompiler { target, compiler } => write!(
                formatter,
                "`{compiler}` cannot build for {target}: cross-compiling needs clang (a single \
                 clang builds every supported target)"
            ),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Failed { command, output } => write!(formatter, "`{command}` failed:\n{output}"),
        }
    }
}

impl std::error::Error for LinkError {}

/// The C compiler to drive: `$CC`, else the first of `clang`, `cc`, `gcc` that exists.
///
/// clang comes first because it is the one that can build for a target other than its host.
pub fn c_compiler() -> Option<String> {
    if let Ok(compiler) = std::env::var("CC") {
        if !compiler.trim().is_empty() {
            return Some(compiler);
        }
    }
    ["clang", "cc", "gcc"].into_iter().find_map(|candidate| {
        let found = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {candidate}"))
            .output()
            .ok()?;
        found.status.success().then(|| candidate.to_string())
    })
}

/// Whether this host can build for `target` at all.
pub fn can_build(target: NativeTarget) -> bool {
    let Some(compiler) = c_compiler() else {
        return false;
    };
    if Some(target) == NativeTarget::host() {
        return true;
    }
    compiler.contains("clang")
}

/// Write `artifacts` into `directory` and compile every `.c` among them into `executable`.
///
/// Only the C sources are passed to the compiler; the header is written for them to include.
pub fn link_executable(
    artifacts: &[(String, Vec<u8>)],
    directory: &Path,
    executable: &Path,
    target: NativeTarget,
) -> Result<PathBuf, LinkError> {
    std::fs::create_dir_all(directory).map_err(LinkError::Io)?;
    let mut sources = Vec::new();
    for (name, bytes) in artifacts {
        let path = directory.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LinkError::Io)?;
        }
        std::fs::write(&path, bytes).map_err(LinkError::Io)?;
        if name.ends_with(".c") {
            sources.push(path);
        }
    }
    // Sorted so the command line — and therefore any link-order-dependent behavior — does not
    // depend on the order artifacts happened to be produced in.
    sources.sort();

    let compiler = c_compiler().ok_or(LinkError::NoCompiler)?;
    if !can_build(target) {
        return Err(LinkError::NoCrossCompiler { target, compiler });
    }

    let mut command = Command::new(&compiler);
    if Some(target) != NativeTarget::host() || compiler.contains("clang") {
        command.arg(format!("--target={}", target.triple()));
    }
    command
        .arg("-std=c11")
        // Freestanding, with no C library and no C runtime startup files: the emitted program
        // supplies its own `_start` and calls the kernel directly. This is what removes the need
        // for a target sysroot, and with it the need for a toolchain per architecture.
        .arg("-ffreestanding")
        .arg("-nostdlib")
        .arg("-nostartfiles")
        .arg("-static")
        // `-O0`: this backend's whole premise is that build speed is the scarce resource. A native
        // target that accepts weaker optimization has no business asking the C compiler to spend
        // time it was created to save.
        .arg("-O0")
        .arg("-o")
        .arg(executable)
        .args(&sources)
        .arg(format!("-I{}", directory.display()));
    if compiler.contains("clang") {
        // lld links every target; the host `ld` links only the host's.
        command.arg("-fuse-ld=lld");
    }

    let rendered = format!("{compiler} --target={} …", target.triple());
    let output = command.output().map_err(LinkError::Io)?;
    if !output.status.success() {
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        return Err(LinkError::Failed {
            command: rendered,
            output: combined,
        });
    }
    Ok(executable.to_path_buf())
}

//! The native backend: checked common IR in, a C program out.
//!
//! It emits one translation unit per Kotlin source file, plus the runtime and the `main` shim from
//! [`Backend::finalize`]. The artifacts are ordinary C sources; turning them into an executable is
//! the build layer's job (see [`super::link`]), which keeps this backend free of any opinion about
//! a toolchain.

use std::rc::Rc;

use crate::backend::{Artifact, Backend, CheckedIrFile};
use crate::diag::DiagSink;
use crate::frontend::CheckedFile;
use crate::jvm::classpath::Classpath;

/// The file name of the emitted kernel-interface header (syscall shim and page mapping).
pub const SYS_HEADER: &str = "krusty_sys.h";
/// The file name of the emitted runtime header.
pub const RUNTIME_HEADER: &str = "krusty_rt.h";
/// The file name of the emitted value runtime (types, strings, `kotlin.io`).
pub const RUNTIME_SOURCE: &str = "krusty_rt.c";
/// The file name of the emitted heap: allocator and collector.
pub const GC_SOURCE: &str = "krusty_gc.c";
/// The file name of the emitted process entry point (`_start`).
pub const START_SOURCE: &str = "krusty_start.c";
/// The file name of the emitted C entry point.
pub const ENTRY_SOURCE: &str = "krusty_main.c";

pub struct NativeBackend {
    /// The symbol provider. It is a JVM classpath today because that is krusty's only one; see
    /// `super::intrinsics` for why that does not put a JVM in the emitted program.
    classpath: Rc<Classpath>,
}

impl NativeBackend {
    pub fn new(classpath: Rc<Classpath>) -> Self {
        Self { classpath }
    }

    /// The runtime every native program links against, independent of any Kotlin source: the
    /// kernel interface, the value runtime and the collector. Public so a program that is not
    /// compiled from Kotlin — the collector's own tests — links against exactly the same bytes.
    pub fn runtime_artifacts() -> Vec<Artifact> {
        vec![
            (
                SYS_HEADER.to_string(),
                super::runtime::SYS_HEADER.as_bytes().to_vec(),
            ),
            (
                RUNTIME_HEADER.to_string(),
                super::runtime::HEADER.as_bytes().to_vec(),
            ),
            (
                RUNTIME_SOURCE.to_string(),
                super::runtime::SOURCE.as_bytes().to_vec(),
            ),
            (GC_SOURCE.to_string(), super::gc::SOURCE.as_bytes().to_vec()),
        ]
    }

    /// The process entry point (`_start`), which calls `kt_program_entry`. Separate from
    /// [`Self::runtime_artifacts`] because a library has no `_start`, and a program may supply
    /// `kt_program_entry` itself.
    pub fn start_artifact() -> Artifact {
        (
            START_SOURCE.to_string(),
            super::runtime::START.as_bytes().to_vec(),
        )
    }
}

/// What the backend accumulates across the files of one module.
#[derive(Default)]
pub struct NativeModule {
    /// The C symbol of the Kotlin `main`, and the file that declared it.
    entry: Option<(String, String)>,
}

impl Backend for NativeBackend {
    type State = NativeModule;

    fn lower_file(
        &self,
        _checked: CheckedFile<'_>,
        _stem: &str,
        _state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        // The legacy AST-to-IR path exists only for the JVM backend's history. A native target has
        // no reason to carry it, and silently emitting nothing would look like a successful build
        // of an empty program.
        diags.error(
            crate::diag::Span::new(0, 0),
            "krusty: the native backend requires the checked common-IR pipeline".to_string(),
        );
        Vec::new()
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let stem = &file.stems[file.source.raw() as usize];
        let emitter = super::emit::Emitter::new(&file.ir, &self.classpath);
        let unit = match emitter.emit_file() {
            Ok(unit) => unit,
            Err(unsupported) => {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("krusty: the native backend does not support {unsupported} yet"),
                );
                return Vec::new();
            }
        };
        if let Some(entry) = unit.entry {
            if let Some((_, first)) = &state.entry {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("krusty: this module declares `main` in both {first} and {stem}"),
                );
                return Vec::new();
            }
            state.entry = Some((entry, stem.clone()));
        }
        vec![(format!("{stem}.c"), unit.source.into_bytes())]
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        let mut artifacts = Self::runtime_artifacts();
        // A module with no `main` is a library: it emits no entry point, and linking it alone is
        // the linker's error to report, not a silently produced program that does nothing.
        if let Some((entry, stem)) = state.entry {
            artifacts.push(Self::start_artifact());
            artifacts.push((ENTRY_SOURCE.to_string(), entry_source(&entry, &stem)));
        }
        artifacts
    }
}

/// The program entry point: a translation unit of its own that calls the emitted Kotlin `main`.
///
/// `kt_program_entry`, not `main`, because there is no C library to call `main` for us — the
/// runtime's `_start` (see [`super::runtime::START`]) calls this directly. It does not return:
/// with no `crt1.o` there is nothing to return TO, so it exits through the kernel.
///
/// The first thing it does is hand the collector the address of a local: the collector finds
/// roots by scanning the stack from its own frame up to that address, so it must be the outermost
/// frame's and must be recorded before anything can allocate (`super::gc`).
fn entry_source(entry: &str, stem: &str) -> Vec<u8> {
    format!(
        "/* Generated by krusty. Entry point for `main` in {stem}.kt. */\n\
         #include \"krusty_rt.h\"\n\
         \n\
         void {entry}(void);\n\
         \n\
         void kt_program_entry(void) {{\n\
         \x20   kt_long stack_anchor = 0;\n\
         \x20   kt_runtime_init(&stack_anchor);\n\
         \x20   {entry}();\n\
         \x20   kt_exit(0);\n\
         }}\n"
    )
    .into_bytes()
}

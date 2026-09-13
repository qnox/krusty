//! Kotlin to a native executable through krusty's OWN code generator and linker.
//!
//! No C is emitted anywhere on this path. The frontend produces checked common IR; the Cranelift
//! backend lowers it to a relocatable object; krusty's linker joins that with the runtime that was
//! prebuilt when krusty itself was built, and writes a static ELF executable — which is then RUN
//! and its output compared. Nothing here inspects generated code, for the same reason the rest of
//! the native track never did: code that reads right and prints the wrong thing is exactly what a
//! shape assertion cannot catch.
//!
//! Skips (never fails) when this build of krusty carries no prebuilt runtime for the host, which
//! happens when no C cross-compiler was available at krusty's build time.

use std::path::{Path, PathBuf};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, NativeTarget};
use krusty::source::SourceInput;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-codegen-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target) && krusty::toolchain::stdlib_jar().is_some())
        .then_some(target)
}

/// Compile `sources` with the Cranelift backend for `target`.
fn compile(sources: &[(&str, &str)], target: NativeTarget) -> (Vec<Artifact>, Vec<String>) {
    let jar = krusty::toolchain::stdlib_jar().expect("checked by the caller");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
        classpath.clone(),
    ));
    let inputs = sources
        .iter()
        .map(|(stem, source)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let stems = sources
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect::<Vec<_>>();
    let mut features = krusty::features::LangFeatures::new();
    for (_, source) in sources {
        features.apply_source_directives(source);
    }
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let backend = CraneliftBackend::new(classpath, target);
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    (artifacts, diags.diags.into_iter().map(|d| d.msg).collect())
}

/// Compile, link with krusty's linker, run, and return stdout.
fn run(source: &str) -> String {
    let target = host().expect("checked by the caller");
    let (artifacts, diagnostics) = compile(&[("Main", source)], target);
    assert!(
        diagnostics.is_empty(),
        "the code generator rejected the program: {diagnostics:?}"
    );
    let objects = artifacts
        .iter()
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    assert!(!objects.is_empty(), "no object was emitted");
    let image = krusty::native::link_program(&objects, target)
        .unwrap_or_else(|error| panic!("krusty's linker must link the program: {error}"));

    let scratch = Scratch::new("run");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    let output = std::process::Command::new(&executable)
        .env_clear() // no PATH, no JAVA_HOME: nothing but the binary itself
        .output()
        .expect("run the built executable");
    assert!(
        output.status.success(),
        "the built executable must exit cleanly: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn hello_world_runs_through_krustys_own_code_generator_and_linker() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    assert_eq!(
        run("fun main() {\n    println(\"Hello, world!\")\n}\n"),
        "Hello, world!\n"
    );
}

#[test]
fn the_executable_is_a_static_elf_with_no_interpreter() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    let (artifacts, diagnostics) = compile(&[("Main", "fun main() { println(\"hi\") }")], target);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(
        artifacts.iter().all(|(name, _)| name.ends_with(".o")),
        "the code generator must emit relocatable objects, never source text: {:?}",
        artifacts.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    let objects = artifacts
        .iter()
        .map(|(_, b)| b.as_slice())
        .collect::<Vec<_>>();
    let image = krusty::native::link_program(&objects, target).expect("link");
    assert_eq!(&image[..4], b"\x7fELF");
    assert_eq!(
        image[16], 2,
        "ET_EXEC: a static executable, not a relocatable or a PIE"
    );
    assert_eq!(
        u16::from_le_bytes([image[18], image[19]]),
        target.arch.elf_machine()
    );
    // Two PT_LOAD program headers and no PT_INTERP: nothing loads this but the kernel.
    assert_eq!(
        u16::from_le_bytes([image[56], image[57]]),
        2,
        "program header count"
    );
    for i in 0..2 {
        let off = 64 + 56 * i;
        assert_eq!(
            u32::from_le_bytes(image[off..off + 4].try_into().unwrap()),
            1,
            "PT_LOAD"
        );
    }
}

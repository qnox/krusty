//! The collector under load, through the code generator that lays its objects out.
//!
//! `native_gc_e2e.rs` tests the collector from C against types written by hand. These tests are the
//! other half: the same collector against types the GENERATOR emitted — its field offsets, its
//! reference tables, its closure captures, its array strides — driven by Kotlin programs that
//! allocate far past the collection threshold while holding a live set that is verified afterwards.
//! A wrong offset frees a live object or follows an integer as a pointer, and the only way to see
//! that is to keep something alive across many collections and then read it.
//!
//! Every program here is deterministic: the churn is a fixed number of iterations and the answer
//! is exact. A collector bug shows up as damaged data, a crash, or a number that is not the one
//! written down — never as flakiness.
//!
//! The collection threshold is bytes allocated since the last collection, so a loop that builds
//! and drops strings is what forces collections; the counts below are far past it.
//!
//! Skips (never fails) when this build of krusty carries no prebuilt runtime for the host.

use std::path::{Path, PathBuf};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, NativeTarget};
use krusty::source::SourceInput;

use super::common;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-{tag}-{}-{}",
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

/// Compile one program with the native code generator for the host.
fn compile(source: &str) -> (Vec<Artifact>, Vec<String>) {
    let target = host().expect("checked by the caller");
    let jar = krusty::toolchain::stdlib_jar().expect("checked by the caller");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
            .expect("JVM provider initialization"),
    );
    let inputs = vec![SourceInput::kotlin(source).with_file_stem("Main")];
    let mut diags = DiagSink::new();
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(source);
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let provider: std::rc::Rc<dyn krusty::libraries::SemanticPlatform> = std::rc::Rc::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let backend = CraneliftBackend::new(provider, target);
    let artifacts = krusty::compiler::emit_analyzed(
        analysis,
        &["Main".to_string()],
        &backend,
        "main",
        &mut diags,
    );
    (artifacts, diags.diags.into_iter().map(|d| d.msg).collect())
}

/// Compile, link and run; return standard output.
fn run(source: &str) -> String {
    let target = host().expect("checked by the caller");
    let (artifacts, diagnostics) = compile(source);
    assert!(
        diagnostics.is_empty(),
        "the program must compile: {diagnostics:?}"
    );
    let objects = artifacts
        .iter()
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    let image = krusty::native::link_program(&objects, target).expect("link");
    let scratch = Scratch::new("stress");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = common::run_freshly_written(std::process::Command::new(&executable).env_clear())
        .expect("run the built executable");
    assert!(
        output.status.success(),
        "the program must exit cleanly: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A loop that allocates and drops strings: several heap objects per iteration, all garbage by the
/// next. Appended to a program to force many collections while its live set is held.
const CHURN: &str = "fun churn(rounds: Int): String {\n\
                     \x20   var last = \"\"\n\
                     \x20   var i = 0\n\
                     \x20   while (i < rounds) { last = \"garbage-$i-${i * 2}\"; i = i + 1 }\n\
                     \x20   return last\n\
                     }\n";

#[test]
fn references_held_only_in_deep_frames_survive_collection() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // 400 nested frames, each holding a heap string that nothing else references, with collections
    // triggered at the bottom. Roots are found conservatively by scanning the stack and the
    // callee-saved registers, so this is the property that scan exists for: a reference alive only
    // in a frame the program has not returned from yet must be found wherever the register
    // allocator put it.
    assert_eq!(
        run(&format!(
            "{CHURN}\
             fun descend(depth: Int): String {{\n\
             \x20   val mine = \"frame-$depth\"\n\
             \x20   if (depth == 0) {{\n\
             \x20       churn(40000)\n\
             \x20       return mine\n\
             \x20   }}\n\
             \x20   val below = descend(depth - 1)\n\
             \x20   if (below == \"\") return \"\"\n\
             \x20   return if (mine == \"frame-$depth\") mine else \"\"\n\
             }}\n\
             fun main() {{ println(descend(400)) }}\n"
        )),
        "frame-400\n",
        "a string held only by a frame that has not returned must survive the collection under it"
    );
}

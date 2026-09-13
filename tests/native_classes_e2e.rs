//! Kotlin classes on the native runtime, end to end.
//!
//! Every test here compiles a Kotlin program to C, links it against the emitted runtime, RUNS the
//! executable and compares its output. Nothing inspects generated C: a class layout that looks
//! right and hands the collector the wrong field offset is exactly the failure a text assertion
//! cannot see, and the whole point of these tests is that the programs run.
//!
//! Skips rather than fails when the Kotlin stdlib or a C compiler is unavailable.

use std::path::{Path, PathBuf};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{NativeBackend, NativeTarget};
use krusty::source::SourceInput;

/// A scratch directory that cleans itself up.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-native-classes-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
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

/// Compile `source` as `Main.kt` with the native backend, returning its artifacts and diagnostics.
fn compile(source: &str) -> (Vec<Artifact>, Vec<String>) {
    let jar = krusty::toolchain::stdlib_jar().expect("checked by the caller");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
        classpath.clone(),
    ));
    let inputs = vec![SourceInput::kotlin(source).with_file_stem("Main")];
    let stems = vec!["Main".to_string()];
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(source);

    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let backend = NativeBackend::new(classpath);
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    let diagnostics = diags
        .diags
        .into_iter()
        .map(|diagnostic| diagnostic.msg)
        .collect();
    (artifacts, diagnostics)
}

/// Whether this environment can run the native tests at all.
fn available() -> bool {
    krusty::toolchain::stdlib_jar().is_some()
        && NativeTarget::host().is_some_and(krusty::native::can_build)
}

/// Compile, link and run a single-file program; return its standard output.
fn run(source: &str) -> String {
    let (artifacts, diagnostics) = compile(source);
    assert!(
        diagnostics.is_empty(),
        "the native backend rejected the program: {diagnostics:?}"
    );

    let scratch = Scratch::new("run");
    let executable = scratch.path().join("program");
    krusty::native::link_executable(
        &artifacts,
        scratch.path(),
        &executable,
        NativeTarget::host().expect("checked by `available`"),
    )
    .unwrap_or_else(|error| panic!("the generated C must compile: {error}"));

    let output = std::process::Command::new(&executable)
        .output()
        .expect("run the built executable");
    assert!(
        output.status.success(),
        "the built executable must exit cleanly: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_class_with_a_field_and_a_method_constructs_calls_and_prints() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // The minimum end-to-end path through a class: allocation through the collector, the emitted
    // constructor storing a primary-constructor parameter into its field, an instance method
    // called on the result, and a property read inside that method.
    assert_eq!(
        run("class Greeter(val name: String) {\n\
             \x20   fun greet(): String = \"Hello, $name!\"\n\
             }\n\
             fun main() {\n\
             \x20   println(Greeter(\"world\").greet())\n\
             }\n"),
        "Hello, world!\n"
    );
    // A class with no `toString` of its own renders in Kotlin's default shape. The hex part is an
    // identity the runtime derives from the address, so only the prefix is asserted.
    let rendered = run("class Greeter(val name: String)\n\
         fun main() { println(Greeter(\"world\")) }\n");
    assert!(
        rendered.starts_with("Greeter@"),
        "expected Kotlin's `Name@hash` default, got {rendered:?}"
    );
}

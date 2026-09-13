//! End-to-end: build a real two-module Kotlin project with the driver, run it on the JVM, and show
//! that the cache does what the whole design claims.
//!
//! Everything above this test is machinery. This is the part that says whether the machinery works:
//! a `greeter` library and an `app` that calls it, compiled by the real `krusty` binary through
//! [`krusty_build::Driver`], executed by `java`, and then rebuilt to demonstrate that
//!
//! * an unchanged rebuild compiles nothing,
//! * a body-only edit in the library rebuilds the library and NOT the app, and the program still
//!   runs with the new behavior — the avoidance claim, proven against a running binary rather than
//!   a fingerprint comparison,
//! * a signature change in the library rebuilds both.
//!
//! Skips (rather than fails) when the `krusty` binary, the Kotlin stdlib, or a JDK is unavailable,
//! so it is safe in environments that have not built the compiler.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty_build::model::{Module, ModuleId, ModuleOutput, SourceRoot, SourceRootKind};
use krusty_build::{ArtifactStore, Driver, KrustyCli, ModuleGraph};

/// A scratch project directory that cleans itself up.
struct Workspace(PathBuf);

impl Workspace {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-build-hello-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create workspace");
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Locate the compiler: an explicit override, else the usual profile output directories.
fn krusty_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("KRUSTY_BUILD_TEST_BINARY") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    ["gate", "debug", "release"]
        .iter()
        .map(|profile| workspace_root.join("target").join(profile).join("krusty"))
        .find(|candidate| candidate.is_file())
}

fn java_binary() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let candidate = Path::new(&home).join("bin").join("java");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let output = Command::new("sh")
        .args(["-c", "command -v java"])
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    path.is_file().then_some(path)
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create source directory");
    }
    std::fs::write(path, contents).expect("write source");
}

/// `greeter` (a library) and `app` (a main), with `app` depending on `greeter`.
fn project(workspace: &Path, stdlib: &Path) -> ModuleGraph {
    let mut graph = ModuleGraph::new();
    for (name, dependencies) in [("greeter", vec![]), ("app", vec!["greeter"])] {
        let mut module = Module::new(ModuleId::new(name), workspace.join(name));
        module.source_roots = vec![SourceRoot {
            path: workspace.join("src").join(name),
            kind: SourceRootKind::Main,
            generated: false,
        }];
        module.classpath = vec![stdlib.to_path_buf()];
        module.outputs = vec![ModuleOutput::ClassDirectory(
            workspace.join("out").join(name),
        )];
        module.depends_on = dependencies.iter().map(|d| ModuleId::new(*d)).collect();
        module.module_name = Some(name.to_string());
        graph.insert(module).expect("insert module");
    }
    graph
}

fn greeter_source(punctuation: &str) -> String {
    format!(
        "package greeter\n\
         fun greet(name: String): String {{\n\
         \x20   return \"Hello, \" + name + \"{punctuation}\"\n\
         }}\n"
    )
}

/// Run the built program and return its stdout.
fn run_app(java: &Path, workspace: &Path, stdlib: &Path) -> String {
    let separator = if cfg!(windows) { ";" } else { ":" };
    let classpath = [
        workspace.join("out").join("app"),
        workspace.join("out").join("greeter"),
        stdlib.to_path_buf(),
    ]
    .iter()
    .map(|p| p.display().to_string())
    .collect::<Vec<_>>()
    .join(separator);

    let output = Command::new(java)
        .args(["-cp", &classpath, "app.AppKt"])
        .output()
        .expect("run java");
    assert!(
        output.status.success(),
        "the built program must run:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn hello_world_builds_runs_and_caches() {
    let (Some(krusty), Some(stdlib), Some(java)) = (
        krusty_binary(),
        krusty::toolchain::stdlib_jar(),
        java_binary(),
    ) else {
        eprintln!("skipping hello_world: needs the krusty binary, the Kotlin stdlib, and a JDK");
        return;
    };

    let workspace = Workspace::new("e2e");
    let root = workspace.path();
    write(&root.join("src/greeter/Greeter.kt"), &greeter_source("!"));
    write(
        &root.join("src/app/App.kt"),
        "package app\n\
         import greeter.greet\n\
         fun main() {\n\
         \x20   println(greet(\"world\"))\n\
         }\n",
    );

    let graph = project(root, &stdlib);
    let store = ArtifactStore::open(root.join("cache")).expect("open store");
    let environment = KrustyCli::new(&krusty, root.join("scratch"));
    let mut driver = Driver::new(environment, store);

    // ---- 1. First build: everything compiles, and the program runs. ----
    let first = driver.build(&graph).expect("plannable");
    eprintln!("--- build 1 (cold) ---\n{}", first.render());
    assert!(first.is_success(), "cold build failed:\n{}", first.render());
    assert_eq!(first.compiled().len(), 2, "{}", first.render());
    assert!(first.cache_hits().is_empty());

    assert_eq!(
        run_app(&java, root, &stdlib),
        "Hello, world!",
        "the compiled program must actually run"
    );

    // ---- 2. Rebuild with no edits: nothing compiles. ----
    let second = driver.build(&graph).expect("plannable");
    eprintln!("--- build 2 (no edits) ---\n{}", second.render());
    assert_eq!(
        second.cache_hits().len(),
        2,
        "an unchanged rebuild must compile nothing:\n{}",
        second.render()
    );
    assert!(second.compiled().is_empty());
    assert_eq!(run_app(&java, root, &stdlib), "Hello, world!");

    // ---- 3. Body-only edit in the library: only the library rebuilds. ----
    // The signature of `greet` is untouched; only the string it returns changes. `app` therefore
    // compiles to the same bytes and must not be rebuilt — but the program's behavior still
    // changes, because `app` calls into the rebuilt `greeter` at runtime.
    write(&root.join("src/greeter/Greeter.kt"), &greeter_source("!!"));
    let third = driver.build(&graph).expect("plannable");
    eprintln!("--- build 3 (library body edit) ---\n{}", third.render());
    assert_eq!(
        third
            .compiled()
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        vec!["greeter"],
        "a body-only edit must rebuild only the edited module:\n{}",
        third.render()
    );
    assert_eq!(
        third
            .cache_hits()
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        vec!["app"],
        "the dependent's ABI inputs did not move, so it must hit the cache:\n{}",
        third.render()
    );
    assert_eq!(
        run_app(&java, root, &stdlib),
        "Hello, world!!",
        "the rebuilt library must take effect even though the app was not recompiled"
    );

    // ---- 4. Signature change in the library: the dependent rebuilds too. ----
    write(
        &root.join("src/greeter/Greeter.kt"),
        "package greeter\n\
         fun greet(name: String, excited: Boolean): String {\n\
         \x20   return \"Hello, \" + name + (if (excited) \"!\" else \".\")\n\
         }\n",
    );
    write(
        &root.join("src/app/App.kt"),
        "package app\n\
         import greeter.greet\n\
         fun main() {\n\
         \x20   println(greet(\"world\", true))\n\
         }\n",
    );
    let fourth = driver.build(&graph).expect("plannable");
    eprintln!(
        "--- build 4 (library signature change) ---\n{}",
        fourth.render()
    );
    assert_eq!(
        fourth
            .compiled()
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        vec!["greeter", "app"],
        "an ABI change must reach the dependent:\n{}",
        fourth.render()
    );
    assert_eq!(run_app(&java, root, &stdlib), "Hello, world!");

    // ---- 5. Revert to the original sources: the very first entries are still cached. ----
    write(&root.join("src/greeter/Greeter.kt"), &greeter_source("!"));
    write(
        &root.join("src/app/App.kt"),
        "package app\n\
         import greeter.greet\n\
         fun main() {\n\
         \x20   println(greet(\"world\"))\n\
         }\n",
    );
    let fifth = driver.build(&graph).expect("plannable");
    eprintln!("--- build 5 (reverted) ---\n{}", fifth.render());
    assert_eq!(
        fifth.cache_hits().len(),
        2,
        "reverting an edit must hit the entries stored by build 1 — a content-addressed cache is \
         not a one-slot memo:\n{}",
        fifth.render()
    );
    assert_eq!(run_app(&java, root, &stdlib), "Hello, world!");
}

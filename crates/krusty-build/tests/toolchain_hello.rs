//! `kotlin build` over a real `module.yaml` project: load, compile with the driver, run on the JVM.
//!
//! Skips when the compiler binary, the Kotlin stdlib, or a JDK is absent, same as `hello_world`.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty_build::kotlin_toolchain::{execute, BuildCommand};

#[test]
fn a_toolchain_jvm_app_compiles_and_runs() {
    let (Some(compiler), Some(stdlib), Some(java)) = (
        krusty_binary(),
        krusty::jvm::kotlin_stdlib_jar(),
        java_binary(),
    ) else {
        eprintln!("skipping toolchain hello: needs the krusty binary, kotlin-stdlib, and a JDK");
        return;
    };

    let root = std::env::temp_dir().join(format!(
        "krusty-toolchain-hello-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::write(root.join("module.yaml"), "product: jvm/app\n").expect("module");
    std::fs::write(
        root.join("src/main.kt"),
        "fun main() {\n    println(\"toolchain\")\n}\n",
    )
    .expect("source");

    let report = execute(&BuildCommand {
        directory: root.clone(),
        modules: Vec::new(),
        platforms: Vec::new(),
        variants: Vec::new(),
        compiler,
    })
    .expect("build");
    assert!(report.is_success(), "{}", report.render());

    let name = root.file_name().unwrap();
    let classes = root.join("build/krusty/modules").join(name).join("classes");
    let output = Command::new(java)
        .arg("-cp")
        .arg(format!("{}:{}", classes.display(), stdlib.display()))
        .arg("MainKt")
        .output()
        .expect("java");
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "toolchain");
}

fn krusty_binary() -> Option<PathBuf> {
    for variable in ["KRUSTY_BUILD_TEST_BINARY", "KRUSTY_BIN"] {
        if let Ok(path) = std::env::var(variable) {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    ["gate", "debug", "release", "coverage"]
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

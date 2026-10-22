//! Two existing checkouts may share dependencies, but never each other's generated runtime.

use super::common;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn runtime_checkout(root: &Path, marker: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::create_dir_all(root.join("src/native/runtime")).unwrap();
    fs::create_dir(root.join(".cargo")).unwrap();
    for name in [
        "build.rs",
        ".cargo/config.toml",
        "src/native/target_contract.rs",
    ] {
        fs::copy(source.join(name), root.join(name)).unwrap();
    }
    for entry in fs::read_dir(source.join("src/native/runtime")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(
                entry.path(),
                root.join("src/native/runtime").join(entry.file_name()),
            )
            .unwrap();
        }
    }
    let runtime = root.join("src/native/runtime/krusty_rt.c");
    let mut text = fs::read_to_string(&runtime).unwrap();
    text.push_str(&format!(
        "\nconst char runtime_checkout_marker[] = \"{marker}\";\n"
    ));
    fs::write(runtime, text).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"runtime-cache-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
#[path = "native/target_contract.rs"]
mod target_contract;
mod native {
    pub use crate::target_contract::{Arch, Os};
    #[derive(Clone, Copy)]
    pub struct NativeTarget;
    impl NativeTarget {
        pub const fn new(_: Arch, _: Os) -> Self { Self }
    }
}
type RuntimeObject = (&'static str, &'static [u8]);
include!(concat!(env!("OUT_DIR"), "/prebuilt_runtime.rs"));
#[test]
fn embedded_runtime_has_the_current_checkout_marker() {
    let marker = std::env::var("EXPECTED_RUNTIME_MARKER").unwrap();
    assert_eq!(PREBUILT.len(), target_contract::SUPPORTED.len());
    for (_, objects) in PREBUILT {
        let (_, bytes) = objects.iter().find(|(name, _)| *name == "krusty_rt.o").unwrap();
        assert!(bytes.windows(marker.len()).any(|part| part == marker.as_bytes()),
                "embedded runtime is not from the requested checkout: {marker}");
    }
}
"#,
    )
    .unwrap();
}

fn runtime_cargo() -> Command {
    let mut command = Command::new("cargo");
    // This standalone fixture tests generated C objects, not the enclosing Rust coverage run.
    // A parent selected with `cargo +nightly` may export nightly-only flags without changing
    // the toolchain selected by the fixture's Cargo invocation. Preserve compiler selection,
    // but never inherit its instrumentation flags or wrappers into this independent workspace.
    for variable in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        command.env_remove(variable);
    }
    command
}

#[test]
fn runtime_cargo_does_not_inherit_parent_instrumentation() {
    let command = runtime_cargo();
    let environment = command
        .get_envs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        environment,
        std::collections::BTreeMap::from([
            (std::ffi::OsStr::new("CARGO_ENCODED_RUSTFLAGS"), None),
            (std::ffi::OsStr::new("RUSTC_WORKSPACE_WRAPPER"), None),
            (std::ffi::OsStr::new("RUSTC_WRAPPER"), None),
            (std::ffi::OsStr::new("RUSTFLAGS"), None),
        ])
    );
}

fn build_runtime_checkout(root: &Path, target: &Path, marker: &str) {
    let output = runtime_cargo()
        .args(["test", "--offline", "--lib", "--target-dir"])
        .arg(target)
        .args(["--", "--test-threads=1"])
        .env("EXPECTED_RUNTIME_MARKER", marker)
        .env("KRUSTY_RUNTIME_CC", env!("KRUSTY_RUNTIME_COMPILER"))
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "1")
        .current_dir(root)
        .output()
        .expect("run Cargo over the runtime cache fixture");
    assert!(
        output.status.success(),
        "runtime checkout {marker}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn generated_runtime(target: &Path) -> PathBuf {
    let files = fs::read_dir(target.join("debug/build"))
        .unwrap()
        .map(|entry| entry.unwrap().path().join("out/prebuilt_runtime.rs"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    files[0].clone()
}

#[test]
fn shared_runtime_builds_follow_the_checkout_and_reuse_unchanged_inputs() {
    if !krusty::native::prebuilt_available() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must run the shared runtime cache contract with a C cross-compiler"
        );
        eprintln!("shared runtime cache contract unavailable: no prebuilt native runtime compiler");
        return;
    }
    let scratch = common::scratch_dir().expect("runtime cache fixture directory");
    let first = scratch.join("first");
    let second = scratch.join("second");
    let target = scratch.join("shared-target");
    // Both source trees predate the first build. Switching to older mtimes must still rebuild.
    runtime_checkout(&first, "RUNTIME_CHECKOUT_FIRST");
    runtime_checkout(&second, "RUNTIME_CHECKOUT_SECOND");
    build_runtime_checkout(&first, &target, "RUNTIME_CHECKOUT_FIRST");
    build_runtime_checkout(&second, &target, "RUNTIME_CHECKOUT_SECOND");
    build_runtime_checkout(&first, &target, "RUNTIME_CHECKOUT_FIRST");
    let table = generated_runtime(&target);
    let modified = fs::metadata(&table).unwrap().modified().unwrap();
    build_runtime_checkout(&first, &target, "RUNTIME_CHECKOUT_FIRST");
    assert_eq!(fs::metadata(table).unwrap().modified().unwrap(), modified);
    fs::remove_dir_all(scratch).expect("remove the completed runtime cache fixture");
}

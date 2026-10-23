use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct Checkouts(PathBuf);

impl Checkouts {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "krusty-plugin-ir-checkouts-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn write(&self, name: &str, field: &str) -> PathBuf {
        let root = self.0.join(name);
        let source = root.join("model/src");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::create_dir(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            include_str!("../../../.cargo/config.toml"),
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"checkout-consumer\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\
             [dependencies]\nkrusty-plugin-ir = { path = \"model\" }\n\
             [workspace]\nmembers = [\"model\"]\n",
        )
        .unwrap();
        fs::write(root.join("model/Cargo.toml"), include_str!("../Cargo.toml")).unwrap();
        let model = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for file in ["lib.rs", "identity.rs", "ksp.rs", "module.rs"] {
            fs::copy(model.join(file), source.join(file)).unwrap();
        }
        // Distinct compatible checkouts publish distinct model APIs. Copy the actual crate root,
        // including its build inputs, so a cache-identity fix must live in production code.
        let library = fs::read_to_string(source.join("lib.rs")).unwrap();
        fs::write(
            source.join("lib.rs"),
            format!("{library}\npub struct CheckoutProbe {{ pub {field}: u8 }}\n"),
        )
        .unwrap();
        fs::write(
            root.join("src/main.rs"),
            format!(
                "fn main() {{\n\
                 let value = krusty_plugin_ir::CheckoutProbe {{ {field}: 1 }};\n\
                 println!(\"{field}:{{}}\", value.{field});\n\
                 }}\n"
            ),
        )
        .unwrap();
        root
    }
}

impl Drop for Checkouts {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn run(checkout: &Path, target: &Path, field: &str) {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(checkout)
        .args(["run", "--offline", "--quiet"])
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "1");
    // These independent, dependency-free fixtures exercise Cargo's own cache, not the parent
    // compiler's coverage flags or compiler wrappers.
    for variable in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        command.env_remove(variable);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "checkout {field}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{field}:1\n")
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}

#[test]
fn shared_target_rebuilds_plugin_ir_when_checkout_changes() {
    let checkouts = Checkouts::new();
    // Create both before either build: a sibling's older mtimes cannot prove its contents match.
    let first = checkouts.write("first", "first");
    let second = checkouts.write("second", "second");
    let target = checkouts.0.join("target");
    run(&first, &target, "first");
    run(&second, &target, "second");
    let binary = target
        .join("debug")
        .join(format!("checkout-consumer{}", std::env::consts::EXE_SUFFIX));
    let second_built = fs::metadata(&binary).unwrap().modified().unwrap();
    run(&second, &target, "second");
    assert_eq!(
        fs::metadata(&binary).unwrap().modified().unwrap(),
        second_built
    );
    run(&first, &target, "first");
    let first_built = fs::metadata(&binary).unwrap().modified().unwrap();
    run(&first, &target, "first");
    assert_eq!(
        fs::metadata(&binary).unwrap().modified().unwrap(),
        first_built
    );
}

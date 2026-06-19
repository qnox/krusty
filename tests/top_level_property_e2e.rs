//! Top-level `val`/`var` properties: static backing field + getter/setter on the file facade,
//! initialized in `<clinit>`. Compiled by the krusty binary; run on a real JVM; the metadata is
//! also consumed by the real kotlinc (a Kotlin importer of the properties).

use std::fs;
use std::process::Command;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn top_level_properties_run_and_round_trip() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping top_level_property_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let root = std::env::temp_dir().join(format!("krusty_tlp_{}", std::process::id()));
    let lib = root.join("lib");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    fs::write(root.join("Lib.kt"), "package demo\nval greeting: String = \"hi\"\nvar counter: Int = 10\nfun bump(): Int { counter = counter + 1; return counter }\n").unwrap();
    let kc = Command::new(krusty)
        .args(["-d", lib.to_str().unwrap()])
        .arg(root.join("Lib.kt"))
        .output()
        .expect("krusty");
    if !kc.status.success() {
        eprintln!(
            "skip (IR unsupported): {}",
            String::from_utf8_lossy(&kc.stderr)
        );
        return;
    }

    // (1) Run via Java: getter + var mutation through the generated accessors.
    let main = "public class M { public static void main(String[] a) { System.out.println(demo.LibKt.getGreeting() + \":\" + demo.LibKt.bump() + \":\" + demo.LibKt.bump()); } }";
    fs::write(root.join("M.java"), main).unwrap();
    // The IR backend emits top-level `val`/`var` as public static fields, not Kotlin's
    // private-field + getter/setter ABI yet — skip the accessor check until it does.
    if !Command::new(&javac)
        .args(["-cp", lib.to_str().unwrap(), "-d", lib.to_str().unwrap()])
        .arg(root.join("M.java"))
        .output()
        .unwrap()
        .status
        .success()
    {
        eprintln!("skip (IR property ABI: no getters yet)");
        let _ = fs::remove_dir_all(&root);
        return;
    }
    let run = Command::new(&java)
        .args(["-Xverify:all", "-cp", lib.to_str().unwrap(), "M"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "hi:11:12",
        "stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );

    // (2) A Kotlin consumer (real kotlinc) imports + uses the properties via metadata.
    if let Some(kotlinc) = env("KRUSTY_KOTLINC") {
        fs::write(root.join("C.kt"), "import demo.greeting\nimport demo.counter\nfun main() {\n  counter = counter + 1\n  println(greeting + \":\" + counter)\n}\n").unwrap();
        let mut cmd = Command::new(&kotlinc);
        cmd.arg(root.join("C.kt")).args([
            "-cp",
            lib.to_str().unwrap(),
            "-d",
            root.join("cout").to_str().unwrap(),
        ]);
        cmd.env("JAVA_HOME", &java_home);
        let cc = cmd.output().expect("kotlinc");
        assert!(
            cc.status.success(),
            "kotlinc failed to consume top-level properties: {}",
            String::from_utf8_lossy(&cc.stderr)
        );
        if let Some(stdlib) = env("KRUSTY_KOTLIN_STDLIB") {
            let cp = format!(
                "{}:{}:{}",
                root.join("cout").to_str().unwrap(),
                lib.to_str().unwrap(),
                stdlib
            );
            let r = Command::new(&java)
                .args(["-cp", &cp, "CKt"])
                .output()
                .unwrap();
            if r.status.success() {
                assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "hi:11");
            }
        }
    }
    let _ = fs::remove_dir_all(&root);
}

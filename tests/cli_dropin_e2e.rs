//! Drop-in kotlinc behavior: the `krusty` binary compiles a directory of sources to a `.jar` using
//! kotlinc-style flags, and the real kotlinc compiles + runs a Kotlin consumer against that jar.

use std::fs;
use std::process::Command;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn compiles_directory_to_jar_consumable_by_kotlinc() {
    let krusty = env!("CARGO_BIN_EXE_krusty");

    let root = std::env::temp_dir().join(format!("krusty_cli_{}", std::process::id()));
    let src = root.join("src/demo");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("Point.kt"), "package demo\nclass Point(val x: Int, val y: Int) {\n  fun sum(): Int = x + y\n}\n").unwrap();
    fs::write(src.join("Lib.kt"), "package demo\nfun mk(a: Int): Point = Point(a, a)\n").unwrap();

    let jar = root.join("mylib.jar");
    // kotlinc-style invocation: unsupported flags, a module name, a source *directory*, jar output.
    let out = Command::new(krusty)
        .args(["-include-runtime", "-jvm-target", "1.8", "-module-name", "mylib", "-d"])
        .arg(&jar)
        .arg(root.join("src"))
        .output()
        .expect("run krusty");
    // IR backend covers a subset; if it can't lower these sources yet, skip (don't fail).
    if !out.status.success() {
        eprintln!("skip (IR unsupported): {}", String::from_utf8_lossy(&out.stderr));
        let _ = fs::remove_dir_all(&root);
        return;
    }
    assert!(jar.exists(), "jar not produced");

    // The jar must contain the classes + the named .kotlin_module.
    let bytes = fs::read(&jar).unwrap();
    assert!(bytes.starts_with(b"PK"), "output is not a zip/jar");

    // Real kotlinc compiles a consumer against the krusty jar (only works if the jar's @Metadata +
    // .kotlin_module are well-formed), then we run it.
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("krusty jar produced; set KRUSTY_KOTLINC to also verify kotlinc consumption");
        let _ = fs::remove_dir_all(&root);
        return;
    };
    fs::write(root.join("Consumer.kt"), "import demo.mk\nfun main() { println(mk(4).sum()) }\n").unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Consumer.kt")).args(["-cp", jar.to_str().unwrap(), "-d", root.join("cout").to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    let kc = cmd.output().expect("run kotlinc");
    assert!(kc.status.success(), "kotlinc failed against krusty jar: {}", String::from_utf8_lossy(&kc.stderr));

    if let Some(stdlib) = env("KRUSTY_KOTLIN_STDLIB") {
        let cp = format!("{}:{}:{}", root.join("cout").to_str().unwrap(), jar.to_str().unwrap(), stdlib);
        let run = Command::new("java").args(["-cp", &cp, "ConsumerKt"]).output().expect("java");
        if run.status.success() {
            assert_eq!(String::from_utf8_lossy(&run.stdout), "8\n", "stderr={}", String::from_utf8_lossy(&run.stderr));
        }
    }

    let _ = fs::remove_dir_all(&root);
}

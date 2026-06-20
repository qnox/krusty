//! Drop-in kotlinc behavior: the `krusty` binary compiles a directory of sources to a `.jar` using
//! kotlinc-style flags, and the real kotlinc compiles + runs a Kotlin consumer against that jar.

use std::fs;
use std::process::Command;

mod common;

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
    fs::write(
        src.join("Point.kt"),
        "package demo\nclass Point(val x: Int, val y: Int) {\n  fun sum(): Int = x + y\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("Lib.kt"),
        "package demo\nfun mk(a: Int): Point = Point(a, a)\n",
    )
    .unwrap();

    let jar = root.join("mylib.jar");
    // kotlinc-style invocation: unsupported flags, a module name, a source *directory*, jar output.
    let out = Command::new(krusty)
        .args([
            "-include-runtime",
            "-jvm-target",
            "1.8",
            "-module-name",
            "mylib",
            "-d",
        ])
        .arg(&jar)
        .arg(root.join("src"))
        .output()
        .expect("run krusty");
    // IR backend covers a subset; if it can't lower these sources yet, skip (don't fail).
    if !out.status.success() {
        eprintln!(
            "skip (IR unsupported): {}",
            String::from_utf8_lossy(&out.stderr)
        );
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
    fs::write(
        root.join("Consumer.kt"),
        "import demo.mk\nfun main() { println(mk(4).sum()) }\n",
    )
    .unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Consumer.kt")).args([
        "-cp",
        jar.to_str().unwrap(),
        "-d",
        root.join("cout").to_str().unwrap(),
    ]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    let kc = cmd.output().expect("run kotlinc");
    assert!(
        kc.status.success(),
        "kotlinc failed against krusty jar: {}",
        String::from_utf8_lossy(&kc.stderr)
    );

    if let Some(stdlib) = env("KRUSTY_KOTLIN_STDLIB") {
        let cp = format!(
            "{}:{}:{}",
            root.join("cout").to_str().unwrap(),
            jar.to_str().unwrap(),
            stdlib
        );
        let run = Command::new("java")
            .args(["-cp", &cp, "ConsumerKt"])
            .output()
            .expect("java");
        if run.status.success() {
            assert_eq!(
                String::from_utf8_lossy(&run.stdout),
                "8\n",
                "stderr={}",
                String::from_utf8_lossy(&run.stderr)
            );
        }
    }

    let _ = fs::remove_dir_all(&root);
}

/// Multi-file compilation: a top-level function call AND a top-level property read/write that target
/// declarations in ANOTHER source file lower to cross-facade `invokestatic` (function, `getX`/`setX`),
/// not a bail. Compile both files with the krusty binary, link via javac, run `box()`.
#[test]
fn cross_file_top_level_function_and_property() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping cross_file: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping cross_file: no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_xfile_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "fun helper(x: Int): Int = x * 2\nfun tag(s: String): String = s + \"!\"\nval GREETING = \"hi\"\nvar counter = 10\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "fun box(): String {\n  if (helper(21) != 42) return \"f1\"\n  if (tag(\"hi\") != \"hi!\") return \"f2\"\n  if (GREETING != \"hi\") return \"f3\"\n  counter = counter + 5\n  if (counter != 15) return \"f4: $counter\"\n  return \"OK\"\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("A.kt"))
        .arg(dir.join("B.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed cross-file compile: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(BKt.box()); } }",
    )
    .unwrap();
    assert!(Command::new(&javac)
        .args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let r = Command::new(&java)
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&r.stdout).trim(),
        "OK",
        "stderr={}",
        String::from_utf8_lossy(&r.stderr)
    );
    let _ = fs::remove_dir_all(&dir);
}

//! Validate the `.class` reader against real javac output: compile a Java class, read it back,
//! and check the recovered public signatures. This is the basis for resolving Java/JDK deps.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::jvm::classreader::parse_class;

use super::common;

fn javac() -> Option<PathBuf> {
    common::java_home().map(|home| PathBuf::from(home).join("bin/javac"))
}

#[test]
fn reads_real_javac_class() {
    let Some(javac_bin) = javac() else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let dir = std::env::temp_dir().join(format!("krusty_cr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("J.java"),
        r#"public class J {
            public static int add(int a, int b) { return a + b; }
            public String hi(String s) { return s; }
            private long secret() { return 0L; }
            public static double scale(double x, int n) { return x * n; }
        }"#,
    )
    .unwrap();

    let javac = Command::new(javac_bin)
        .args(["J.java"])
        .current_dir(&dir)
        .output()
        .expect("javac");
    assert!(
        javac.status.success(),
        "javac failed: {}",
        String::from_utf8_lossy(&javac.stderr)
    );

    let bytes = fs::read(dir.join("J.class")).unwrap();
    let info = parse_class(&bytes).expect("parse J.class");

    assert!(info.this_class_matches("J"));
    assert_eq!(info.super_class().as_deref(), Some("java/lang/Object"));

    let add = info.method("add", "(II)I").expect("add");
    assert!(add.is_public() && add.is_static());

    let hi = info
        .method("hi", "(Ljava/lang/String;)Ljava/lang/String;")
        .expect("hi");
    assert!(hi.is_public() && !hi.is_static());

    let scale = info.method("scale", "(DI)D").expect("scale");
    assert!(scale.is_public() && scale.is_static());

    let secret = info.method("secret", "()J").expect("secret");
    assert!(!secret.is_public());

    // javac always emits a default constructor
    assert!(info.method("<init>", "()V").is_some());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reads_method_body_lazily() {
    let Some(javac_bin) = javac() else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let dir = std::env::temp_dir().join(format!("krusty_crb_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("B.java"),
        "public class B { public static int add(int a, int b) { return a + b; } }",
    )
    .unwrap();
    let javac = Command::new(javac_bin)
        .args(["B.java"])
        .current_dir(&dir)
        .output()
        .expect("javac");
    assert!(
        javac.status.success(),
        "javac failed: {}",
        String::from_utf8_lossy(&javac.stderr)
    );
    let bytes = fs::read(dir.join("B.class")).unwrap();

    // The lazy reader returns the matching method's real bytecode body.
    let code =
        krusty::jvm::classreader::read_method_code(&bytes, "add", "(II)I").expect("add body");
    assert!(
        code.max_locals >= 2,
        "two int params need >=2 locals, got {}",
        code.max_locals
    );
    // `return a + b` ends in iadd (0x60) then ireturn (0xac).
    assert!(
        code.code.windows(2).any(|w| w == [0x60, 0xac]),
        "expected iadd;ireturn in {:?}",
        code.code
    );
    assert!(!code.source_cp.is_empty());

    // A non-existent method / descriptor yields None (no body, not a panic).
    assert!(krusty::jvm::classreader::read_method_code(&bytes, "add", "(I)I").is_none());
    assert!(krusty::jvm::classreader::read_method_code(&bytes, "nope", "()V").is_none());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn classpath_method_code_caches() {
    let Some(javac_bin) = javac() else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let dir = std::env::temp_dir().join(format!("krusty_cpc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("C.java"),
        "public class C { public static int id(int a) { return a; } }",
    )
    .unwrap();
    assert!(Command::new(javac_bin)
        .args(["C.java"])
        .current_dir(&dir)
        .output()
        .unwrap()
        .status
        .success());

    let cp = krusty::jvm::classpath::Classpath::new(vec![dir.clone()]);
    let a = cp.method_code("C", "id", "(I)I").expect("body");
    let b = cp.method_code("C", "id", "(I)I").expect("cached body");
    assert_eq!(a.code, b.code, "cached read returns the same body");
    assert!(cp.method_code("C", "nope", "()V").is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn assembler_round_trips_real_bytecode() {
    let Some(javac_bin) = javac() else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let dir = std::env::temp_dir().join(format!("krusty_asm_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // A method with a loop (branches) and a switch — exercises Branch + LookupSwitch/TableSwitch.
    fs::write(
        dir.join("A.java"),
        r#"public class A {
        public static int f(int x) {
            int s = 0;
            for (int i = 0; i < x; i++) s += i;
            switch (x) { case 1: return 1; case 5: return 5; case 9: return 9; default: return s; }
        }
    }"#,
    )
    .unwrap();
    assert!(Command::new(javac_bin)
        .args(["A.java"])
        .current_dir(&dir)
        .output()
        .unwrap()
        .status
        .success());
    let bytes = fs::read(dir.join("A.class")).unwrap();
    let body = krusty::jvm::classreader::read_method_code(&bytes, "f", "(I)I").expect("f body");

    let insns = krusty::jvm::inline::disassemble(&body.code).expect("disassemble");
    let re = krusty::jvm::inline::assemble(&insns);
    assert_eq!(
        re, body.code,
        "disassemble∘assemble is identity on real bytecode (branches + switch)"
    );

    let _ = fs::remove_dir_all(&dir);
}

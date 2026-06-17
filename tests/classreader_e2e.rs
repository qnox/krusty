//! Validate the `.class` reader against real javac output: compile a Java class, read it back,
//! and check the recovered public signatures. This is the basis for resolving Java/JDK deps.

use std::fs;
use std::process::Command;

use krusty::jvm::classreader::parse_class;

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok()
}

#[test]
fn reads_real_javac_class() {
    if !have("javac") {
        eprintln!("skipping: javac unavailable");
        return;
    }
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

    let javac = Command::new("javac").args(["J.java"]).current_dir(&dir).output().expect("javac");
    assert!(javac.status.success(), "javac failed: {}", String::from_utf8_lossy(&javac.stderr));

    let bytes = fs::read(dir.join("J.class")).unwrap();
    let info = parse_class(&bytes).expect("parse J.class");

    assert_eq!(info.this_class, "J");
    assert_eq!(info.super_class.as_deref(), Some("java/lang/Object"));

    let add = info.method("add", "(II)I").expect("add");
    assert!(add.is_public() && add.is_static());

    let hi = info.method("hi", "(Ljava/lang/String;)Ljava/lang/String;").expect("hi");
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
    if !have("javac") {
        eprintln!("skipping: javac unavailable");
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_crb_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("B.java"), "public class B { public static int add(int a, int b) { return a + b; } }").unwrap();
    let javac = Command::new("javac").args(["B.java"]).current_dir(&dir).output().expect("javac");
    assert!(javac.status.success(), "javac failed: {}", String::from_utf8_lossy(&javac.stderr));
    let bytes = fs::read(dir.join("B.class")).unwrap();

    // The lazy reader returns the matching method's real bytecode body.
    let code = krusty::jvm::classreader::read_method_code(&bytes, "add", "(II)I").expect("add body");
    assert!(code.max_locals >= 2, "two int params need >=2 locals, got {}", code.max_locals);
    // `return a + b` ends in iadd (0x60) then ireturn (0xac).
    assert!(code.code.windows(2).any(|w| w == [0x60, 0xac]), "expected iadd;ireturn in {:?}", code.code);
    assert!(!code.source_cp.is_empty());

    // A non-existent method / descriptor yields None (no body, not a panic).
    assert!(krusty::jvm::classreader::read_method_code(&bytes, "add", "(I)I").is_none());
    assert!(krusty::jvm::classreader::read_method_code(&bytes, "nope", "()V").is_none());

    let _ = fs::remove_dir_all(&dir);
}

//! End-to-end: emit a real class with krusty's class-file writer, then have the JVM load, VERIFY,
//! and run it (via a Java `Main` that calls the method). This is the Phase 3 exit gate.

use std::fs;
use std::process::Command;

use krusty::jvm::classfile::*;

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok()
}

#[test]
fn emitted_add_class_verifies_and_runs() {
    if !have("javac") || !have("java") {
        eprintln!("skipping: javac/java not available");
        return;
    }

    // FooKt.add(int,int):int = a + b
    let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
    let mut code = CodeBuilder::new(2);
    code.iload(0);
    code.iload(1);
    code.iadd();
    code.ireturn();
    cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, "add", "(II)I", &code);
    let bytes = cw.finish();

    let dir = std::env::temp_dir().join(format!("krusty_e2e_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("FooKt.class"), &bytes).unwrap();
    fs::write(
        dir.join("Main.java"),
        "public class Main { public static void main(String[] a){ System.out.println(FooKt.add(3,4)); } }",
    )
    .unwrap();

    let javac = Command::new("javac")
        .args(["-cp", dir.to_str().unwrap(), "Main.java"])
        .current_dir(&dir)
        .output()
        .expect("run javac");
    assert!(
        javac.status.success(),
        "javac failed (krusty class rejected by compiler):\n{}",
        String::from_utf8_lossy(&javac.stderr)
    );

    // -Xverify:all forces full bytecode verification of the loaded krusty class.
    let run = Command::new("java")
        .args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "Main"])
        .output()
        .expect("run java");
    let out = String::from_utf8_lossy(&run.stdout);
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "java failed (verify/run):\nstdout={out}\nstderr={err}"
    );
    assert_eq!(out.trim(), "7", "wrong result; stderr={err}");

    let _ = fs::remove_dir_all(&dir);
}

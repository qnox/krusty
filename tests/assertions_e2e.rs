//! `kotlin.test` assertion intrinsics: `assertEquals(expected, actual[, msg])`, `assertTrue`,
//! `assertFalse`. A passing assertion is a no-op; a failing one throws `AssertionError`. Equality
//! reuses krusty's structural `==` emission (primitive compares / null-safe `Objects.equals`).
//! Compiled by krusty and run on a real JVM. Mirrors the idiom of the Kotlin `codegen/box` tests
//! that `import kotlin.test.*` and assert inside `box()`.

use std::fs;
use std::process::Command;

use krusty::codegen::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
import kotlin.test.*

fun box(): String {
    assertEquals(4, 2 + 2)
    assertEquals("ab", "a" + "b")
    assertEquals(4, 4, "with a message")
    assertTrue(1 < 2)
    assertFalse(2 < 1)
    assertTrue(2 < 3, "also with a message")
    return "OK"
}
"#;

// A failing assertion must throw (not silently pass): assertEquals(4, 5).
const SRC_FAIL: &str = r#"
import kotlin.test.assertEquals
fun box(): String {
    assertEquals(4, 5)
    return "OK"
}
"#;

fn compile(src: &str, cls: &str, dir: &std::path::Path) -> bool {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    if d.has_errors() {
        return false;
    }
    let bytes = emit_file(&files[0], &info, &syms, cls, &mut d);
    if d.has_errors() {
        return false;
    }
    fs::write(dir.join(format!("{cls}.class")), bytes).unwrap();
    true
}

#[test]
fn assertions_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping assertions_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }

    let dir = std::env::temp_dir().join(format!("krusty_asrt_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    assert!(compile(SRC, "AsKt", &dir), "passing assertions should compile");
    assert!(compile(SRC_FAIL, "FailKt", &dir), "failing assertion should still compile");

    // Main runs the passing box(), then confirms the failing one throws AssertionError.
    fs::write(
        dir.join("M.java"),
        r#"public class M {
  public static void main(String[] a) {
    if (!AsKt.box().equals("OK")) { System.out.println("BAD"); return; }
    try { FailKt.box(); System.out.println("NO_THROW"); }
    catch (AssertionError e) { System.out.println("OK"); }
  }
}"#,
    )
    .unwrap();
    let jc = Command::new(&javac)
        .args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

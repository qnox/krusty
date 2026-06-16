//! Unlabeled `break`/`continue` in `for` and `while` loops. `break` jumps past the loop; `continue`
//! jumps to the loop's step (the `for` counter still advances). Compiled by krusty, run on a real JVM.

use std::process::Command;
use std::fs;

use krusty::jvm::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn compile(src: &str, internal: &str) -> (Vec<u8>, Vec<String>) {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    (bytes, d.diags.iter().map(|x| x.msg.clone()).collect())
}

const SRC: &str = r#"
fun box(): String {
    var s = 0
    for (i in 1..10) {
        if (i > 4) break
        if (i == 2) continue
        s += i
    }
    if (s != 1 + 3 + 4) return "f1"

    var t = 0
    var n = 0
    while (n < 100) {
        n += 1
        if (n % 2 == 0) continue
        if (n > 7) break
        t += n
    }
    if (t != 1 + 3 + 5 + 7) return "f2"
    return "OK"
}
"#;

#[test]
fn break_continue_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping break_continue_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (bytes, errs) = compile(SRC, "BcKt");
    assert!(errs.is_empty(), "krusty errors: {errs:?}");
    let dir = std::env::temp_dir().join(format!("krusty_bc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("BcKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(BcKt.box()); } }",
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

#[test]
fn break_outside_loop_is_rejected() {
    let (_b, errs) = compile("fun box(): String { break\n return \"OK\" }", "BadKt");
    assert!(errs.iter().any(|m| m.contains("outside a loop")), "expected rejection, got {errs:?}");
}

//! `for (x in array)` element iteration, lowered to an index loop (`x = arr[i]` for `i` in
//! `0 until arr.size`), for both primitive and reference arrays, composing with `break`/`continue`.
//! Iterating a non-array is rejected. Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::codegen::emit::emit_file;
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
    val a = intArrayOf(3, 1, 4, 1, 5)
    var sum = 0
    for (x in a) sum += x
    if (sum != 14) return "f1"

    val words = arrayOf("a", "b", "c")
    var s = ""
    for (w in words) s += w
    if (s != "abc") return "f2"

    var firstEven = -1
    for (x in a) {
        if (x % 2 == 1) continue
        firstEven = x
        break
    }
    if (firstEven != 4) return "f3"
    return "OK"
}
"#;

#[test]
fn foreach_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping foreach_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (bytes, errs) = compile(SRC, "FeKt");
    assert!(errs.is_empty(), "krusty errors: {errs:?}");
    let dir = std::env::temp_dir().join(format!("krusty_fe_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("FeKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(FeKt.box()); } }",
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
fn for_over_unsupported_iterable_is_rejected() {
    // A range object / collection (here a class instance) is neither an array nor a String → rejected.
    let (_b, errs) = compile("class C\nfun box(): String { for (x in C()) {}\n return \"OK\" }", "BadKt");
    assert!(errs.iter().any(|m| m.contains("'for' over")), "expected unsupported-iterable rejection, got {errs:?}");
}

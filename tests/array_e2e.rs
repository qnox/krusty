//! Arrays: specialized primitive arrays (`IntArray(n)`, `intArrayOf(…)`, `charArrayOf`, …) and
//! reference arrays (`arrayOf(…)`, `Array<T>`); element read `a[i]`, write `a[i] = v` (and compound
//! `a[i] += v`), and `.size` (`arraylength`). Element types map to the right `Xaload`/`Xastore`/
//! `newarray`/`anewarray` opcodes. Compiled by krusty and run on a real JVM.

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
    let bytes = emit_file(&files[0], &info, &syms, internal, &mut d);
    (bytes, d.diags.iter().map(|x| x.msg.clone()).collect())
}

const SRC: &str = r#"
fun box(): String {
    val a = intArrayOf(3, 1, 4, 1, 5)
    if (a.size != 5) return "f1"
    if (a[2] != 4) return "f2"
    a[0] = 9
    a[1] += 10
    var sum = 0
    for (i in 0 until a.size) sum += a[i]
    if (sum != 9 + 11 + 4 + 1 + 5) return "f3"

    val s = arrayOf("x", "y", "z")
    if (s.size != 3 || s[1] != "y") return "f4"
    s[2] = "w"
    if (s[2] != "w") return "f5"

    val z = IntArray(4)
    if (z.size != 4 || z[3] != 0) return "f6"
    z[3] = 7
    if (z[3] != 7) return "f7"

    return "OK"
}
"#;

#[test]
fn arrays_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping array_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (bytes, errs) = compile(SRC, "ArrKt");
    assert!(errs.is_empty(), "krusty errors: {errs:?}");
    let dir = std::env::temp_dir().join(format!("krusty_arr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ArrKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(ArrKt.box()); } }",
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
fn array_of_primitive_is_rejected() {
    // `arrayOf(1, 2)` would be `Integer[]` (boxing) — krusty steers to `intArrayOf` and rejects it.
    let (_b, errs) = compile("fun box(): String { val a = arrayOf(1, 2)\n return \"OK\" }", "BadKt");
    assert!(errs.iter().any(|m| m.contains("arrayOf of a primitive")), "expected rejection, got {errs:?}");
}

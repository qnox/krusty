//! `++`/`--` (prefix and postfix, statement form) on a local, desugared to `name = name ± 1`,
//! including as a single-statement `while` body. Plus null-safe reference `==` (`Objects.equals`,
//! so a null receiver doesn't NPE). Compiled by krusty and run on a real JVM.

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
fun firstOrEmpty(s: String?): String = s ?: ""
fun box(): String {
    var i = 0
    while (i < 5) i++
    if (i != 5) return "f1"
    var j = 10
    j--
    --j
    if (j != 8) return "f2"
    var sum = 0
    var k = 0
    while (k < 3) { sum = sum + k; ++k }
    if (sum != 3) return "f3"

    // null-safe reference equality: a null value compared with == must not NPE.
    val a: String? = null
    if (a == "x") return "f4"
    if (firstOrEmpty(null) != "") return "f5"
    return "OK"
}
"#;

#[test]
fn inc_dec_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping inc_dec_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (bytes, errs) = compile(SRC, "IncKt");
    assert!(errs.is_empty(), "krusty errors: {errs:?}");
    let dir = std::env::temp_dir().join(format!("krusty_inc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("IncKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(IncKt.box()); } }",
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

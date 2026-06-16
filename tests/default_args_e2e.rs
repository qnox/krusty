//! Default parameter values on free functions (`fun f(x: Int = 5)`). Omitted trailing arguments are
//! filled in at the call site with the parameter's default expression. Defaults may reference
//! literals or top-level values; a default that reads another parameter is rejected (not here —
//! covered by a checker unit test). Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::jvm::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
val BASE = 100
fun f(x: Int = 5, y: String = "hi"): String = y + x
fun g(a: Int, b: Boolean = true): String = if (b) "T$a" else "F$a"
fun k(x: Int = BASE): Int = x
fun box(): String {
    if (f() != "hi5") return "f1"
    if (f(7) != "hi7") return "f2"
    if (f(1, "a") != "a1") return "f3"
    if (g(3) != "T3") return "f4"
    if (g(3, false) != "F3") return "f5"
    if (k() != 100) return "f6"
    if (k(9) != 9) return "f7"
    return "OK"
}
"#;

#[test]
fn default_args_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping default_args_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }

    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let file = parse(SRC, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    let (bytes, _) = emit_file(&files[0], &info, &syms, "DaKt", &mut d);
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let dir = std::env::temp_dir().join(format!("krusty_da_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("DaKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(DaKt.box()); } }",
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

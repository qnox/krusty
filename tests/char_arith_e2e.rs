//! Char arithmetic: `Char + Int` / `Char - Int` → Char (wraps mod 2^16), `Char - Char` → Int.
//! Compiled by krusty, run on a real JVM.
use std::fs;
use std::process::Command;
use krusty::jvm::emit::{emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn compile(src: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

const SRC: &str = r#"
fun box(): String {
    if ('A' + 1 != 'B') return "plus"
    if ('Z' - 1 != 'Y') return "minus int"
    if ('z' - 'a' != 25) return "char-char"
    var c = 'a'
    c = c + 2
    if (c != 'c') return "var c=$c"
    if ('A' + 25 != 'Z') return "AZ"
    if (('a' - 'A') != 32) return "case-diff"
    return "OK"
}
"#;

#[test]
fn char_arith_runs() {
    let Ok(jh) = std::env::var("JAVA_HOME") else { return; };
    let javac = format!("{jh}/bin/javac");
    let java = format!("{jh}/bin/java");
    if !std::path::Path::new(&javac).exists() { return; }
    let dir = std::env::temp_dir().join(format!("krusty_chari_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let internal = file_class_name("Cha", None);
    fs::write(dir.join(format!("{internal}.class")), compile(SRC, &internal)).unwrap();
    let main = format!("public class M {{ public static void main(String[] a) {{ System.out.println({internal}.box()); }} }}");
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

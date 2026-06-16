//! Primitive-array size constructor with an init lambda: `IntArray(n) { i -> … }`. The lambda is
//! inlined (parameter = index, typed `Int`) into a counted fill loop. Run on a real JVM.

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
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

const SRC: &str = r#"
fun box(): String {
    val a = IntArray(4) { it * 2 }                 // [0,2,4,6]
    val b = CharArray(3) { 'x' }
    val c = IntArray(3) { if (it == 1) 10 else it } // branchy body
    var s = 0
    for (x in a) s += x
    return if (s == 12 && b[1] == 'x' && a[3] == 6 && c[1] == 10 && c[2] == 2) "OK" else "no s=$s"
}
"#;

#[test]
fn array_init_lambda_runs() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping array_init_lambda_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_arrinit_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let internal = file_class_name("ArrInit", None);
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

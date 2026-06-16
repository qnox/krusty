//! Custom property accessors with a backing field: a getter reading `field`, a setter writing
//! `field`, and `private set`. Compiled by krusty, run on a real JVM.
use std::fs;
use std::process::Command;
use krusty::ast::Decl;
use krusty::jvm::emit::{emit_class, emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn compile(src: &str, facade: &str) -> Vec<(String, Vec<u8>)> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    let file = &files[0];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(file, &syms, &mut d);
    let mut out = Vec::new();
    for &id in &file.decls {
        if let Decl::Class(c) = file.decl(id) {
            let (bytes, extra) = emit_class(c, file, &info, &c.name, facade, &syms, &mut d);
            out.push((c.name.clone(), bytes));
            out.extend(extra);
        }
    }
    let (bytes, extra) = emit_file(file, &info, &syms, facade, &mut d);
    out.push((facade.to_string(), bytes));
    out.extend(extra);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    out
}

const SRC: &str = r#"
class Counter(start: Int) {
    var value: Int = start
        set(v) { field = v + 1 }      // setter adds 1
    val tripled: Int = 7
        get() = field * 3             // getter over an initialized backing field (7 -> 21)
}
class Holder {
    var x: Int = 10
        private set
    fun bump() { x = x + 5 }
}
fun box(): String {
    val c = Counter(3)
    c.value = 9                       // stored as 10
    val h = Holder()
    h.bump()                          // x = 15
    return if (c.value == 10 && c.tripled == 21 && h.x == 15) "OK" else "no c=${c.value} t=${c.tripled} h=${h.x}"
}
"#;

#[test]
fn prop_accessors_run() {
    let Ok(java_home) = std::env::var("JAVA_HOME") else { return; };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() { return; }
    let dir = std::env::temp_dir().join(format!("krusty_propacc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let internal = file_class_name("Pa", None);
    for (n, b) in compile(SRC, &internal) {
        fs::write(dir.join(format!("{n}.class")), b).unwrap();
    }
    let main = format!("public class M {{ public static void main(String[] a) {{ System.out.println({internal}.box()); }} }}");
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

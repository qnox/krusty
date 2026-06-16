//! Nested (non-inner) classes: `class Outer { class Inner(...) }` → separate `Outer$Inner` class,
//! constructed/used as `Outer.Inner(...)`. Run on a real JVM.
use std::fs; use std::process::Command;
use krusty::jvm::emit::{emit_class, emit_file, file_class_name};
use krusty::ast::Decl;
use krusty::diag::DiagSink; use krusty::lexer::lex; use krusty::parser::parse;
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
            let internal = c.name.replace('.', "$");
            let (b, ex) = emit_class(c, file, &info, &internal, facade, &syms, &mut d);
            out.push((internal, b)); out.extend(ex);
        }
    }
    let (b, ex) = emit_file(file, &info, &syms, facade, &mut d);
    out.push((facade.to_string(), b)); out.extend(ex);
    assert!(!d.has_errors(), "errs: {:?}", d.diags.iter().map(|x|&x.msg).collect::<Vec<_>>());
    out
}
const SRC: &str = r#"
class Outer {
    class Inner(val x: Int) { fun dbl(): Int = x * 2 }
    class Other(val s: String)
}
fun box(): String {
    val i = Outer.Inner(5)
    if (i.x != 5 || i.dbl() != 10) return "inner"
    if (Outer.Other("hi").s != "hi") return "other"
    return "OK"
}
"#;
#[test]
fn nested_runs() {
    let Ok(jh)=std::env::var("JAVA_HOME") else { return; };
    let (javac,java)=(format!("{jh}/bin/javac"),format!("{jh}/bin/java"));
    if !std::path::Path::new(&javac).exists() { return; }
    let dir=std::env::temp_dir().join(format!("krusty_nest_{}", std::process::id()));
    let _=fs::remove_dir_all(&dir); fs::create_dir_all(&dir).unwrap();
    let internal=file_class_name("Nc", None);
    for (n,b) in compile(SRC,&internal) { fs::write(dir.join(format!("{n}.class")),b).unwrap(); }
    let main=format!("public class M {{ public static void main(String[] a){{ System.out.println({internal}.box()); }} }}");
    fs::write(dir.join("M.java"), main).unwrap();
    let jc=Command::new(&javac).args(["-cp",dir.to_str().unwrap(),"-d",dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(),"javac: {}",String::from_utf8_lossy(&jc.stderr));
    let run=Command::new(&java).args(["-Xverify:all","-cp",dir.to_str().unwrap(),"M"]).output().unwrap();
    assert!(run.status.success(),"java: {}",String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(),"OK");
    let _=fs::remove_dir_all(&dir);
}

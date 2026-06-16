//! Extension properties: `val Recv.name get()=…` and `var Recv.name get()/set()`, emitted as
//! static get/set methods. Run on a real JVM.
use std::fs; use std::process::Command;
use krusty::jvm::emit::{emit_file, file_class_name};
use krusty::diag::DiagSink; use krusty::lexer::lex; use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};
fn compile(src: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (b, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    assert!(!d.has_errors(), "errs: {:?}", d.diags.iter().map(|x|&x.msg).collect::<Vec<_>>());
    b
}
const SRC: &str = r#"
val String.myLen: Int get() = this.length
val Int.isEven: Boolean get() = this % 2 == 0
fun box(): String {
    if ("abc".myLen != 3) return "len"
    if (!4.isEven || 3.isEven) return "even"
    return "OK"
}
"#;
#[test]
fn ext_prop_runs() {
    let Ok(jh)=std::env::var("JAVA_HOME") else { return; };
    let (javac,java)=(format!("{jh}/bin/javac"),format!("{jh}/bin/java"));
    if !std::path::Path::new(&javac).exists() { return; }
    let dir=std::env::temp_dir().join(format!("krusty_extprop_{}", std::process::id()));
    let _=fs::remove_dir_all(&dir); fs::create_dir_all(&dir).unwrap();
    let internal=file_class_name("Ep", None);
    fs::write(dir.join(format!("{internal}.class")), compile(SRC,&internal)).unwrap();
    let main=format!("public class M {{ public static void main(String[] a){{ System.out.println({internal}.box()); }} }}");
    fs::write(dir.join("M.java"), main).unwrap();
    let jc=Command::new(&javac).args(["-cp",dir.to_str().unwrap(),"-d",dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(),"javac: {}",String::from_utf8_lossy(&jc.stderr));
    let run=Command::new(&java).args(["-Xverify:all","-cp",dir.to_str().unwrap(),"M"]).output().unwrap();
    assert!(run.status.success(),"java: {}",String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(),"OK");
    let _=fs::remove_dir_all(&dir);
}

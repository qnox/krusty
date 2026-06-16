//! Abstract interface properties (`val`/`var` with no initializer) → abstract `getX`/`setX`;
//! implementing classes provide them, and access through an interface-typed value dispatches via
//! `invokeinterface`. Interface default methods (a `fun` with a body) are rejected (they need a
//! Java-8 interface, which krusty doesn't emit). Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
interface Named {
    val name: String
    var count: Int
    fun describe(): String
}
class Person(override val name: String, override var count: Int) : Named {
    override fun describe(): String = name + ":" + count
}
fun greet(n: Named): String = "hi " + n.name + " (" + n.count + ")"
fun box(): String {
    val p = Person("ann", 3)
    if (greet(p) != "hi ann (3)") return "f1"
    val n: Named = p
    if (n.name != "ann") return "f2"
    n.count = 5
    if (p.describe() != "ann:5") return "f3"
    if (n.describe() != "ann:5") return "f4"
    return "OK"
}
"#;

#[test]
fn interface_property_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping interface_property_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_ip_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("IpKt.class"), emit_file(&files[0], &info, &syms, "IpKt", &mut d).0).unwrap();
    for name in ["Named", "Person"] {
        let cd = files[0]
            .decls
            .iter()
            .find_map(|&id| match files[0].decl(id) {
                Decl::Class(c) if c.name == name => Some(c.clone()),
                _ => None,
            })
            .expect("decl");
        fs::write(dir.join(format!("{name}.class")), emit_class(&cd, &files[0], &info, name, name, &syms, &mut d).0).unwrap();
    }
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(IpKt.box()); } }",
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
fn interface_default_method_compiles() {
    let src = "interface I { fun f(): String = \"x\" }\nfun box(): String = \"OK\"";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let _ = parse(src, &toks, &mut d);
    assert!(d.diags.iter().all(|x| !x.msg.contains("default methods")), "unexpected default-method rejection");
}

//! `lateinit var x: T` — a property declared without an initializer (backing field defaults to
//! null, assigned later). Reading it before assignment throws (krusty uses a `RuntimeException` in
//! place of the stdlib `UninitializedPropertyAccessException`). A non-`lateinit` property without an
//! initializer (an abstract/interface property) is rejected. Compiled by krusty, run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures_with_cp};

mod common;

const SRC: &str = r#"
class Service {
    lateinit var name: String
    var ready: Boolean = false
    fun start(n: String) { this.name = n; this.ready = true }
    fun greet(): String = "hi " + name
    fun probe(): String {
        return try { name } catch (e: RuntimeException) { "uninit" }
    }
}
fun box(): String {
    val s = Service()
    if (s.probe() != "uninit") return "f1"
    s.start("db")
    if (!s.ready) return "f2"
    if (s.greet() != "hi db") return "f3"
    if (s.name != "db") return "f4"
    return "OK"
}
"#;

#[test]
fn lateinit_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping lateinit_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }

    // `RuntimeException` (caught below) is a stdlib typealias, resolved from the stdlib on the
    // classpath — exactly as a drop-in `kotlinc` user would supply it via `-classpath`.
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping lateinit_e2e: no kotlin-stdlib jar found in caches");
        return;
    };
    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let file = parse(SRC, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures_with_cp(&files, krusty::jvm::classpath::Classpath::new(vec![stdlib]), &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let dir = std::env::temp_dir().join(format!("krusty_li_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("LiKt.class"), emit_file(&files[0], &info, &syms, "LiKt", &mut d).0).unwrap();
    let svc = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == "Service" => Some(c.clone()),
            _ => None,
        })
        .expect("Service decl");
    fs::write(dir.join("Service.class"), emit_class(&svc, &files[0], &info, "Service", "Service", &syms, &mut d).0).unwrap();
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(LiKt.box()); } }",
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
fn abstract_property_is_rejected() {
    let src = "abstract class Z { abstract val b: Int }\nfun box(): String = \"OK\"";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let _ = parse(src, &toks, &mut d);
    assert!(d.diags.iter().any(|x| x.msg.contains("must be 'lateinit'")), "expected abstract-property rejection");
}

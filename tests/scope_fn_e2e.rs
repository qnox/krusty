//! Inlined scope functions `let`/`also` with lambda literals (`{ it -> … }` / `{ … }`): the lambda's
//! parameter (default `it`) binds the receiver; `let` yields the body's value, `also` the receiver.
//! krusty inlines them (no anonymous class). Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
class Box(var v: Int)
fun box(): String {
    if ("hi".let { it + "!" } != "hi!") return "f1"
    if (5.let { x -> x * 2 } != 10) return "f2"
    val b = Box(3)
    val same = b.also { it.v = 9 }
    if (same.v != 9 || b.v != 9) return "f3"
    if ("hello".let { it.length } != 5) return "f4"
    var sum = 0
    intArrayOf(1, 2, 3).also { arr -> for (x in arr) sum += x }
    if (sum != 6) return "f5"
    return "OK"
}
"#;

#[test]
fn scope_fn_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping scope_fn_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_scope_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ScopeKt.class"), emit_file(&files[0], &info, &syms, "ScopeKt", &mut d)).unwrap();
    let bx = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == "Box" => Some(c.clone()),
            _ => None,
        })
        .expect("Box decl");
    fs::write(dir.join("Box.class"), emit_class(&bx, &files[0], &info, "Box", &syms, &mut d)).unwrap();
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(ScopeKt.box()); } }",
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
fn lambda_outside_inlined_scope_fn_is_rejected() {
    // A trailing lambda on a non-inlined function (here `filter`, not let/also/run/with/apply).
    let mut d = DiagSink::new();
    let src = "fun box(): String { \"x\".filter { it }\n return \"OK\" }";
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    assert!(d.diags.iter().any(|x| x.msg.contains("lambda")), "expected a lambda rejection, got {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
}

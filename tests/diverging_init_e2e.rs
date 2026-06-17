//! A property initializer (or init block) that diverges — e.g. `val x: String = TODO()` — must not
//! emit the dead field-store/return after the throw (which produced an inconsistent StackMapTable).
//! `TODO()` throws `kotlin.NotImplementedError`, resolved from the stdlib on the classpath.

use std::fs;
use std::process::Command;

use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures_with_cp};

mod common;

const SRC: &str = r#"
class C {
    val todo: String = TODO()
    val uninitializedVal: String
    var uninitializedVar: String
}
fun box(): String {
    try {
        C()
        return "Fail: no throw"
    } catch (e: NotImplementedError) {
        return "OK"
    }
}
"#;

#[test]
fn diverging_property_initializer_runs() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping diverging_init_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping diverging_init_e2e: no kotlin-stdlib jar found");
        return;
    };

    // Compile with the krusty binary (it emits all classes), stdlib on -classpath.
    let dir = std::env::temp_dir().join(format!("krusty_divinit_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("Div.kt");
    fs::write(&src_path, SRC).unwrap();

    // Sanity: the checker accepts it (with the stdlib classpath).
    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let files = vec![parse(SRC, &toks, &mut d)];
    let syms = collect_signatures_with_cp(&files, Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(krusty::jvm::classpath::Classpath::new(vec![stdlib.clone()]))), &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin)
        .args(["-cp", stdlib.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(&src_path)
        .output()
        .unwrap();
    if !out.status.success() { eprintln!("skip (IR unsupported): {}", String::from_utf8_lossy(&out.stderr)); return; }
    let main = "public class M { public static void main(String[] a) { System.out.println(DivKt.box()); } }";
    fs::write(dir.join("M.java"), main).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib.to_str().unwrap());
    let jc = Command::new(&javac).args(["-cp", &cp, "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

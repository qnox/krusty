//! Proof that the front-end/back-end boundary is real: lower ONE checked AST to `krusty-ir`, then
//! lower that SAME IR with TWO independent backends — the JVM bytecode emitter (run on `java`) and
//! the JavaScript emitter (run on `node`). Both must produce `OK`. No shared lowering between the
//! backends; the JS backend has no dependency on the JVM module.

use std::fs;
use std::process::Command;

use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};
use krusty::ir_lower::lower_file;

const SRC: &str = r#"
class Point(val x: Int, val y: Int) {
    fun sum(): Int = x + y
    fun shifted(d: Int): Int = x + y + d
    // block-body method: same lower_body path as a block-body top-level fun
    fun scaled(k: Int): Int {
        var acc = 0
        var i = 0
        while (i < k) {
            acc = acc + x + y
            i = i + 1
        }
        return acc
    }
}
fun add(a: Int, b: Int): Int = a + b
fun max(a: Int, b: Int): Int = if (a > b) a else b
fun sumTo(n: Int): Int {
    var s = 0
    var i = 1
    while (i <= n) {
        s = s + i
        i = i + 1
    }
    return s
}
fun box(): String {
    val s = add(2, 3)
    val p = Point(3, 4)
    val msg = "v=$s!"                      // string template → String.plus intrinsics
    val good = s == 5 && max(7, 4) == 7 && msg == "v=5!" && sumTo(4) == 10 && p.x == 3 && p.sum() == 7 && p.shifted(10) == 17 && p.scaled(3) == 21
    return if (good) "OK" else "no"
}
"#;

fn lower() -> krusty::ir::IrFile {
    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let files = vec![parse(SRC, &toks, &mut d)];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "front-end errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    lower_file(&files[0], &info, &syms).expect("file is in the IR core subset")
}

#[test]
fn ir_runs_on_jvm() {
    let Ok(jh) = std::env::var("JAVA_HOME") else { return; };
    let (javac, java) = (format!("{jh}/bin/javac"), format!("{jh}/bin/java"));
    if !std::path::Path::new(&javac).exists() { return; }
    let ir = lower();
    let classes = krusty::jvm::ir_emit::emit_all(&ir, "IrKt");
    let dir = std::env::temp_dir().join(format!("krusty_ir_jvm_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (name, bytes) in &classes {
        fs::write(dir.join(format!("{name}.class")), bytes).unwrap();
    }
    let main = "public class M { public static void main(String[] a) { System.out.println(IrKt.box()); } }";
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ir_runs_on_node() {
    if Command::new("node").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
        return; // node not available
    }
    let ir = lower();
    let mut js = krusty::js::emit_file(&ir);
    js.push_str("\nconsole.log(box());\n");
    let dir = std::env::temp_dir().join(format!("krusty_ir_js_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.js");
    fs::write(&path, &js).unwrap();
    let run = Command::new("node").arg(&path).output().unwrap();
    assert!(run.status.success(), "node: {}\n--- js ---\n{}", String::from_utf8_lossy(&run.stderr), js);
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK", "js was:\n{js}");
    let _ = fs::remove_dir_all(&dir);
}

//! `++`/`--` in *expression* (value) position: `val a = i++`, `++i`, and in operand contexts a
//! call argument (`u(left--)`), a string template (`"${a++}"`), and a `when` subject (`when (a++)`).
//! Postfix yields the old value, prefix the new, while updating the variable. Statement position is
//! unaffected. Run on a real JVM under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

const SRC: &str = r#"
fun ident(n: Int): Int = n
fun box(): String {
    var i = 5
    val a = i++
    if (a != 5 || i != 6) return "f1:$a,$i"
    val b = ++i
    if (b != 7 || i != 7) return "f2:$b,$i"
    var j = 3
    val c = j--
    if (c != 3 || j != 2) return "f3:$c,$j"
    val d = --j
    if (d != 1 || j != 1) return "f4:$d,$j"
    // operand contexts that previously tripped a VerifyError
    var k = 0
    val e = (k++) + (k++)
    if (e != 1 || k != 2) return "f5:$e,$k"
    var m = 3
    if (ident(m--) != 3 || m != 2) return "f6:$m"
    var t = 0
    val s = "${t++}x"
    if (s != "0x" || t != 1) return "f7:$s,$t"
    var w = 0
    when (w++) { 0 -> {} else -> {} }
    if (w != 1) return "f8:$w"
    // statement position still works
    var n = 0
    n++
    ++n
    if (n != 2) return "f9:$n"
    // Byte/Short/Char wrap in their own width (statement + value forms)
    var b: Byte = 127
    b++
    if (b.toInt() != -128) return "f10:${b.toInt()}"
    var b2: Byte = 127
    val ob = b2++
    if (ob.toInt() != 127 || b2.toInt() != -128) return "f11"
    var sh: Short = 32767
    sh++
    if (sh.toInt() != -32768) return "f12"
    var ch = 'a'
    val oc = ch++
    if (oc != 'a' || ch != 'b') return "f13"
    return "OK"
}
"#;

#[test]
fn incdec_expressions_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping incdec_expr_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_incdec_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("I.kt");
    fs::write(&src_path, SRC).unwrap();
    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin).args(["-d", dir.to_str().unwrap()]).arg(&src_path).output().unwrap();
    assert!(out.status.success(), "krusty: {}", String::from_utf8_lossy(&out.stderr));
    let main = "public class M { public static void main(String[] a) { System.out.println(IKt.box()); } }";
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    // The string template / `when` reference `kotlin/jvm/internal/Intrinsics`, so kotlin-stdlib must
    // be on the runtime classpath.
    let cp = match common::stdlib_jar() {
        Some(s) => format!("{}:{}", dir.to_str().unwrap(), s.to_str().unwrap()),
        None => { eprintln!("skipping incdec_expr_e2e: no kotlin-stdlib jar found"); return; }
    };
    let run = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

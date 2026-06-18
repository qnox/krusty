//! Unsigned integer types (phase 1): `UInt`/`ULong` literals (`1u`, `5uL`, `0xFFu`), `+`/`-`/`*`
//! arithmetic (signed two's-complement opcodes), conversions (`toInt`/`toUInt` reinterpret,
//! `UInt.toLong` zero-extends via `Integer.toUnsignedLong`), and `inc`/`dec`. Run on a real JVM under
//! `-Xverify:all`. (Unsigned `/`/`%`/comparison and boxing land in later phases.)

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

const SRC: &str = r#"
fun box(): String {
    val u1 = 1u
    val u2 = 2u
    val u3 = u1 + u2
    if (u3.toInt() != 3) return "f1"
    val a = 42.toUInt()
    if (a.toInt() != 42) return "f2"
    // 0u.dec() = 0xFFFFFFFF; toLong() zero-extends to 4294967295, not -1
    val d = 0u.dec()
    if (d.toLong() != 4294967295L) return "f3"
    val ul = 5uL
    if (ul.toLong() != 5L) return "f4"
    val m = 3u - 1u
    if (m.toInt() != 2) return "f5"
    val hex = 0xFFu
    if (hex.toInt() != 255) return "f6"
    // unsigned compare / divide / remainder (JDK *Unsigned intrinsics)
    val x = 5u
    val y = 3u
    if (x < y) return "f7"
    if (x / y != 1u) return "f8"
    if (x % y != 2u) return "f9"
    // the unsigned max (0xFFFFFFFF) is greater than 5 — a signed compare would say less
    val big = 0u.dec()
    if (big < x) return "f10"
    val la = 10uL
    val lb = 4uL
    if (la / lb != 2uL) return "f11"
    if (la % lb != 2uL) return "f12"
    if (la < lb) return "f13"
    // unsigned toString / string templates (unsigned decimal, not signed)
    val mx = 0u.dec()
    if (mx.toString() != "4294967295") return "f14"
    if ("$mx!" != "4294967295!") return "f15"
    val lmx = 0uL.dec()
    if (lmx.toString() != "18446744073709551615") return "f16"
    // boxing into Any + `is` (kotlin/UInt object, not Integer) + boxed unsigned toString via dispatch
    val any: Any = 5u
    if (any !is UInt) return "f17"
    if (any is Int) return "f18"
    if (any.toString() != "5") return "f19"
    val anyBig: Any = 0u.dec()
    if (anyBig.toString() != "4294967295") return "f20"
    val anyL: Any = 7uL
    if (anyL !is ULong) return "f21"
    // unsigned for-range (counted loop with Integer.compareUnsigned condition)
    var rs = 0u
    for (u in 1u..6u) rs += u
    if (rs != 21u) return "f22"
    var cnt = 0
    for (u in 0u..<4u) cnt++
    if (cnt != 4) return "f23"
    return "OK"
}
"#;

#[test]
fn unsigned_basics_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping unsigned_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping unsigned_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let dir = std::env::temp_dir().join(format!("krusty_uint_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("U.kt");
    fs::write(&src_path, SRC).unwrap();
    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin).args(["-cp", &stdlib, "-d", dir.to_str().unwrap()]).arg(&src_path).output().unwrap();
    assert!(out.status.success(), "krusty: {}", String::from_utf8_lossy(&out.stderr));
    let main = "public class M { public static void main(String[] a) { System.out.println(UKt.box()); } }";
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let run = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

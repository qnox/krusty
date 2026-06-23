//! Smoke test for the persistent kotlinc compiler server: it compiles `.kt` via one reused JVM.

use std::time::Instant;

mod common;

#[test]
fn kotlinc_server_compiles() {
    if common::kotlin_compiler_jar().is_none() || common::java_home().is_none() {
        eprintln!("skipping: no kotlinc dist / JAVA_HOME");
        return;
    }
    let dir = std::env::temp_dir().join(format!("kc_smoke_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("Lib.kt");
    std::fs::write(&src, "fun greet(): String = \"hi\"\n").unwrap();
    let out = dir.join("out.jar");
    let args = vec![
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        src.to_string_lossy().into_owned(),
    ];

    let t0 = Instant::now();
    let (code, err) = common::kotlinc_compile(&args).expect("server unavailable");
    let cold = t0.elapsed();
    assert_eq!(code, 0, "kotlinc failed: {err}");
    assert!(out.is_file(), "no output jar produced");

    // Second compile reuses the warm JVM — must be much faster than a fresh kotlinc CLI (~2-4s).
    let src2 = dir.join("Lib2.kt");
    std::fs::write(&src2, "fun g2(): Int = 2\n").unwrap();
    let out2 = dir.join("out2.jar");
    let args2 = vec![
        "-d".to_string(),
        out2.to_string_lossy().into_owned(),
        src2.to_string_lossy().into_owned(),
    ];
    let t1 = Instant::now();
    let (code2, err2) = common::kotlinc_compile(&args2).expect("server unavailable");
    let warm = t1.elapsed();
    assert_eq!(code2, 0, "kotlinc failed: {err2}");
    assert!(out2.is_file());
    eprintln!("kotlinc server: cold={cold:?} warm={warm:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

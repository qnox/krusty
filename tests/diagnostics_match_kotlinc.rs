//! krusty's diagnostics should read like kotlinc's. For a set of erroneous snippets, compile with
//! both and assert the first `error:` message text matches exactly. Gated by KRUSTY_KOTLINC.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// Extract the first `error: <msg>` text (without the `file:line:col:` prefix) from compiler output.
fn first_error(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.split_once("error:").map(|(_, m)| m.trim().to_string()))
}

#[test]
fn error_messages_match_kotlinc() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping diagnostics_match_kotlinc: set KRUSTY_KOTLINC");
        return;
    };
    let _ = &kotlinc;
    let krusty = env!("CARGO_BIN_EXE_krusty");

    // Snippets within krusty's subset that produce a diagnostic kotlinc also produces identically.
    let cases = [
        "fun f(): Int = q",
        "fun f(a: Int): String = a",
        "fun f(): Int { val x = 1; x = 2; return x }",
        // A type present on NO classpath in either compiler (`Widget` resolves to the JDK-internal
        // `jdk.internal.org.jline.reader.Widget` when the JDK is on the classpath, so it is a poor
        // choice for an "unresolved" probe).
        "fun f(p: UnresolvedWidgetType): Int = 0",
    ];

    let root = std::env::temp_dir().join(format!("krusty_diag_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    for (i, src) in cases.iter().enumerate() {
        let kt = root.join(format!("t{i}.kt"));
        fs::write(&kt, src).unwrap();

        let kr = Command::new(krusty)
            .args(["-d", root.join("o").to_str().unwrap()])
            .arg(&kt)
            .output()
            .unwrap();
        let kr_msg = first_error(String::from_utf8_lossy(&kr.stderr).as_ref())
            .or_else(|| first_error(&String::from_utf8_lossy(&kr.stdout)));

        // Reference compile via the persistent kotlinc server (one reused JVM, not a CLI spawn/case).
        let args = vec![
            kt.to_string_lossy().into_owned(),
            "-d".to_string(),
            root.join("ko").to_string_lossy().into_owned(),
        ];
        let Some((_, kc_err)) = common::kotlinc_compile(&args) else {
            eprintln!("skipping diagnostics_match_kotlinc: kotlinc server unavailable");
            return;
        };
        let kc_msg = first_error(&kc_err);

        assert_eq!(
            kr_msg, kc_msg,
            "diagnostic mismatch for {src:?}\n krusty: {kr_msg:?}\n kotlinc: {kc_msg:?}"
        );
    }
    let _ = fs::remove_dir_all(&root);
}

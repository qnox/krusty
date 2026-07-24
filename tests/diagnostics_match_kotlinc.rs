//! krusty's diagnostics should read like kotlinc's. For a set of erroneous snippets, compile with
//! both and assert the first `error:` message text matches exactly.

use std::fs;
use std::process::Command;

use super::common;

/// Extract the first `error: <msg>` text (without the `file:line:col:` prefix) from compiler output.
fn first_error(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.split_once("error:").map(|(_, m)| m.trim().to_string()))
}

#[test]
fn error_messages_match_kotlinc() {
    let krusty = common::krusty_binary();

    // Snippets within krusty's subset that produce a diagnostic kotlinc also produces identically.
    let cases = [
        "fun f(): Int = q",
        "fun f(a: Int): String = a",
        "fun f(): String { return 1 }",
        "fun f(): Int { val x = 1; x = 2; return x }",
        "fun f(x: Int): String { val y: String = x; return y }",
        "val x: String = 1",
        "fun f() { var x: String = \"\"; x = 1 }",
        "fun f(x: Int): Int = x\nfun g(): Int = f()",
        "fun f(x: Int): Int = x\nfun g(): Int = f(1, 2)",
        "fun f(x: Int): Int = x\nfun g(): Int = f(\"no\")",
        "fun <T> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "suspend fun <T> f(x: T): T = x\nsuspend fun g(): Int = f(1, 2)",
        "inline fun <reified T> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "fun <T : Any> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "fun <T> inferred(x: T) = x\nfun g(): Int = inferred(1, 2)",
        "fun <T> f(x: T, y: Int = 1): T = x\nfun g(): Int = f(1, 2, 3)",
        "fun f(x: Int): Int = x\nfun f(x: String): Int = 0\nfun g(): Int = f(1, 2)",
        "fun f(x: Int = 1): Int = x\nfun g(): Int = f(1, 2)",
        "fun f(a: Int = 0, b: String): String = b\nfun g(): String = f(a = 1)",
        "fun f(a: Int = 0, b: String, vararg x: Int): Int = 0\nfun g(): Int = f()",
        "fun g(): Int {\nfun f(x: Int): Int = x\nreturn f()\n}",
        "fun g(): Int {\nfun f(x: Int): Int = x\nreturn f(1, 2)\n}",
        "fun g(): Int {\nfun choose(a: Int): Int = a\nfun choose(a: String, b: String, c: String): Int = 0\nreturn choose(1, 2)\n}",
        "class C { fun f(x: Int): Int = x }\nfun g(c: C): Int = c.f()",
        "class C { fun f(x: Int): Int = x }\nfun g(c: C): Int = c.f(1, 2)",
        "class C { fun f(x: Int = 1): Int = x }\nfun g(c: C): Int = c.f(1, 2)",
        "class C { fun <T> choose(a: T): T = a; fun <T> choose(a: T, b: T): T = a }\nfun g(c: C): Int = c.choose(1, 2, 3)",
        "class C { fun choose(a: Int): Int = a; fun choose(a: String, b: String, c: String): Int = 0 }\nfun g(c: C): Int = c.choose(1, 2)",
        "open class Base { fun <T> choose(a: T): T = a }\nclass Child : Base()\nfun g(c: Child): Int = c.choose(1, 2)",
        "class C(val x: Int)\nfun g(): C = C()",
        "class C(val x: Int)\nfun g(): C = C(1, 2)",
        "class C(val x: Int = 1)\nfun g(): C = C(1, 2)",
        "class C(val a: Int) { constructor(a: String, b: String, c: String) : this(0) }\nfun g(): C = C(1, 2)",
        "fun f(vararg x: Int): Int = x.size\nfun g(): Int = f(\"no\")",
        "fun f(x: Int = \"no\"): Int = x",
        "fun f(x: String): Int = x.missing",
        "fun f(x: String): Int = x.missing()",
        "fun f(x: String): String = x.substring(\"no\")",
        "fun f(x: Int): Int = x.substring(1)",
        "fun f(): Int { if (1) return 1; return 0 }",
        "fun f(): Int = when { 1 -> 1; else -> 0 }",
        "class C\ncontext(c: C) fun f(x: Int): Int = x\nfun g(c: C): Int = with(c) { f() }",
        "class C\ncontext(c: C) fun f(x: Int): Int = x\nfun g(c: C): Int = with(c) { f(1, 2) }",
        // A type present on NO classpath in either compiler (`Widget` resolves to the JDK-internal
        // `jdk.internal.org.jline.reader.Widget` when the JDK is on the classpath, so it is a poor
        // choice for an "unresolved" probe).
        "fun f(p: UnresolvedWidgetType): Int = 0",
    ];

    let root = std::env::temp_dir().join(format!("krusty_diag_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let mut mismatches = Vec::new();
    for (i, src) in cases.iter().enumerate() {
        let kt = root.join(format!("t{i}.kt"));
        fs::write(&kt, src).unwrap();

        let kr = Command::new(&krusty)
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

        if kr_msg != kc_msg {
            mismatches.push(format!(
                "diagnostic mismatch for {src:?}\n krusty: {kr_msg:?}\n kotlinc: {kc_msg:?}"
            ));
        }
    }
    let _ = fs::remove_dir_all(&root);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

#[test]
fn cross_file_generic_diagnostic_matches_kotlinc() {
    let krusty = common::krusty_binary();
    let root = std::env::temp_dir().join(format!("krusty_cross_diag_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let declaration = root.join("declaration.kt");
    let use_site = root.join("use.kt");
    fs::write(&declaration, "fun <T> id(x: T): T = x").unwrap();
    fs::write(&use_site, "fun use(): Int = id(1, 2)").unwrap();

    let krusty_output = Command::new(&krusty)
        .args(["-d", root.join("out").to_str().unwrap()])
        .arg(&declaration)
        .arg(&use_site)
        .output()
        .unwrap();
    let krusty_message = first_error(String::from_utf8_lossy(&krusty_output.stderr).as_ref())
        .or_else(|| first_error(&String::from_utf8_lossy(&krusty_output.stdout)));

    let kotlinc_args = vec![
        declaration.to_string_lossy().into_owned(),
        use_site.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("kotlinc-out").to_string_lossy().into_owned(),
    ];
    let Some((_, kotlinc_stderr)) = common::kotlinc_compile(&kotlinc_args) else {
        eprintln!("skipping cross-file diagnostics parity: kotlinc server unavailable");
        return;
    };
    let kotlinc_message = first_error(&kotlinc_stderr);
    let _ = fs::remove_dir_all(&root);

    assert_eq!(krusty_message, kotlinc_message);
}

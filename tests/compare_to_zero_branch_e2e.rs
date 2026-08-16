//! Comparisons against the literal `0`, which the backend emits with the single-operand `iflt`/`ifge`/
//! `ifle`/`ifgt`/`ifeq`/`ifne` branches (`cmp0_branch`) rather than pushing a `0` and using `if_icmp*`.
//! The corpus exercises a couple; this walks all six relations with a runtime operand (a parameter, so
//! the comparison isn't const-folded) in both true and false outcomes.
//!
//! A BOOLEAN comparand belongs to the same family: `!b` lowers to `b == false`, and `false` IS the int
//! `0` in the JVM's int category. That path recognized only the INT literal, so every `!` in a program
//! emitted `iconst_0; if_icmpne` where kotlinc emits `ifne` — one extra instruction, and every branch
//! offset after it shifted.

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn all_relations_against_zero() {
    run_ok(
        "Cmp0",
        "fun bits(n: Int): Int {\n\
         var r = 0\n\
         if (n < 0) r += 1\n\
         if (n > 0) r += 2\n\
         if (n <= 0) r += 4\n\
         if (n >= 0) r += 8\n\
         if (n == 0) r += 16\n\
         if (n != 0) r += 32\n\
         return r\n\
         }\n\
         fun box(): String {\n\
         if (bits(-5) != 1 + 4 + 32) return \"neg=${bits(-5)}\"\n\
         if (bits(5) != 2 + 8 + 32) return \"pos=${bits(5)}\"\n\
         if (bits(0) != 4 + 8 + 16) return \"zero=${bits(0)}\"\n\
         return \"OK\"\n\
         }\n",
    );
}

use std::fs;

/// One method's `javap -c` body. Constant-pool indices and branch offsets are normalized away (the
/// offsets shift with the very instruction under test); every other operand is kept.
fn method_body(dir: &std::path::Path, class: &str, method: &str) -> Vec<String> {
    let path = dir.join(format!("{class}.class"));
    let raw = common::javap(&["-c", "-p", &path.to_string_lossy()]).expect("pooled javap");
    let mut out = Vec::new();
    let mut inside = false;
    for line in raw.lines() {
        if line.contains(&format!(" {method}(")) && line.trim_end().ends_with(");") {
            inside = true;
            continue;
        }
        if inside {
            if line.trim().is_empty() || line == "}" {
                break;
            }
            let trimmed = line.trim();
            if trimmed == "Code:" {
                continue;
            }
            let mut chars = trimmed.chars().peekable();
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
            if chars.peek() == Some(&':') {
                chars.next();
            }
            let mut normalized = String::new();
            while let Some(c) = chars.next() {
                normalized.push(c);
                if c == '#' {
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                }
            }
            let mut tokens = normalized.split_whitespace();
            let normalized = match tokens.next() {
                Some(mnemonic) if mnemonic.starts_with("if") || mnemonic == "goto" => {
                    mnemonic.to_string()
                }
                Some(mnemonic) => std::iter::once(mnemonic)
                    .chain(tokens)
                    .collect::<Vec<_>>()
                    .join(" "),
                None => String::new(),
            };
            out.push(normalized);
        }
    }
    assert!(!out.is_empty(), "no body found for {class}.{method}");
    out
}

/// Compile `src` with BOTH compilers; `None` when the provisioned toolchain is unavailable.
fn compile_both(src: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let base = std::env::temp_dir().join(format!("krusty_cmp_zero_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let krusty_dir = base.join("krusty");
    let kotlinc_dir = base.join("kotlinc");
    fs::create_dir_all(&krusty_dir).ok()?;
    fs::create_dir_all(&kotlinc_dir).ok()?;

    let source = base.join("Zero.kt");
    fs::write(&source, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ])?;
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");

    let classes = common::compile_in_process(
        src,
        "Zero",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty failed to compile the fixture");
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(&path, bytes).ok()?;
    }
    Some((krusty_dir, kotlinc_dir))
}

const SRC: &str = r#"
fun notLocal(b: Boolean): Boolean = !b
fun notCall(s: Collection<String>, p: String): Boolean = !s.contains(p)
fun notBranch(b: Boolean): Int = if (!b) 1 else 2
fun intZero(n: Int): Boolean = n == 0
fun intNonZero(n: Int): Boolean = n != 0
fun box(): String {
    val ok = !false && notCall(listOf("a"), "b") && notBranch(true) == 2 && intZero(0)
    return if (ok) "OK" else "FAIL"
}
"#;

#[test]
fn a_boolean_tested_against_false_uses_the_single_operand_branch() {
    let Some((krusty_dir, kotlinc_dir)) = compile_both(SRC) else {
        return; // toolchain not provisioned
    };
    for method in ["notLocal", "notCall", "notBranch", "intZero", "intNonZero"] {
        assert_eq!(
            method_body(&krusty_dir, "ZeroKt", method),
            method_body(&kotlinc_dir, "ZeroKt", method),
            "{method}: the body must match kotlinc's"
        );
    }
    // Guard the comparison: the two-operand form must be gone, not merely equal on both sides.
    let body = method_body(&krusty_dir, "ZeroKt", "notLocal");
    assert!(
        body.iter().any(|line| line == "ifne"),
        "the negation must branch on the value itself: {body:?}"
    );
    // The trailing `iconst_0` is the FALSE result, not a compare operand; the two-operand form is
    // what must be gone.
    assert!(
        !body.iter().any(|line| line.starts_with("if_icmp")),
        "no two-operand comparison remains: {body:?}"
    );
}

/// The shorter shape must still compute the same answers.
#[test]
fn the_single_operand_branch_still_evaluates_correctly() {
    assert_eq!(
        common::compile_and_run_box(
            SRC,
            "Zero",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path())
        )
        .as_deref(),
        Some("OK")
    );
}

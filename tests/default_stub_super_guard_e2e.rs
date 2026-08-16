//! A member's `$default` synthetic opens with kotlinc's super-call guard.
//!
//! `super.m()` carrying defaults cannot be dispatched — the stub would re-enter the OVERRIDE through
//! `invokevirtual` — so kotlinc passes a non-null marker at such a call site and has the stub throw
//! `UnsupportedOperationException`. krusty emitted no guard, so every `$default` body started at the
//! mask tests and every branch offset after it diverged.
//!
//! Only a member whose owner can be inherited from gets it, which this pins in both directions.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and the
//! `$default` bodies are compared instruction for instruction.
use std::fs;

use super::common;

/// One method's `javap -c` body with constant-pool indices and bytecode offsets normalized away, so
/// the comparison is about the instruction SEQUENCE.
fn method_body(dir: &std::path::Path, class: &str, method: &str) -> Vec<String> {
    let path = dir.join(format!("{class}.class"));
    let raw = common::javap(&["-c", "-p", &path.to_string_lossy()]).expect("pooled javap");
    let mut out = Vec::new();
    let mut inside = false;
    for line in raw.lines() {
        if line.contains(method) && line.trim_end().ends_with(");") {
            inside = true;
            continue;
        }
        if inside {
            // The body ends at the blank line before the next member, or at the class's closing `}`
            // when this is the last one.
            if line.trim().is_empty() || line == "}" {
                break;
            }
            let trimmed = line.trim();
            if trimmed == "Code:" {
                continue;
            }
            let mut normalized = String::new();
            let mut chars = trimmed.chars().peekable();
            // Drop the leading `N:` offset.
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
            if chars.peek() == Some(&':') {
                chars.next();
            }
            while let Some(c) = chars.next() {
                normalized.push(c);
                if c == '#' {
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                }
            }
            // A BRANCH's operand is a bytecode offset, which shifts with the guard; drop it. Every
            // other numeric operand is a local slot or a constant, which must match kotlinc exactly.
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
    out
}

/// Compile `src` with BOTH compilers; `None` when the provisioned toolchain is unavailable.
fn compile_both(src: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let base = std::env::temp_dir().join(format!("krusty_def_guard_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let krusty_dir = base.join("krusty");
    let kotlinc_dir = base.join("kotlinc");
    fs::create_dir_all(&krusty_dir).ok()?;
    fs::create_dir_all(&kotlinc_dir).ok()?;

    let source = base.join("Defaults.kt");
    fs::write(&source, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ])?;
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");

    let classes = common::compile_in_process(
        src,
        "Defaults",
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

/// Every owner shape that decides the guard, plus a top-level function (a facade has no receiver to
/// `super`-call). `Sealed` and `Abstract` need a member with a body to have a `$default` at all.
const OWNERS: &str = r#"
open class Openly { open fun m(a: Int = 1, b: String = "x"): String = "$a$b" }
open class NotOpenMember { fun m(a: Int = 1): Int = a }
abstract class Abstractly { fun m(a: Int = 1): Int = a }
sealed class Sealedly { fun m(a: Int = 1): Int = a }
class Finally { fun m(a: Int = 1): Int = a }
data class Dataly(val x: Int) { fun m(a: Int = 1): Int = a }
object Objectly { fun m(a: Int = 1): Int = a }
// A wide parameter occupies two slots, so the marker's slot is NOT the parameter count.
open class Widely { open fun m(a: Long = 1L, b: Double = 2.0, c: String = "s"): String = "$a$b$c" }
fun top(a: Int = 1): Int = a
"#;

#[test]
fn an_inheritable_owners_default_stub_carries_the_super_guard() {
    let Some((krusty_dir, kotlinc_dir)) = compile_both(OWNERS) else {
        return; // toolchain not provisioned
    };
    for (class, method) in [
        ("Openly", "m$default"),
        ("NotOpenMember", "m$default"),
        ("Abstractly", "m$default"),
        ("Sealedly", "m$default"),
        ("Finally", "m$default"),
        ("Dataly", "m$default"),
        ("Objectly", "m$default"),
        ("Widely", "m$default"),
        ("DefaultsKt", "top$default"),
    ] {
        assert_eq!(
            method_body(&krusty_dir, class, method),
            method_body(&kotlinc_dir, class, method),
            "{class}.{method}: the stub body must match kotlinc's"
        );
    }
    // Guard the comparison in both directions: an inheritable owner throws, a final one does not.
    let guarded = method_body(&krusty_dir, "Widely", "m$default");
    assert!(
        guarded.first().is_some_and(|line| line == "aload 7"),
        "the guard must load the MARKER slot, past two wide parameters: {guarded:?}"
    );
    let guarded = method_body(&krusty_dir, "Openly", "m$default");
    assert!(
        guarded
            .iter()
            .any(|line| line.contains("UnsupportedOperationException")),
        "an open class's stub must guard: {guarded:?}"
    );
    for class in ["Finally", "Dataly", "Objectly"] {
        let body = method_body(&krusty_dir, class, "m$default");
        assert!(
            !body
                .iter()
                .any(|line| line.contains("UnsupportedOperationException")),
            "{class}: a final owner's stub must NOT guard: {body:?}"
        );
    }
}

/// The guarded stub must still WORK for an ordinary defaulted call — the marker is null there.
#[test]
fn a_guarded_stub_still_fills_defaults() {
    let src = r#"
open class Greeter { open fun greet(name: String = "world", n: Int = 2): String = "$name$n" }
fun box(): String {
    val g = Greeter()
    return if (g.greet() == "world2" && g.greet("hi") == "hi2" && g.greet(n = 9) == "world9") "OK"
           else "FAIL"
}
"#;
    assert_eq!(
        common::compile_and_run_box(
            src,
            "Defaults",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path())
        )
        .as_deref(),
        Some("OK")
    );
}

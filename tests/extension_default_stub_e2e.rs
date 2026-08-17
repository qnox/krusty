//! A top-level EXTENSION function with a NON-CONSTANT default value must still emit the
//! `<name>$default` facade stub (kotlinc's default-argument ABI). krusty used to suppress the stub
//! silently: registering the lowered defaults was gated on all-constant defaults — a CALL-SITE-reuse
//! concern — which also starved the stub emitter, so downstream modules (and Java callers) omitting
//! the argument hit `NoSuchMethodError`. Found byte-verifying intellij's icons-api `SwingIconKt`
//! (krusty 3 methods, kotlinc 5: `swingIcon$default`/`toSwingIcon$default` missing).
//!
//! The stub sections are compared against the provisioned reference kotlinc (fresh, same version —
//! no committed goldens); the same-module omitted-arg call keeps today's bail-don't-miscompile
//! behavior, pinned here so the stub-only registration can never leak into call-site filling.
use std::fs;

use super::common;

/// The verified repro plus regression anchors, one facade:
///  * `apply`/`scaleIt` — extension defaults referencing a companion object (`= M`) and a companion
///    `@JvmField` val (`= S.Default`), the two non-constant shapes from the icons-api gap;
///  * `tag` — an all-constant extension default (stub already emitted before the fix);
///  * `applyTop` — a PLAIN top-level fn with the same companion-ref default (ditto).
const FIXTURE: &str = "interface M { companion object : M }\n\
interface S { companion object { @JvmField val Default: S = object : S {} } }\n\
class D\n\
fun D.apply(m: M = M): Int = 1\n\
fun D.scaleIt(s: S = S.Default): Int = 2\n\
fun D.tag(a: Int = 1, b: String = \"z\"): String = \"\" + a + b\n\
fun applyTop(m: M = M): Int = 3\n";

/// Normalize `javap -c` output so semantically-equal bytecode compares equal: drop the source
/// banner, per-instruction offsets, and constant-pool index tokens (same rules as
/// `bytecode_parity_e2e`).
fn normalize(s: &str) -> String {
    let mut out = Vec::new();
    for raw in s.lines() {
        let line = raw.trim_end();
        if line.starts_with("Compiled from") || line.is_empty() {
            continue;
        }
        let t = line.trim_start();
        let body = match t.find(": ") {
            Some(p) if p > 0 && t[..p].chars().all(|c| c.is_ascii_digit()) => &t[p + 2..],
            _ => t,
        };
        let mut cleaned = String::new();
        let b = body.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'#' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                cleaned.push(b[i] as char);
                i += 1;
            }
        }
        let n = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        if !n.is_empty() {
            out.push(n);
        }
    }
    out.join("\n")
}

/// The `javap -c -p` method section starting at `marker` (the method-header line), up to the blank
/// line that ends it, normalized. Panics with the full disassembly when the marker is absent — a
/// MISSING stub is exactly the bug under test.
fn method_section(disasm: &str, marker: &str, side: &str) -> String {
    let s = disasm
        .find(marker)
        .unwrap_or_else(|| panic!("{side}: method {marker:?} not found in:\n{disasm}"));
    let rest = &disasm[s..];
    let end = rest[1..].find("\n\n").map(|p| p + 1).unwrap_or(rest.len());
    normalize(&rest[..end])
}

/// Compile [`FIXTURE`] once with krusty (in-process, the shipping pipeline) and once with the
/// reference kotlinc; return both facade disassemblies (`javap -c -p` of `SwingKt`). `None` when the
/// provisioned kotlinc/JDK toolchain is unavailable (tests then skip).
fn facade_disasms() -> Option<&'static (String, String)> {
    static CACHE: std::sync::OnceLock<Option<(String, String)>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let stdlib = common::stdlib_jar();
            let jdk = common::jdk_modules();
            let classes = common::compile_in_process(FIXTURE, "Swing", &[stdlib], Some(&jdk))
                .expect("krusty must compile the declaration-only fixture");
            let dir = std::env::temp_dir()
                .join(format!("krusty_ext_default_stub_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            let kref = dir.join("kref");
            let krout = dir.join("krout");
            fs::create_dir_all(&kref).unwrap();
            fs::create_dir_all(&krout).unwrap();
            for (internal, bytes) in &classes {
                let path = krout.join(format!("{internal}.class"));
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                fs::write(path, bytes).unwrap();
            }
            let src = dir.join("Swing.kt");
            fs::write(&src, FIXTURE).unwrap();
            let args = vec![
                src.to_string_lossy().into_owned(),
                "-d".to_string(),
                kref.to_string_lossy().into_owned(),
            ];
            let Some((code, stderr)) = common::kotlinc_compile(&args) else {
                eprintln!("skip (provisioned kotlinc server unavailable)");
                let _ = fs::remove_dir_all(&dir);
                return None;
            };
            assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
            let kr = common::javap(&["-c", "-p", &krout.join("SwingKt.class").to_string_lossy()])
                .expect("pooled JavaRunner unavailable");
            let ko = common::javap(&["-c", "-p", &kref.join("SwingKt.class").to_string_lossy()])
                .expect("pooled JavaRunner unavailable");
            let _ = fs::remove_dir_all(&dir);
            Some((kr, ko))
        })
        .as_ref()
}

/// Assert krusty's `<marker>` stub section equals kotlinc's, normalized.
fn assert_stub_matches(marker: &str) {
    let Some((kr, ko)) = facade_disasms() else {
        return; // toolchain unavailable
    };
    assert_eq!(
        method_section(kr, marker, "krusty"),
        method_section(ko, marker, "kotlinc"),
        "{marker}: krusty stub must match kotlinc (fresh, same version)"
    );
}

/// The fix: a companion-object-reference default (`= M`) no longer suppresses the stub.
#[test]
fn companion_ref_default_extension_emits_stub_matching_kotlinc() {
    assert_stub_matches("apply$default");
}

/// The fix: a companion `@JvmField` property-read default (`= S.Default`) no longer suppresses the
/// stub. Structural (not byte-parity) assertions: krusty does not yet hoist an interface-companion
/// `@JvmField` val to a static on the interface, so the default fill reads
/// `S.Companion.getDefault()` where kotlinc reads `getstatic S.Default` — a PRE-EXISTING property
/// realization divergence, identical for a plain top-level fn's stub, orthogonal to the stub gap
/// fixed here. Tighten this to `assert_stub_matches` once that realization lands.
#[test]
fn companion_field_default_extension_emits_stub() {
    let Some((kr, _)) = facade_disasms() else {
        return; // toolchain unavailable
    };
    let section = method_section(kr, "scaleIt$default", "krusty");
    // The slice starts AT the marker, so the header's access flags precede it in the raw line.
    assert!(
        section.starts_with("scaleIt$default(D, S, int, java.lang.Object);"),
        "stub header/descriptor:\n{section}"
    );
    for expected in [
        "iconst_1\niand\nifeq",                     // the logical mask bit test
        "invokestatic // Method scaleIt:(LD;LS;)I", // delegation to the real extension
        "getstatic",                                // the companion-backed default fill
    ] {
        assert!(
            section.contains(expected),
            "stub must contain {expected:?}:\n{section}"
        );
    }
}

/// Regression: an all-constant extension default emitted the stub before the fix — unchanged.
#[test]
fn constant_default_extension_stub_unchanged() {
    assert_stub_matches("tag$default");
}

/// Regression: a PLAIN top-level function with the same companion-ref default emitted the stub
/// before the fix — unchanged.
#[test]
fn plain_top_level_companion_ref_default_stub_unchanged() {
    assert_stub_matches("applyTop$default");
}

/// A SAME-MODULE call that omits the non-constant-defaulted argument keeps today's behavior: the
/// file bails (skip, never miscompile) — krusty deliberately fills same-module omitted defaults at
/// the call site from checker-recorded constants and does not route module calls through the stub.
/// Registering the defaults for the stub must not change that, so the stub-only marker is proven
/// effective here.
#[test]
fn same_module_omitted_argument_call_still_bails() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let src = format!("{FIXTURE}fun probe(d: D): Int = d.scaleIt()\n");
    // `None` here means FRONT-END diagnostics, not a missing toolchain (stdlib_jar/jdk_modules
    // above already panic on that) — a checker rejection of the fixture must fail loudly, or this
    // test passes vacuously exactly when the shape regresses.
    let outcome = common::backend_outcome_in_process(&src, "Probe", &[stdlib], Some(jdk.as_path()))
        .expect("the checker rejected the fixture; the bail under test is a BACKEND decline");
    match outcome {
        common::BackendOutcome::LowerBail(reason) => {
            // The bail comes from the omitted-arg CALL lowering declining (the checker records no
            // constant default value for `= S.Default`), not from the declarations themselves —
            // the declaration-only fixture compiles (see the stub tests).
            assert_eq!(
                reason, "qualified call has no supported semantic lowering",
                "the same-module omitted-arg call must keep its pre-fix bail"
            );
        }
        other => panic!("expected the same-module omitted-arg call to bail, got {other:?}"),
    }
}

/// End-to-end over the module boundary: a DOWNSTREAM krusty module omitting the argument resolves
/// the classpath extension's `$default` stub and observes the declared default values at runtime —
/// the exact icons-api consumer shape that used to throw `NoSuchMethodError`.
#[test]
fn cross_module_omitted_argument_call_routes_through_the_stub() {
    common::Fixture::new()
        .lib(
            "Lib.kt",
            "package iconlib\n\
             interface M { fun tag(): String\n\
             \x20 companion object : M { override fun tag(): String = \"companion\" } }\n\
             interface S { fun scale(): Int\n\
             \x20 companion object { @JvmField val Default: S = object : S { override fun scale(): Int = 42 } } }\n\
             class D\n\
             fun D.decorate(m: M = M): String = m.tag()\n\
             fun D.scaleIt(s: S = S.Default): Int = s.scale()\n",
        )
        .assert_box_ok(
            "import iconlib.*\n\
             fun box(): String {\n\
             \x20 val d = D()\n\
             \x20 if (d.decorate() != \"companion\") return \"decorate \" + d.decorate()\n\
             \x20 if (d.scaleIt() != 42) return \"scale \" + d.scaleIt()\n\
             \x20 return \"OK\"\n\
             }\n",
        );
}

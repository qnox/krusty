//! `-Xconsistent-data-class-copy-visibility` (language feature
//! `DataClassCopyRespectsConstructorVisibility`): a data class's synthesized `copy`/`copy$default`
//! take the PRIMARY CONSTRUCTOR's visibility instead of being unconditionally public.
//!
//! krusty compiles in-process (`compile_in_process_metadata_cp`, `// LANGUAGE:` directive); the
//! reference kotlinc gets the real CLI flag. FULL byte-identity for a private-ctor data class is
//! blocked by a pre-existing, feature-independent gap — krusty emits a declared-private primary
//! constructor as ACC_PUBLIC and synthesizes no `(…Lkotlin/jvm/internal/DefaultConstructorMarker;)V`
//! accessor ctor, so the whole-file comparison fails WITHOUT the flag too. These tests therefore
//! compare the feature's exact surface against kotlinc: the normalized `javap -v` sections of
//! `copy`/`copy$default` (flags, body dispatch, annotations, debug tables) and the decoded
//! `@Metadata` function visibility. The per-class `kotlin.ConsistentCopyVisibility` /
//! `kotlin.ExposedCopyVisibility` annotation overrides are unhandled (no user in the target project).

use super::common;
use krusty::jvm::classreader::parse_class;
use krusty::jvm::metadata::class_functions;

/// krusty's emitted bytes for `class_internal`, compiled in-process with class metadata on. A `None`
/// from the compile conflates "unavailable" with "rejected", so it panics with diagnostics instead.
fn krusty_bytes(src: &str, stem: &str, class_internal: &str) -> Vec<u8> {
    let classes = common::compile_in_process_metadata_cp(src, stem, &[]).unwrap_or_else(|| {
        let diagnostics = common::front_end_diagnostics(src, &[], None);
        panic!("{class_internal}: krusty declined the source; diagnostics: {diagnostics:?}")
    });
    classes
        .into_iter()
        .find(|(n, _)| n == class_internal)
        .map(|(_, b)| b)
        .unwrap_or_else(|| panic!("{class_internal} was not emitted"))
}

/// kotlinc's reference bytes for `class_internal`, compiled with `extra_args` (server-backed).
/// `None` ⇒ toolchain unavailable (the caller skips).
fn kotlinc_bytes(
    src: &str,
    stem: &str,
    class_internal: &str,
    extra_args: &[&str],
) -> Option<Vec<u8>> {
    common::java_home();
    let dir = common::scratch_dir()?;
    let out = dir.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let kt = dir.join(format!("{stem}.kt"));
    std::fs::write(&kt, src).ok()?;
    let mut args = vec![kt.to_string_lossy().into_owned()];
    args.extend(extra_args.iter().map(|a| (*a).to_string()));
    args.extend(["-d".to_string(), out.to_string_lossy().into_owned()]);
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    // kotlinc compiled, so past this point the only skip signal is gone: a missing class file is a
    // wrong `class_internal`, and degrading it to a skip would silently pass the test.
    let class_file = out.join(format!("{class_internal}.class"));
    let bytes = std::fs::read(&class_file).unwrap_or_else(|error| {
        panic!(
            "kotlinc succeeded but {} is unreadable ({error}) — wrong class_internal?",
            class_file.display()
        )
    });
    let _ = std::fs::remove_dir_all(&dir);
    Some(bytes)
}

/// `javap -v -p` of raw class bytes (via a scratch file and the pooled JavaRunner). The dir takes a
/// process-wide sequence number: tests in this module run concurrently in one process, so a
/// pid-plus-tag name alone would let them clobber each other's class file mid-disassembly.
fn disassemble(tag: &str, bytes: &[u8]) -> String {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("krusty_ccv_{tag}_{}_{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("C.class");
    std::fs::write(&path, bytes).unwrap();
    let d = common::javap(&["-v", "-p", &path.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_dir_all(&dir);
    d
}

/// Drop constant-pool index tokens (`#12`) and collapse whitespace runs (javap pads the comment
/// column to the operand width, so equal code can align differently) — sections then compare on
/// their semantic content: flags, mnemonics, resolved names, annotations, debug tables.
fn normalize(s: &str) -> String {
    let mut out = String::new();
    for line in s.lines() {
        let mut cleaned = String::new();
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'#' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                cleaned.push(b[i] as char);
                i += 1;
            }
        }
        out.push_str(&cleaned.split_whitespace().collect::<Vec<_>>().join(" "));
        out.push('\n');
    }
    out
}

/// The `copy` and `copy$default` method sections of a `javap -v -p` disassembly, normalized.
/// (javap separates member entries with blank lines and puts the constant pool in one unbroken
/// block, so paragraph-splitting isolates exactly the two method entries.)
fn copy_sections(disasm: &str) -> String {
    let sections: Vec<String> = disasm
        .split("\n\n")
        .filter(|para| {
            let first = para.lines().next().unwrap_or("").trim();
            first.ends_with(';') && (first.contains(" copy(") || first.contains(" copy$default("))
        })
        .map(normalize)
        .collect();
    assert_eq!(
        sections.len(),
        2,
        "expected exactly the `copy` and `copy$default` sections, got {sections:#?}"
    );
    sections.join("\n\n")
}

/// Assert the `copy`/`copy$default` sections of krusty's `class_internal` match kotlinc's, with the
/// same source but kotlinc invoked with `kotlinc_extra_args`. Skips only when kotlinc is unavailable.
fn assert_copy_sections_match(
    src: &str,
    stem: &str,
    class_internal: &str,
    kotlinc_extra_args: &[&str],
) {
    let Some(ko) = kotlinc_bytes(src, stem, class_internal, kotlinc_extra_args) else {
        eprintln!("skip ({class_internal}: provisioned kotlinc unavailable)");
        return;
    };
    let kr = krusty_bytes(src, stem, class_internal);
    let kr_sections = copy_sections(&disassemble("kr", &kr));
    let ko_sections = copy_sections(&disassemble("ko", &ko));
    assert_eq!(
        kr_sections, ko_sections,
        "{class_internal}: krusty's copy/copy$default must match kotlinc's"
    );
}

/// The private-primary-ctor data class of the ground-truth probe. The `// LANGUAGE:` directive is a
/// comment to kotlinc (which takes the CLI flag instead), so BOTH compilers see identical source —
/// line numbers and debug tables stay comparable. The REFERENCE-typed `s` matters: it is what makes
/// the copy section carry `checkNotNullParameter` entry guards and parameter `@NotNull`s in the
/// public shape, both of which a private `copy` must DROP (an `Int`-only fixture exercises neither).
const PRIVATE_CTOR_SRC: &str = "\
// LANGUAGE: +DataClassCopyRespectsConstructorVisibility
package demo

data class D private constructor(val s: String, val n: Int) {
    companion object {
        fun make(s: String, n: Int) = D(s, n)
    }
}
";

/// [`PRIVATE_CTOR_SRC`] with the feature explicitly DISABLED — the same line count, so the two
/// variants' debug tables (and thus every feature-untouched class byte) are directly comparable.
const PRIVATE_CTOR_SRC_NO_FEATURE: &str = "\
// LANGUAGE: -DataClassCopyRespectsConstructorVisibility
package demo

data class D private constructor(val s: String, val n: Int) {
    companion object {
        fun make(s: String, n: Int) = D(s, n)
    }
}
";

/// With the feature, `copy` takes the private ctor's visibility. Ground truth (kotlinc 2.4.10, this
/// exact source): `copy` ACC_PRIVATE|ACC_FINAL, NO `@NotNull` annotations, and NO
/// `Intrinsics.checkNotNullParameter` entry guards (its body starts directly at `new`);
/// `copy$default` package-private ACC_STATIC|ACC_SYNTHETIC dispatching via `invokespecial`. The
/// normalized javap sections pin all of it against the reference output.
#[test]
fn private_ctor_copy_sections_match_kotlinc_with_the_flag() {
    assert_copy_sections_match(
        PRIVATE_CTOR_SRC,
        "D",
        "demo/D",
        &["-Xconsistent-data-class-copy-visibility"],
    );
}

/// WITHOUT the feature, a private-ctor data class keeps kotlinc's default shape — a public
/// `@NotNull`-annotated `copy` and a public `copy$default` dispatching via `invokevirtual`.
#[test]
fn private_ctor_copy_sections_match_kotlinc_without_the_flag() {
    assert_copy_sections_match(PRIVATE_CTOR_SRC_NO_FEATURE, "D", "demo/D", &[]);
}

/// The feature's whole JVM surface is the data class's own `copy`/`copy$default`: every OTHER class
/// of the compilation (here the companion, which reaches the private ctor) must be byte-for-byte
/// unchanged by toggling it.
#[test]
fn the_flag_leaves_the_companion_bytes_untouched() {
    let with = krusty_bytes(PRIVATE_CTOR_SRC, "D", "demo/D$Companion");
    let without = krusty_bytes(PRIVATE_CTOR_SRC_NO_FEATURE, "D", "demo/D$Companion");
    assert_eq!(
        with, without,
        "the companion must not change under the flag"
    );
    let d_with = krusty_bytes(PRIVATE_CTOR_SRC, "D", "demo/D");
    let d_without = krusty_bytes(PRIVATE_CTOR_SRC_NO_FEATURE, "D", "demo/D");
    assert_ne!(d_with, d_without, "the data class itself must change");
}

/// With the feature and a PRIVATE ctor, krusty's own emitted surface carries the exact deltas
/// (access flags + `@Metadata` visibility), independent of the kotlinc toolchain. The `@Metadata`
/// function-flags word was cross-checked byte-equal against kotlinc 2.4.10's d1 (0xC6 → 0xC2).
#[test]
fn private_ctor_copy_deltas_hold_in_krustys_own_output() {
    let bytes = krusty_bytes(PRIVATE_CTOR_SRC, "D", "demo/D");
    let ci = parse_class(&bytes).expect("krusty's class parses back");
    let copy = ci
        .methods
        .iter()
        .find(|m| m.name == "copy")
        .expect("`copy` must be emitted");
    assert_eq!(copy.access, 0x0012, "`copy` must be ACC_PRIVATE|ACC_FINAL");
    let copy_default = ci
        .methods
        .iter()
        .find(|m| m.name == "copy$default")
        .expect("`copy$default` must be emitted");
    assert_eq!(
        copy_default.access, 0x1008,
        "`copy$default` must be package-private ACC_STATIC|ACC_SYNTHETIC"
    );
    let meta_copy = class_functions(&ci)
        .iter()
        .find(|f| f.kotlin_name == "copy")
        .expect("@Metadata must list `copy`");
    assert_eq!(
        meta_copy.visibility,
        krusty::types::Visibility::Private,
        "@Metadata must record the copy Function as private"
    );
}

/// An INTERNAL primary ctor with the feature: kotlinc mangles the JVM methods (`copy$<module>`)
/// while keeping them public, and records internal visibility in `@Metadata`. krusty does not
/// mangle internal member functions AT ALL yet (a declared `internal fun` also emits under its
/// plain name), so `copy` follows that systemic convention — public, unmangled JVM — while
/// `@Metadata` carries the internal visibility that actually enforces the module boundary.
/// Byte-parity with kotlinc's mangled form is deferred until internal mangling lands module-wide.
#[test]
fn internal_ctor_copy_is_public_unmangled_with_internal_metadata() {
    let src = "\
// LANGUAGE: +DataClassCopyRespectsConstructorVisibility
package demo

data class E internal constructor(val x: Int)
";
    let bytes = krusty_bytes(src, "E", "demo/E");
    let ci = parse_class(&bytes).expect("krusty's class parses back");
    let copy = ci
        .methods
        .iter()
        .find(|m| m.name == "copy")
        .expect("`copy` must keep its unmangled JVM name");
    assert_eq!(
        copy.access, 0x0011,
        "internal-ctor `copy` stays ACC_PUBLIC|ACC_FINAL (krusty's internal-member convention)"
    );
    let copy_default = ci
        .methods
        .iter()
        .find(|m| m.name == "copy$default")
        .expect("`copy$default` must keep its unmangled JVM name");
    assert_eq!(
        copy_default.access, 0x1009,
        "internal-ctor `copy$default` stays ACC_PUBLIC|ACC_STATIC|ACC_SYNTHETIC"
    );
    let meta_copy = class_functions(&ci)
        .iter()
        .find(|f| f.kotlin_name == "copy")
        .expect("@Metadata must list `copy`");
    assert_eq!(
        meta_copy.visibility,
        krusty::types::Visibility::Internal,
        "@Metadata must record the copy Function as internal"
    );
}

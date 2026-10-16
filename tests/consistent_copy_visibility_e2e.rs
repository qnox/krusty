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
//! `kotlin.ExposedCopyVisibility` annotations select the same two shapes per class, and both the
//! annotations' misuse and an out-of-scope `copy` call are diagnosed exactly as kotlinc does.

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

/// An INTERNAL primary ctor with the feature: the JVM methods are public `copy$<module>` and
/// `copy$<module>$default` (the default module is `main`), and `@Metadata` records internal
/// visibility under the Kotlin name `copy`.
#[test]
fn internal_ctor_copy_is_public_and_module_mangled() {
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
        .find(|m| m.name == "copy$main")
        .expect("`copy` is public `copy$main`");
    assert_eq!(
        copy.access, 0x0011,
        "internal-ctor `copy$main` stays ACC_PUBLIC|ACC_FINAL"
    );
    let copy_default = ci
        .methods
        .iter()
        .find(|m| m.name == "copy$main$default")
        .expect("`copy$main$default` is the default-argument stub");
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

/// Compile `src` with both compilers (kotlinc gets `kotlinc_args`; krusty reads the source's
/// `// LANGUAGE:` directive) and assert they reject it with the identical error ledger.
fn assert_rejected_like_kotlinc(src: &str, kotlinc_args: &[&str], tag: &str) {
    let args = kotlinc_args
        .iter()
        .map(|argument| (*argument).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_reference_args(&[("Main.kt", src)], &[], &args);
    common::expect_identical_rejection(&result, tag);
}

/// The annotations' own checks: both on one data class are incompatible (reported on each entry),
/// and on anything but a data class the `@ConsistentCopyVisibility` entry is the wrong target
/// (kotlinc names it even when written second, and reads a qualified spelling by identity). A
/// `data object` is a data class to the check, so it is accepted.
#[test]
fn annotation_misuse_matches_kotlinc() {
    let src = "\
package demo

@ConsistentCopyVisibility @ExposedCopyVisibility data class D private constructor(val x: Int)
@ConsistentCopyVisibility class E(val x: Int)
@ExposedCopyVisibility class F(val x: Int)
@ExposedCopyVisibility @ConsistentCopyVisibility class G(val x: Int)
@kotlin.ConsistentCopyVisibility interface I
@ConsistentCopyVisibility data object O
";
    assert_rejected_like_kotlinc(src, &[], "annotation misuse");
}

/// Without the flag, `@ConsistentCopyVisibility` alone makes `copy` follow the constructor: private
/// and protected copies are rejected outside the class, an internal one (here spelled through a type
/// alias) is reachable in the module, and an unannotated private-ctor class keeps a public `copy`.
/// Inside the class, and from a nested class's own body, a private `copy` stays callable.
#[test]
fn annotated_copy_access_matches_kotlinc_without_the_flag() {
    let src = "\
package demo

typealias Consistent = ConsistentCopyVisibility

@ConsistentCopyVisibility data class A private constructor(val x: Int) {
    fun bump() = copy(x = x + 1)
    companion object { fun make() = A(1) }
}
@ConsistentCopyVisibility data class Q protected constructor(val x: Int) {
    companion object { fun make() = Q(1) }
}
@Consistent data class P internal constructor(val x: Int)
data class C private constructor(val x: Int) {
    companion object { fun make() = C(1) }
}
class Outer {
    @ConsistentCopyVisibility data class N private constructor(val x: Int) {
        fun again() = copy(x = 3)
    }
    fun f(n: N) = n.copy()
}

fun use(p: P) {
    A.make().copy(x = 2)
    Q.make().copy()
    p.copy(x = 2)
    C.make().copy(x = 2)
}
";
    assert_rejected_like_kotlinc(src, &[], "annotated copy access");
}

/// Under the flag every non-public-ctor `copy` is restricted unless the class says
/// `@ExposedCopyVisibility`, which keeps it public.
#[test]
fn flagged_copy_access_matches_kotlinc() {
    let src = "\
// LANGUAGE: +DataClassCopyRespectsConstructorVisibility
package demo

data class C private constructor(val x: Int) {
    companion object { fun make() = C(1) }
}
@ExposedCopyVisibility data class X private constructor(val x: Int) {
    companion object { fun make() = X(1) }
}

fun use() {
    C.make().copy(x = 2)
    X.make().copy(x = 2)
}
";
    assert_rejected_like_kotlinc(
        src,
        &["-Xconsistent-data-class-copy-visibility"],
        "flagged copy access",
    );
}

/// Under the flag `@ConsistentCopyVisibility` restates the default: kotlinc warns that it is
/// redundant on every data class, public constructor and `data object` included.
#[test]
fn redundant_consistent_annotation_warning_matches_kotlinc() {
    let src = "\
// LANGUAGE: +DataClassCopyRespectsConstructorVisibility
package demo

@ConsistentCopyVisibility data class A private constructor(val x: Int)
@ConsistentCopyVisibility data class B(val x: Int)
@ConsistentCopyVisibility data object O
@ExposedCopyVisibility data class X private constructor(val x: Int)
";
    let args = ["-Xconsistent-data-class-copy-visibility".to_string()];
    let result = common::compiler_diagnostics_with_reference_args(&[("Main.kt", src)], &[], &args);
    assert_eq!(
        result.reference_code, 0,
        "kotlinc: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    let reference = common::compiler_warnings(&result.reference_stderr);
    assert_eq!(reference.len(), 3, "kotlinc: {}", result.reference_stderr);
    assert_eq!(common::compiler_warnings(&result.krusty_stderr), reference);
}

/// `@ConsistentCopyVisibility` without the flag gives the exact flagged shape: a private, unguarded,
/// un-annotated `copy` and a package-private `copy$default` dispatching via `invokespecial`.
#[test]
fn consistent_annotation_copy_sections_match_kotlinc() {
    let src = "\
package demo

@ConsistentCopyVisibility
data class D private constructor(val s: String, val n: Int) {
    companion object {
        fun make(s: String, n: Int) = D(s, n)
    }
}
";
    assert_copy_sections_match(src, "D", "demo/D", &[]);
    let bytes = krusty_bytes(src, "D", "demo/D");
    let ci = parse_class(&bytes).expect("krusty's class parses back");
    let meta_copy = class_functions(&ci)
        .iter()
        .find(|function| function.kotlin_name == "copy")
        .expect("@Metadata must list `copy`");
    assert_eq!(meta_copy.visibility, krusty::types::Visibility::Private);
}

/// `@ExposedCopyVisibility` under the flag keeps kotlinc's public shape.
#[test]
fn exposed_annotation_copy_sections_match_kotlinc_with_the_flag() {
    let src = "\
// LANGUAGE: +DataClassCopyRespectsConstructorVisibility
package demo

@ExposedCopyVisibility
data class D private constructor(val s: String, val n: Int) {
    companion object {
        fun make(s: String, n: Int) = D(s, n)
    }
}
";
    assert_copy_sections_match(
        src,
        "D",
        "demo/D",
        &["-Xconsistent-data-class-copy-visibility"],
    );
}

/// A PROTECTED primary ctor under `@ConsistentCopyVisibility`: kotlinc emits a protected `copy`
/// beside a public `copy$default`.
#[test]
fn protected_ctor_copy_sections_match_kotlinc() {
    let src = "\
package demo

@ConsistentCopyVisibility
data class Q protected constructor(val s: String, val n: Int)
";
    assert_copy_sections_match(src, "Q", "demo/Q", &[]);
}

//! `@JvmField` on companion-object (and named-object) properties: kotlinc realizes the property as
//! a PUBLIC static field on the OWNER class — `final` for a `val`, non-final for a `var` — with NO
//! getter/setter anywhere and NO `access$…$cp` bridges. The owner's `<clinit>` initializes it after
//! the `Companion` instance store, in declaration order. On an INTERFACE owner the field is hoisted
//! onto the interface itself (`public static final`, the only field shape an interface admits),
//! which kotlinc permits only when every companion property is a `public final val` with
//! `@JvmField`. The companion's `@Metadata` property record keeps the `JvmField` annotation and
//! drops the accessor signatures. (All measured against reference kotlinc 2.4.10.)
use super::common;

/// krusty's emitted bytes for `class_internal`, compiled in-process with class metadata on. A `None`
/// from the compile helper conflates "toolchain unavailable" with "krusty REJECTED the source", so
/// it is not a skip signal: this panics with the front-end diagnostics instead of reporting a
/// declined source as a pass.
fn krusty_bytes(src: &str, stem: &str, class_internal: &str) -> Vec<u8> {
    // The kotlin stdlib rides the classpath so `@JvmField` resolves exactly as it does for the CLI
    // (kotlinc implicitly has its own stdlib, so this changes nothing on the reference side).
    let cp = [common::stdlib_jar()];
    let classes = common::compile_in_process_metadata_cp(src, stem, &cp).unwrap_or_else(|| {
        let diagnostics = common::front_end_diagnostics(src, &cp, None);
        panic!("{class_internal}: krusty declined the source; diagnostics: {diagnostics:?}")
    });
    classes
        .into_iter()
        .find(|(n, _)| n == class_internal)
        .map(|(_, b)| b)
        .unwrap_or_else(|| panic!("{class_internal} was not emitted"))
}

/// kotlinc's reference bytes for `class_internal` (server-backed). `None` ⇒ toolchain unavailable.
fn kotlinc_bytes(src: &str, stem: &str, class_internal: &str) -> Option<Vec<u8>> {
    common::java_home();
    let dir = common::scratch_dir()?;
    let out = dir.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let kt = dir.join(format!("{stem}.kt"));
    std::fs::write(&kt, src).ok()?;
    let args = vec![
        kt.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let bytes = std::fs::read(out.join(format!("{class_internal}.class"))).ok();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

/// Assert krusty's in-process output for `class_internal` is byte-for-byte identical to kotlinc's.
/// Skips only when the reference kotlinc toolchain is unavailable; a source krusty declines FAILS.
fn assert_byte_identical(src: &str, class_internal: &str) {
    let stem = class_internal
        .rsplit('/')
        .next()
        .unwrap()
        .split('$')
        .next()
        .unwrap();
    let Some(ko) = kotlinc_bytes(src, stem, class_internal) else {
        eprintln!("skip ({class_internal}: provisioned kotlinc unavailable)");
        return;
    };
    let kr = krusty_bytes(src, stem, class_internal);
    assert_eq!(
        kr,
        ko,
        "{class_internal} must be byte-for-byte identical to kotlinc (krusty {} B, kotlinc {} B)",
        kr.len(),
        ko.len(),
    );
}

// ---- Byte identity: class owner ------------------------------------------------------------------

/// The everyday shape: `@JvmField val` + `@JvmField var` in a class companion. The owner carries
/// `public static final String Named` / `public static int Counter` (no getter, no `access$…$cp`
/// bridges), each initialized in `<clinit>` after the `Companion` store, in declaration order; the
/// field's `RuntimeInvisibleAnnotations` carry `JvmField` before the nullability annotation.
const CLASS_OWNER: &str = "package demo\nclass C {\n    companion object {\n        @JvmField val Named: String = \"x\"\n        @JvmField var Counter: Int = 7\n    }\n}\n";

#[test]
fn jvmfield_companion_owner_class_is_byte_identical() {
    assert_byte_identical(CLASS_OWNER, "demo/C");
}

/// The companion class keeps NO accessors and no fields; its `@Metadata` property records keep the
/// `JvmField` annotation and drop the accessor signatures.
#[test]
fn jvmfield_companion_class_is_byte_identical() {
    assert_byte_identical(CLASS_OWNER, "demo/C$Companion");
}

/// An `internal` companion `@JvmField val` still hoists as a PUBLIC field (kotlinc does not mangle
/// or restrict the field).
#[test]
fn jvmfield_internal_companion_val_owner_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass D {\n    companion object {\n        @JvmField internal val I2: Int = 2\n    }\n}\n",
        "demo/D",
    );
}

// ---- Byte identity: interface owner --------------------------------------------------------------

/// kotlinc hoists an interface companion's `@JvmField val` onto the INTERFACE as `public static
/// final`, initialized in the interface `<clinit>` after the `Companion` alias store (`getstatic
/// I$Companion.$$INSTANCE; putstatic Companion; …; putstatic Default`).
const IFACE_OWNER: &str = "package demo\ninterface I {\n    fun f(): Int\n    companion object {\n        @JvmField val Default: Int = 42\n    }\n}\n";

#[test]
fn jvmfield_interface_companion_owner_is_byte_identical() {
    assert_byte_identical(IFACE_OWNER, "demo/I");
}

/// The interface companion keeps its `$$INSTANCE` self-construction and nothing else — no fields,
/// no accessors.
#[test]
fn jvmfield_interface_companion_class_is_byte_identical() {
    assert_byte_identical(IFACE_OWNER, "demo/I$Companion");
}

// ---- Runtime: same-module reads ------------------------------------------------------------------

/// Same-file reads (and a write through the companion `var`) resolve against the hoisted public
/// field — `getstatic`/`putstatic C.Name` directly, since no accessor exists to call.
#[test]
fn jvmfield_companion_reads_same_file() {
    let src = r#"
class C {
    companion object {
        @JvmField val Named: String = "x"
        @JvmField var Counter: Int = 7
    }
}
fun box(): String {
    if (C.Named != "x") return "F:val"
    C.Counter = C.Counter + 1
    if (C.Counter != 8) return "F:var"
    return "OK"
}
"#;
    common::expect_box_ok_files_with_stdlib(&[("SameFile.kt", src)], "same-file @JvmField reads");
}

/// Cross-file (same module) reads of class- and interface-owner `@JvmField` properties.
#[test]
fn jvmfield_companion_reads_cross_file() {
    const DECL: &str = r#"
package lib
class C {
    companion object {
        @JvmField val Named: String = "x"
    }
}
interface I {
    fun f(): Int
    companion object {
        @JvmField val Default: Int = 42
    }
}
"#;
    const USE: &str = r#"
package lib
fun box(): String {
    if (C.Named != "x") return "F:class"
    if (I.Default != 42) return "F:iface"
    return "OK"
}
"#;
    common::expect_box_ok_files_with_stdlib(
        &[("Decl.kt", DECL), ("Use.kt", USE)],
        "cross-file @JvmField reads",
    );
}

// ---- Runtime: ineligible placements fall back TOGETHER -------------------------------------------

/// An interface companion mixing a `@JvmField val` with a `const val` fails kotlinc's
/// whole-companion rule, so NOTHING hoists — krusty accepts the source (kotlinc rejects it) and
/// must keep the ordinary accessor realization CONSISTENTLY: the pass that emits and the checker
/// that routes reads must consult the same property universe, or a read resolves to an accessor
/// that was never emitted (`NoSuchMethodError` with zero compile signal).
#[test]
fn jvmfield_mixed_interface_companion_falls_back_whole() {
    let src = r#"
interface I {
    fun f(): Int
    companion object {
        @JvmField val a: Int = 41
        const val B: Int = 2
    }
}
fun box(): String {
    if (I.a != 41) return "F:a"
    if (I.B != 2) return "F:B"
    return "OK"
}
"#;
    common::expect_box_ok_files_with_stdlib(
        &[("MixedIface.kt", src)],
        "mixed interface companion falls back whole",
    );
}

/// A VALUE-CLASS-typed `@JvmField` companion property: the JVM pass declines the hoist (the
/// backing field erases), so the checker must not route reads to a hoisted field that was never
/// emitted (`NoSuchFieldError`). Both sides fall back to the ordinary accessor realization.
#[test]
fn jvmfield_value_class_companion_falls_back() {
    let src = r#"
@JvmInline
value class V(val x: Int)
class C {
    companion object {
        @JvmField val a: V = V(41)
    }
}
fun box(): String = if (C.a.x == 41) "OK" else "F:" + C.a.x
"#;
    common::expect_box_ok_files_with_stdlib(
        &[("VcCompanion.kt", src)],
        "value-class @JvmField companion falls back",
    );
}

// ---- Runtime: write receiver side effects --------------------------------------------------------

/// `side().Counter = 7`: kotlinc EVALUATES the write receiver (then `pop; putstatic`) while a READ
/// receiver of the same shape is dropped entirely (measured: `sideW().Counter = 7; sideR().Counter`
/// prints only "w" — the direct `getstatic` has no call). The write lowering must keep the
/// receiver's effects in order; the read keeps dropping them.
#[test]
fn jvmfield_write_receiver_side_effect_runs() {
    let src = r#"
class C {
    companion object {
        @JvmField var Counter: Int = 0
    }
}
var log: String = ""
fun sideW(): C.Companion { log += "w"; return C.Companion }
fun sideR(): C.Companion { log += "r"; return C.Companion }
fun box(): String {
    sideW().Counter = 7
    if (log != "w") return "F:w-not-evaluated:" + log
    if (C.Counter != 7) return "F:store"
    val read = sideR().Counter
    if (read != 7) return "F:read"
    if (log != "w") return "F:r-evaluated:" + log
    return "OK"
}
"#;
    common::expect_box_ok_files_with_stdlib(
        &[("WriteRecv.kt", src)],
        "@JvmField write receiver side effect",
    );
}

// ---- Runtime: cross-module consumption -----------------------------------------------------------

/// A krusty-built library's `@JvmField` companion properties are consumed from a separate module via
/// the classpath (plain public fields — no accessor exists in the jar). The default-on lib
/// cross-check also runs the same main against a kotlinc-built lib.
#[test]
fn jvmfield_companion_reads_cross_module() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    const LIB: &str = "package lib\n\
        class C {\n\
            companion object {\n\
                @JvmField val Named: String = \"x\"\n\
                @JvmField var Counter: Int = 7\n\
            }\n\
        }\n\
        interface I {\n\
            fun f(): Int\n\
            companion object {\n\
                @JvmField val Default: Int = 42\n\
            }\n\
        }\n";
    let Some(lo) = common::compile_lib("jvmfield_companion", LIB) else {
        return;
    };
    const MAIN: &str = "import lib.C\n\
        import lib.I\n\
        fun box(): String {\n\
            if (C.Named != \"x\") return \"F:val\"\n\
            C.Counter = C.Counter + 1\n\
            if (C.Counter != 8) return \"F:var\"\n\
            if (I.Default != 42) return \"F:iface\"\n\
            return \"OK\"\n\
        }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[lo, sl, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"), "cross-module @JvmField reads");
}

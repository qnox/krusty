//! Bytecode-parity TDD: assert krusty emits the SAME JVM instruction shapes kotlinc does for the
//! patterns closed in phases 397–400. Two kinds of check:
//!   * shape assertions on krusty's own `javap -c` output (no kotlinc needed) — the regression guard;
//!   * a differential full-class normalized-equality check vs the provisioned real kotlinc.
//!
//! Run with `JAVA_HOME` set; kotlinc path overrides are optional.

use std::fs;
use std::process::Command;

use super::common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn java_home() -> Option<String> {
    env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME"))
}

/// Compile `src` with the krusty binary into a fresh dir; return the dir (or `None` if javap/JAVA_HOME
/// is unavailable — the test then skips).
fn krusty_compile(name: &str, src: &str) -> Option<(std::path::PathBuf, String)> {
    let jh = java_home()?;
    if !std::path::Path::new(&format!("{jh}/bin/javap")).exists() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("krusty_bcp_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // Compile in-process (no CLI spawn): these snippets need no classpath, exactly as the previous
    // `krusty -d dir B.kt` (no `-cp`). Write the class bytes to `dir` so `javap` can disassemble them.
    let classes = common::compile_in_process(src, "B", &[], None)
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    for (internal, bytes) in &classes {
        let path = dir.join(format!("{internal}.class"));
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).ok();
        }
        fs::write(&path, bytes).unwrap();
    }
    Some((dir, jh))
}

/// `javap -c -p` of one class file.
fn javap(jh: &str, class_file: &std::path::Path) -> String {
    let out = Command::new(format!("{jh}/bin/javap"))
        .args(["-c", "-p"])
        .arg(class_file)
        .output()
        .unwrap();
    assert!(out.status.success(), "javap failed on {class_file:?}");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Normalize `javap -c` output so semantically-equal bytecode compares equal: drop the source banner,
/// the per-instruction bytecode offset, and constant-pool index tokens.
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

/// The disassembly of the facade class (`BKt.class`).
fn facade_disasm(name: &str, src: &str) -> Option<String> {
    let (dir, jh) = krusty_compile(name, src)?;
    let cls = dir.join("BKt.class");
    let d = javap(&jh, &cls);
    let _ = fs::remove_dir_all(&dir);
    Some(d)
}

// ---- Phase 400: iinc + compare-to-zero -------------------------------------------------------

#[test]
fn counting_loop_uses_iinc_not_load_add_store() {
    let Some(d) = facade_disasm(
        "iinc",
        "fun box(): String {\n  var s = 0\n  for (i in 0 until 4) { s += i }\n  return \"OK\"\n}\n",
    ) else {
        return;
    };
    // The loop counter increment is `iinc`, never `iconst_1; iadd; istore` for the counter.
    assert!(
        d.contains("iinc"),
        "expected `iinc` for the loop counter:\n{d}"
    );
}

#[test]
fn compare_against_zero_is_single_operand_branch() {
    // `x != 0` → `ifeq`/`ifne` (compare-to-zero), NOT `iconst_0; if_icmp*`.
    let Some(d) = facade_disasm(
        "cmp0",
        "fun box(): String {\n  val x = 3\n  if (x != 0) return \"OK\"\n  return \"f\"\n}\n",
    ) else {
        return;
    };
    // No materialized zero for the comparison: the `if (x != 0)` test must not push iconst_0 then if_icmp.
    let n = normalize(&d);
    assert!(
        n.contains("ifne") || n.contains("ifeq"),
        "expected a single-operand compare-to-zero branch:\n{n}"
    );
    assert!(
        !n.contains("iconst_0\nif_icmpeq") && !n.contains("iconst_0\nif_icmpne"),
        "comparison against 0 must not materialize iconst_0 then if_icmp:\n{n}"
    );
}

// ---- Phase 399: dcmpl/fcmpl for > and >= -----------------------------------------------------

#[test]
fn double_greater_than_uses_dcmpl() {
    let Some(d) = facade_disasm(
        "dcmpl",
        "fun gt(a: Double, b: Double) = a > b\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    assert!(
        d.contains("dcmpl"),
        "`a > b` on Double must use dcmpl (NaN-correct, kotlinc's choice):\n{d}"
    );
}

// ---- Phase 397: comparison fusion ------------------------------------------------------------

#[test]
fn loop_condition_is_fused_if_icmp() {
    let Some(d) = facade_disasm(
        "fuse",
        "fun box(): String {\n  var s = 0\n  for (i in 0 until 10) { s += 1 }\n  return \"OK\"\n}\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    // The loop bound `i < 10` fuses to `if_icmpge` (exit), not a materialized boolean + ifeq.
    assert!(
        n.contains("if_icmpge"),
        "loop condition must fuse to if_icmpge:\n{n}"
    );
}

// ---- string templates: one StringBuilder + append(C) + String.valueOf -----------------------

#[test]
fn string_template_uses_single_stringbuilder_and_append_char() {
    let Some(d) = facade_disasm(
        "tmpl",
        "fun f(a: Int, b: String): String = \"x=$a y=$b!\"\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    // Exactly ONE StringBuilder is allocated for the whole template (not one per `+`).
    let sbs = d.matches("class java/lang/StringBuilder").count();
    assert_eq!(
        sbs, 1,
        "a string template must allocate ONE StringBuilder:\n{d}"
    );
    // The trailing single-char literal "!" appends as a char (append(C) with bipush 33).
    assert!(
        d.contains("StringBuilder.append:(C)"),
        "a single-char literal in a template must append as a char:\n{d}"
    );
}

#[test]
fn single_interpolation_uses_string_valueof() {
    let Some(d) = facade_disasm("valueof", "fun g(n: Int) = \"$n\"\nfun box() = \"OK\"\n") else {
        return;
    };
    assert!(
        d.contains("String.valueOf:(I)") && !d.contains("class java/lang/StringBuilder"),
        "a lone interpolation `\"$n\"` must be String.valueOf(I), no StringBuilder:\n{d}"
    );
}

// ---- data-class toString: one StringBuilder + merged prefix + append(C) ----------------------

#[test]
fn data_class_tostring_uses_single_stringbuilder() {
    // A data class's synthesized `toString` must build with ONE StringBuilder (kotlinc's shape), not a
    // chain of `String.plus` (one StringBuilder per `+`). The class-name + first field name merge into a
    // single `"P(x="` constant, and the closing `")"` single char appends as a char.
    let Some((dir, jh)) = krusty_compile(
        "dctostr",
        "data class P(val x: Int, val y: String)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let d = javap(&jh, &dir.join("P.class"));
    let _ = std::fs::remove_dir_all(&dir);
    let sbs = d.matches("class java/lang/StringBuilder").count();
    assert_eq!(
        sbs, 1,
        "data-class toString must allocate ONE StringBuilder:\n{d}"
    );
    assert!(
        d.contains("String P(x="),
        "the class name + first field should merge into one `P(x=` constant:\n{d}"
    );
    assert!(
        d.contains("StringBuilder.append:(C)"),
        "the closing `)` should append as a char:\n{d}"
    );
}

#[test]
fn data_class_member_order_matches_kotlin() {
    // kotlinc emits data-class members in the order: componentN, copy, copy$default, toString, hashCode,
    // equals. krusty must match (copy before toString), not append copy last.
    let Some((dir, jh)) = krusty_compile(
        "dcorder",
        "data class P(val x: Int, val y: String)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let out = Command::new(format!("{jh}/bin/javap"))
        .arg("-p")
        .arg(dir.join("P.class"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);
    let pos = |needle: &str| text.find(needle);
    let (c2, copy, ts) = (pos("component2"), pos(" copy("), pos("toString("));
    assert!(
        c2 < copy && copy < ts,
        "data-class member order must be componentN, copy, …, toString:\n{text}"
    );
}

#[test]
fn data_class_copy_null_checks_nonnull_reference_params() {
    // kotlinc guards each non-null reference `copy` parameter with `checkNotNullParameter` at entry
    // (the same null-checks the constructor emits), but never a primitive one. Mirror that.
    let Some((dir, jh)) = krusty_compile(
        "dccopynull",
        "data class D(val s: String, val n: Int)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let text = javap(&jh, &dir.join("D.class"));
    let _ = std::fs::remove_dir_all(&dir);
    // Isolate the `copy(` method body (up to the next method declaration).
    let copy = &text[text.find(" copy(").expect("copy method")..];
    let copy = &copy[..copy.find("copy$default").unwrap_or(copy.len())];
    assert!(
        copy.contains("checkNotNullParameter") && copy.contains("// String s"),
        "copy must null-check its non-null String param `s`:\n{copy}"
    );
    // Exactly one guard — the `Int` param must NOT be checked.
    assert_eq!(
        copy.matches("checkNotNullParameter").count(),
        1,
        "copy must guard only the reference param, not the primitive `n`:\n{copy}"
    );
}

#[test]
fn classpath_interface_override_is_not_final() {
    let Some(jh) = java_home() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(libdir) = common::compile_lib(
        "cpiface",
        "package p\ninterface Port { fun handle(s: String): String }\n",
    ) else {
        return;
    };
    let src = "import p.Port\n\
        class Adapter : Port { override fun handle(s: String): String = s + \"!\" }\n\
        fun box() = \"OK\"\n";
    let classes = common::compile_in_process(src, "Main", &[libdir], Some(&jdk))
        .expect("krusty should compile the adapter");
    let dir = std::env::temp_dir().join(format!("krusty_cpiface_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (internal, bytes) in &classes {
        let path = dir.join(format!("{internal}.class"));
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).ok();
        }
        fs::write(&path, bytes).unwrap();
    }
    let text = javap(&jh, &dir.join("Adapter.class"));
    let _ = fs::remove_dir_all(&dir);
    let line = text
        .lines()
        .find(|l| l.contains(" handle("))
        .expect("Adapter must declare handle");
    assert!(
        !line.contains("final"),
        "a classpath-interface override must NOT be final (kotlinc drops ACC_FINAL):\n{line}"
    );
}

#[test]
fn data_class_object_overrides_are_not_final() {
    // kotlinc leaves a data class's Object-overrides (toString/hashCode/equals) `public` (open) even in
    // a final class, but emits component/copy/getX as `public final`. Match that exactly.
    let Some((dir, jh)) = krusty_compile(
        "dcfinal",
        "data class D(val s: String, val n: Int)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let out = Command::new(format!("{jh}/bin/javap"))
        .arg("-p")
        .arg(dir.join("D.class"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let l = line.trim();
        if l.contains(" toString(") || l.contains(" hashCode(") || l.contains(" equals(") {
            assert!(
                !l.contains("final"),
                "Object-override must NOT be final (kotlinc keeps it open):\n{l}"
            );
        }
        if l.contains(" component") || l.contains(" copy(") {
            assert!(l.contains("final"), "component/copy must be final:\n{l}");
        }
    }
}

#[test]
fn data_class_nonnull_string_hashes_via_string_hashcode() {
    // Both a non-null `String` and a nullable `String?` field hash via `invokevirtual String.hashCode()`
    // (kotlinc's shape). The nullable one is null-guarded inline (`d != null ? d.hashCode() : 0`, an
    // `ifnonnull` branch), NOT routed through `Objects.hashCode`.
    let Some((dir, jh)) = krusty_compile(
        "dcstrhash",
        "data class D(val s: String, val q: String?)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let text = javap(&jh, &dir.join("D.class"));
    let _ = std::fs::remove_dir_all(&dir);
    let hc = &text[text.find("int hashCode").expect("hashCode")..];
    let hc = &hc[..hc[1..].find("\n\n").map(|p| p + 1).unwrap_or(hc.len())];
    assert!(
        hc.contains("String.hashCode"),
        "String fields must hash via String.hashCode:\n{hc}"
    );
    assert!(
        hc.contains("ifnonnull"),
        "nullable String? field must be null-guarded inline (ifnonnull), not Objects.hashCode:\n{hc}"
    );
    assert!(
        !hc.contains("Objects.hashCode"),
        "nullable String? field must NOT route through Objects.hashCode:\n{hc}"
    );
}

#[test]
fn data_class_hash_shapes_match_kotlinc_per_field_kind() {
    // kotlinc's per-field-kind hash dispatch, shape-for-shape: an ARRAY content-hashes via
    // `java.util.Arrays.hashCode` (and content-prints via `Arrays.toString`); a BOXED nullable
    // primitive (`Int?`) dispatches `Object.hashCode()` (its Kotlin type has no JVM class to name);
    // a custom-class field dispatches a virtual `hashCode()` on its OWN class. None of them route
    // through `Objects.hashCode`.
    let Some((dir, jh)) = krusty_compile(
        "dchashkinds",
        "class Own\ndata class D(val xs: IntArray, val b: Int?, val o: Own)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let text = javap(&jh, &dir.join("D.class"));
    let _ = std::fs::remove_dir_all(&dir);
    let hc = &text[text.find("int hashCode").expect("hashCode")..];
    let hc = &hc[..hc[1..].find("\n\n").map(|p| p + 1).unwrap_or(hc.len())];
    assert!(
        hc.contains("Arrays.hashCode"),
        "array field must content-hash via Arrays.hashCode:\n{hc}"
    );
    assert!(
        hc.contains("ifnonnull"),
        "boxed Int? field must be null-guarded inline (ifnonnull):\n{hc}"
    );
    assert!(
        hc.contains("Object.hashCode"),
        "boxed Int? field must dispatch Object.hashCode:\n{hc}"
    );
    assert!(
        hc.contains("Own.hashCode"),
        "custom-class field must dispatch its own class's hashCode:\n{hc}"
    );
    assert!(
        !hc.contains("Objects.hashCode"),
        "no field kind routes through Objects.hashCode:\n{hc}"
    );
    let ts = &text[text.find("String toString").expect("toString")..];
    let ts = &ts[..ts[1..].find("\n\n").map(|p| p + 1).unwrap_or(ts.len())];
    assert!(
        ts.contains("Arrays.toString"),
        "array field must content-print via Arrays.toString:\n{ts}"
    );
}

/// An INTERFACE-typed data-class field hashes via `Object.hashCode()` (kotlinc's owner) — `hashCode`
/// is not an interface member, so a Methodref on the interface owner would throw
/// `IncompatibleClassChangeError` at runtime ("Found interface, but class was expected"). Runnable:
/// the box() call would die with that error under the old owner.
#[test]
fn data_class_interface_field_hashes_via_object_hashcode() {
    let Some((dir, jh)) = krusty_compile(
        "dcifacehash",
        "interface Marker\nclass M : Marker\ndata class D(val m: Marker, val n: Int)\n\
         fun box(): String { val d = D(M(), 1); return if (d.hashCode() == d.hashCode()) \"OK\" else \"ne\" }\n",
    ) else {
        return;
    };
    let text = javap(&jh, &dir.join("D.class"));
    let _ = std::fs::remove_dir_all(&dir);
    let hc = &text[text.find("int hashCode").expect("hashCode")..];
    let hc = &hc[..hc[1..].find("\n\n").map(|p| p + 1).unwrap_or(hc.len())];
    assert!(
        hc.contains("Object.hashCode"),
        "interface-typed field must dispatch Object.hashCode:\n{hc}"
    );
    assert!(
        !hc.contains("Marker.hashCode"),
        "interface-typed field must NOT name the interface as the owner:\n{hc}"
    );
}

// ---- safe-call + elvis primitive fusion (no boxing) -----------------------------------------

#[test]
fn safe_call_elvis_primitive_does_not_box() {
    // `s?.length ?: -1` (primitive result) must null-check the receiver and read the primitive member
    // directly (`ifnull` + `String.length`) — NOT box the member to Integer and unbox through the elvis.
    let Some(d) = facade_disasm(
        "scelvis",
        "fun nn(s: String?): Int = s?.length ?: -1\nfun box(): String = if (nn(\"abc\") == 3 && nn(null) == -1) \"OK\" else \"f\"\n",
    ) else {
        return;
    };
    assert!(
        !d.contains("Integer.valueOf"),
        "`s?.length ?: -1` must not box the member to Integer:\n{d}"
    );
    assert!(
        d.contains("ifnull") && d.contains("String.length"),
        "expected a fused ifnull + primitive String.length:\n{d}"
    );
}

// ---- Phase 398: top-level property field modifiers + accessors -------------------------------

#[test]
fn top_level_property_abi_matches_kotlin() {
    let Some(d) = facade_disasm(
        "tlp",
        "val x: Int = 5\nvar y: String = \"a\"\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    assert!(
        d.contains("private static final int x"),
        "top-level val must be `private static final`:\n{d}"
    );
    assert!(
        d.contains("private static java.lang.String y"),
        "top-level var must be `private static`:\n{d}"
    );
    assert!(d.contains("getX()"), "expected synthesized getX():\n{d}");
    assert!(d.contains("getY()"), "expected synthesized getY():\n{d}");
    assert!(
        d.contains("setY(java.lang.String)"),
        "expected synthesized setY():\n{d}"
    );
}

// ---- Differential: a counting loop is BYTE-IDENTICAL to kotlinc ------------------------------

#[test]
fn for_in_intarray_is_byte_identical_to_kotlinc() {
    assert_diff("for_in_intarray");
}

/// Normalized javap of `class_file`, optionally sliced to just the method whose disassembly contains
/// `marker` (up to the next blank line) — for asserting one synthesized method (`hashCode`/`equals`).
fn disasm(jh: &str, class_file: &std::path::Path, marker: Option<&str>) -> String {
    let full = javap(jh, class_file);
    match marker {
        Some(m) => {
            let s = full
                .find(m)
                .unwrap_or_else(|| panic!("method marker {m:?} not found"));
            let rest = &full[s..];
            let end = rest[1..].find("\n\n").map(|p| p + 1).unwrap_or(rest.len());
            normalize(&rest[..end])
        }
        None => normalize(&full),
    }
}

/// One differential parity case: a uniquely-named source, the class to disassemble, and an optional
/// method-slice marker. The unique file name gives each its own facade so they all compile together.
struct DiffCase {
    name: &'static str,
    file: &'static str,
    src: &'static str,
    class: &'static str,
    marker: Option<&'static str>,
}

/// Every differential parity case. Compiled ALL AT ONCE (one kotlinc + one krusty invocation) — see
/// `diff_refs`. Add a case here and reference it by `name` from a `#[test]` via `assert_diff`.
fn diff_cases() -> Vec<DiffCase> {
    vec![
        DiffCase { name: "ruc", file: "Ruc.kt", class: "RucKt", marker: None,
            src: "fun box(): String {\n  var s = 0\n  for (i in 0 until 10) s += i\n  return \"OK\"\n}\n" },
        DiffCase { name: "rtc", file: "Rtc.kt", class: "RtcKt", marker: None,
            src: "fun box(): String {\n  var s = 0\n  for (i in 1..10) s += i\n  return \"OK\"\n}\n" },
        DiffCase { name: "ruv", file: "Ruv.kt", class: "RuvKt", marker: None,
            src: "fun box(): String {\n  var s = 0\n  val n = 5\n  for (i in 0 until n) s += i\n  return \"OK\"\n}\n" },
        DiffCase { name: "dtc", file: "Dtc.kt", class: "DtcKt", marker: None,
            src: "fun box(): String {\n  var s = 0\n  for (i in 10 downTo 2) s += i\n  return \"OK\"\n}\n" },
        DiffCase { name: "for_in_intarray", file: "ForInIntArray.kt", class: "ForInIntArrayKt", marker: None,
            src: "fun box(): String {\n  for (x in IntArray(5)) {\n    if (x != 0) return \"Fail $x\"\n  }\n  return \"OK\"\n}\n" },
        DiffCase { name: "for_in_local_array", file: "ForInLocalArray.kt", class: "ForInLocalArrayKt", marker: None,
            src: "fun box(): String {\n  val a = IntArray(5)\n  var s = 0\n  for (x in a) { s += x }\n  return if (s == 0) \"OK\" else \"Fail\"\n}\n" },
        DiffCase { name: "dc_hash", file: "DcHash.kt", class: "P", marker: Some("int hashCode"),
            src: "data class P(val b: Byte, val s: Short, val c: Char, val i: Int, val l: Long, val f: Float, val d: Double, val bo: Boolean)\nfun dcHashBox() = \"OK\"\n" },
        DiffCase { name: "dc_eq", file: "DcEq.kt", class: "D", marker: Some("boolean equals"),
            src: "data class D(val s: String, val n: Int)\nfun dcEqBox() = \"OK\"\n" },
    ]
}

/// Compile ALL differential cases with kotlinc ONCE and krusty ONCE (fresh — so it tracks whatever
/// kotlinc version/config is configured, no committed goldens to go stale), disassemble each side, and
/// cache `name → (krusty_disasm, kotlinc_disasm)` for the whole test process. `None` when the
/// provisioned toolchain is unavailable (the tests then skip).
fn diff_refs() -> Option<&'static std::collections::HashMap<String, (String, String)>> {
    static CACHE: std::sync::OnceLock<Option<std::collections::HashMap<String, (String, String)>>> =
        std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let jh = java_home()?;
            let cases = diff_cases();
            let dir = std::env::temp_dir().join(format!("krusty_diff_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            let src_dir = dir.join("src");
            let kref = dir.join("kref");
            let krout = dir.join("krout");
            fs::create_dir_all(&src_dir).unwrap();
            fs::create_dir_all(&kref).unwrap();
            fs::create_dir_all(&krout).unwrap();
            let files: Vec<std::path::PathBuf> = cases
                .iter()
                .map(|c| {
                    let p = src_dir.join(c.file);
                    fs::write(&p, c.src).unwrap();
                    p
                })
                .collect();
            // kotlinc — one server-backed invocation for every case.
            let mut args: Vec<String> = files
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            args.extend(["-d".to_string(), kref.to_string_lossy().into_owned()]);
            let Some((code, stderr)) = common::kotlinc_compile(&args) else {
                eprintln!("skip (provisioned kotlinc server unavailable)");
                let _ = fs::remove_dir_all(&dir);
                return None;
            };
            if code != 0 {
                eprintln!("skip (kotlinc batch failed): {stderr}");
                let _ = fs::remove_dir_all(&dir);
                return None;
            }
            // krusty — one invocation for every case.
            let kc = Command::new(env!("CARGO_BIN_EXE_krusty"))
                .args(["-d", krout.to_str().unwrap()])
                .args(&files)
                .output()
                .unwrap();
            assert!(
                kc.status.success(),
                "krusty batch failed: {}",
                String::from_utf8_lossy(&kc.stderr)
            );
            let mut map = std::collections::HashMap::new();
            for c in &cases {
                let kr = disasm(&jh, &krout.join(format!("{}.class", c.class)), c.marker);
                let ko = disasm(&jh, &kref.join(format!("{}.class", c.class)), c.marker);
                map.insert(c.name.to_string(), (kr, ko));
            }
            let _ = fs::remove_dir_all(&dir);
            Some(map)
        })
        .as_ref()
}

/// Assert the named differential case's krusty disassembly equals the fresh kotlinc one. Skips when the
/// provisioned kotlinc toolchain is unavailable.
fn assert_diff(name: &str) {
    let Some(refs) = diff_refs() else {
        eprintln!("skip ({name}: provisioned kotlinc/JAVA_HOME unavailable)");
        return;
    };
    let (kr, ko) = refs
        .get(name)
        .expect("differential case registered in diff_cases()");
    assert_eq!(
        kr, ko,
        "{name}: krusty bytecode must match kotlinc (fresh, same version)"
    );
}

/// Counted range loops with unit step must be byte-identical to kotlinc: a CONSTANT bound folds to a
/// single `i < C` exclusive test (no hoisted bound local, no overflow guard) — `1..10` → `i < 11`,
/// `0 until 10` → `i < 10`; a variable `until` bound hoists but still needs no guard.
#[test]
fn range_until_and_through_loops_byte_identical_to_kotlinc() {
    assert_diff("ruc");
    assert_diff("rtc");
    assert_diff("ruv");
}

/// A constant `downTo` loop folds to an exclusive `(C-1) < i` test (no hoisted bound, no guard),
/// byte-identical to kotlinc — for a bound `C-1 != 0` (a `C-1 == 0`, i.e. `downTo 1`, still hits the
/// compare-to-zero divergence and is a documented follow-up).
#[test]
fn downto_constant_loop_byte_identical_to_kotlinc() {
    assert_diff("dtc");
}

/// Shape guard (no kotlinc): a constant-bound `0 until 10` loop must NOT hoist the bound into a local
/// (no `istore` of the bound) and must NOT emit an overflow break (`if_icmpne … goto` guard) — it is a
/// plain `iload i; bipush 10; if_icmpge exit` counted loop.
#[test]
fn constant_until_loop_has_no_bound_local_or_guard() {
    let Some(d) = facade_disasm(
        "noguard",
        "fun box(): String {\n  var s = 0\n  for (i in 0 until 10) s += i\n  return \"OK\"\n}\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    // The constant bound is inlined in the condition (`bipush 10; if_icmpge`), not loaded from a
    // hoisted slot — and there is no overflow break guard (`if_icmpne … goto`).
    assert!(
        n.contains("bipush 10\nif_icmpge"),
        "the constant bound must be inlined in the loop condition:\n{n}"
    );
    assert!(
        !n.contains("if_icmpne"),
        "an exclusive constant-bound loop needs no overflow break guard:\n{n}"
    );
}

/// `for (x in localArray)` must iterate on the EXISTING local directly — kotlinc does not snapshot an
/// already-local iterable into a fresh slot. krusty used to emit a redundant `aload; astore` copy.
/// Byte-identical (normalized) to kotlinc.
#[test]
fn for_in_local_array_no_redundant_copy_is_byte_identical_to_kotlinc() {
    assert_diff("for_in_local_array");
}

/// Shape guard (no kotlinc): `for (x in localArray)` must NOT re-store the array into a second slot.
/// The array val gets exactly one `astore`; the loop reads it back with `aload` — never an extra
/// `astore` of the array reference between the val and the loop.
#[test]
fn for_in_local_array_does_not_copy_array_to_temp() {
    let src = "fun box(): String {\n  val a = IntArray(5)\n  var s = 0\n  for (x in a) { s += x }\n  return \"OK\"\n}\n";
    let Some((dir, _jh)) = krusty_compile("shapert", src) else {
        return;
    };
    let jh = java_home().unwrap();
    let d = javap(&jh, &dir.join("BKt.class"));
    let _ = fs::remove_dir_all(&dir);
    // The array reference is stored once (the `val a`); a redundant loop copy would be a 2nd astore of
    // an object slot. After `astore_0` (a) we expect the loop to `aload_0` for arraylength/iaload, not
    // store the array again.
    let astore_count = d.matches("astore").count();
    // slots: a(0). i, n, x are int (istore). sum is int. So exactly ONE astore (the array val `a`).
    assert_eq!(
        astore_count, 1,
        "expected one astore (the array val); a redundant array copy adds another:\n{d}"
    );
}

/// The `hashCode` of an all-primitive `data class` must be byte-identical to kotlinc: each field hashed
/// via its boxed `X.hashCode(prim)` static, folded into a `result` LOCAL (`result = result*31 + h`).
#[test]
fn data_class_primitive_hashcode_is_byte_identical_to_kotlinc() {
    // Slice just `hashCode` (the access-flag `final` divergence on the Object-overrides is a SEPARATE
    // parity item; the Code attribute asserted here is unaffected).
    assert_diff("dc_hash");
}

/// A data class `equals` must be byte-identical to kotlinc: the `this === other` identity fast-path, the
/// `instanceof; ifne` guard (no materialized boolean), a single `checkcast` into a local, then per-field
/// `Intrinsics.areEqual` / `if_icmp` compares.
#[test]
fn data_class_equals_is_byte_identical_to_kotlinc() {
    assert_diff("dc_eq");
}

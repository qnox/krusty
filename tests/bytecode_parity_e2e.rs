//! Bytecode-parity TDD: assert krusty emits the SAME JVM instruction shapes kotlinc does for the
//! patterns closed in phases 397–400. Two kinds of check:
//!   * shape assertions on krusty's own `javap -c` output (no kotlinc needed) — the regression guard;
//!   * a differential full-class normalized-equality check vs the real kotlinc (gated on KRUSTY_KOTLINC).
//!
//! Run with `JAVA_HOME` set; the kotlinc differential parts also need `KRUSTY_KOTLINC`.

use std::fs;
use std::process::Command;

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
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_bcp_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("B.kt"), src).unwrap();
    let out = Command::new(krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("B.kt"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name}: krusty failed to compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    // A non-null `String` field hashes via `invokevirtual String.hashCode()` (kotlinc's shape); a
    // nullable `String?` stays on the null-safe `Objects.hashCode`.
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
        "non-null String field must hash via String.hashCode:\n{hc}"
    );
    assert!(
        hc.contains("Objects.hashCode"),
        "nullable String? field must stay on the null-safe Objects.hashCode:\n{hc}"
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
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skip (set KRUSTY_KOTLINC for the differential check)");
        return;
    };
    let src = "fun box(): String {\n  for (x in IntArray(5)) {\n    if (x != 0) return \"Fail $x\"\n  }\n  return \"OK\"\n}\n";
    let Some((dir, jh)) = krusty_compile("diff", src) else {
        return;
    };
    let stdlib = format!(
        "{}/../lib/kotlin-stdlib.jar",
        std::path::Path::new(&kotlinc).parent().unwrap().display()
    );
    let kdir = dir.join("kref");
    fs::create_dir_all(&kdir).unwrap();
    let cc = Command::new(&kotlinc)
        .arg(dir.join("B.kt"))
        .args(["-d", kdir.to_str().unwrap()])
        .env("JAVA_HOME", &jh)
        .output()
        .unwrap();
    if !cc.status.success() {
        eprintln!(
            "skip (kotlinc unavailable/failed): {}",
            String::from_utf8_lossy(&cc.stderr)
        );
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let krusty_d = normalize(&javap(&jh, &dir.join("BKt.class")));
    let kotlinc_d = normalize(&javap(&jh, &kdir.join("BKt.class")));
    let _ = fs::remove_dir_all(&dir);
    let _ = stdlib; // (no stdlib needed to compile this snippet)
    assert_eq!(
        krusty_d, kotlinc_d,
        "krusty bytecode must be byte-identical (normalized) to kotlinc for a counting loop"
    );
}

/// Compile `src` with krusty AND kotlinc, assert the facade class is byte-identical (normalized).
/// Skips (returns) when the kotlinc differential env is unavailable.
fn assert_byte_identical_to_kotlinc(name: &str, src: &str) {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skip (set KRUSTY_KOTLINC)");
        return;
    };
    let Some((dir, jh)) = krusty_compile(name, src) else {
        return;
    };
    let kdir = dir.join("kref");
    fs::create_dir_all(&kdir).unwrap();
    let cc = Command::new(&kotlinc)
        .arg(dir.join("B.kt"))
        .args(["-d", kdir.to_str().unwrap()])
        .env("JAVA_HOME", &jh)
        .output()
        .unwrap();
    if !cc.status.success() {
        eprintln!(
            "skip (kotlinc failed): {}",
            String::from_utf8_lossy(&cc.stderr)
        );
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let kr = normalize(&javap(&jh, &dir.join("BKt.class")));
    let kc = normalize(&javap(&jh, &kdir.join("BKt.class")));
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        kr, kc,
        "{name}: krusty bytecode must be byte-identical to kotlinc"
    );
}

/// Counted range loops with unit step must be byte-identical to kotlinc: a CONSTANT bound folds to a
/// single `i < C` exclusive test (no hoisted bound local, no overflow guard) — `1..10` → `i < 11`,
/// `0 until 10` → `i < 10`; a variable `until` bound hoists but still needs no guard.
#[test]
fn range_until_and_through_loops_byte_identical_to_kotlinc() {
    assert_byte_identical_to_kotlinc(
        "ruc",
        "fun box(): String {\n  var s = 0\n  for (i in 0 until 10) s += i\n  return \"OK\"\n}\n",
    );
    assert_byte_identical_to_kotlinc(
        "rtc",
        "fun box(): String {\n  var s = 0\n  for (i in 1..10) s += i\n  return \"OK\"\n}\n",
    );
    assert_byte_identical_to_kotlinc(
        "ruv",
        "fun box(): String {\n  var s = 0\n  val n = 5\n  for (i in 0 until n) s += i\n  return \"OK\"\n}\n",
    );
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
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skip (set KRUSTY_KOTLINC for the differential check)");
        return;
    };
    let src = "fun box(): String {\n  val a = IntArray(5)\n  var s = 0\n  for (x in a) { s += x }\n  return if (s == 0) \"OK\" else \"Fail\"\n}\n";
    let Some((dir, jh)) = krusty_compile("diffloc", src) else {
        return;
    };
    let kdir = dir.join("kref");
    fs::create_dir_all(&kdir).unwrap();
    let cc = Command::new(&kotlinc)
        .arg(dir.join("B.kt"))
        .args(["-d", kdir.to_str().unwrap()])
        .env("JAVA_HOME", &jh)
        .output()
        .unwrap();
    if !cc.status.success() {
        eprintln!(
            "skip (kotlinc unavailable): {}",
            String::from_utf8_lossy(&cc.stderr)
        );
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let krusty_d = normalize(&javap(&jh, &dir.join("BKt.class")));
    let kotlinc_d = normalize(&javap(&jh, &kdir.join("BKt.class")));
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        krusty_d, kotlinc_d,
        "for-in-local-array must be byte-identical to kotlinc (no redundant array copy)"
    );
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
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skip (set KRUSTY_KOTLINC for the differential check)");
        return;
    };
    let src = "data class P(val b: Byte, val s: Short, val c: Char, val i: Int, val l: Long, val f: Float, val d: Double, val bo: Boolean)\nfun box() = \"OK\"\n";
    let Some((dir, jh)) = krusty_compile("dchash", src) else {
        return;
    };
    let kdir = dir.join("kref");
    fs::create_dir_all(&kdir).unwrap();
    let cc = Command::new(&kotlinc)
        .arg(dir.join("B.kt"))
        .args(["-d", kdir.to_str().unwrap()])
        .env("JAVA_HOME", &jh)
        .output()
        .unwrap();
    if !cc.status.success() {
        eprintln!(
            "skip (kotlinc failed): {}",
            String::from_utf8_lossy(&cc.stderr)
        );
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    // Slice just the `hashCode` method's disassembly (the access-flag `final` divergence on the
    // Object-overrides — toString/hashCode/equals — is a SEPARATE parity item; the Code attribute,
    // which is what this asserts, is unaffected by it).
    let slice = |full: &str| -> String {
        let s = full.find("int hashCode").expect("hashCode method");
        let rest = &full[s..];
        let end = rest[1..].find("\n\n").map(|p| p + 1).unwrap_or(rest.len());
        normalize(&rest[..end])
    };
    let kr = slice(&javap(&jh, &dir.join("P.class")));
    let kc = slice(&javap(&jh, &kdir.join("P.class")));
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        kr, kc,
        "all-primitive data-class hashCode must be byte-identical to kotlinc"
    );
}

/// A data class `equals` must be byte-identical to kotlinc: the `this === other` identity fast-path, the
/// `instanceof; ifne` guard (no materialized boolean), a single `checkcast` into a local, then per-field
/// `Intrinsics.areEqual` / `if_icmp` compares.
#[test]
fn data_class_equals_is_byte_identical_to_kotlinc() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skip (set KRUSTY_KOTLINC for the differential check)");
        return;
    };
    let src = "data class D(val s: String, val n: Int)\nfun box() = \"OK\"\n";
    let Some((dir, jh)) = krusty_compile("dceq", src) else {
        return;
    };
    let kdir = dir.join("kref");
    fs::create_dir_all(&kdir).unwrap();
    let cc = Command::new(&kotlinc)
        .arg(dir.join("B.kt"))
        .args(["-d", kdir.to_str().unwrap()])
        .env("JAVA_HOME", &jh)
        .output()
        .unwrap();
    if !cc.status.success() {
        eprintln!(
            "skip (kotlinc failed): {}",
            String::from_utf8_lossy(&cc.stderr)
        );
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let slice = |full: &str| -> String {
        let s = full.find("boolean equals").expect("equals method");
        let rest = &full[s..];
        let end = rest[1..].find("\n\n").map(|p| p + 1).unwrap_or(rest.len());
        normalize(&rest[..end])
    };
    let kr = slice(&javap(&jh, &dir.join("D.class")));
    let kc = slice(&javap(&jh, &kdir.join("D.class")));
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        kr, kc,
        "data-class equals must be byte-identical to kotlinc"
    );
}

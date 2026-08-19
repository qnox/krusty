//! Bytecode-parity TDD: assert krusty emits the SAME JVM instruction shapes kotlinc does for the
//! patterns closed in phases 397–400. Two kinds of check:
//!   * shape assertions on krusty's own `javap -c` output (no kotlinc needed) — the regression guard;
//!   * a differential full-class normalized-equality check vs the provisioned real kotlinc.
//!
//! Run with `JAVA_HOME` set; kotlinc path overrides are optional.

use std::fs;

use super::common;

fn java_home() -> String {
    common::java_home()
}

/// Compile `src` with the krusty binary into a fresh dir; return the dir (or `None` if javap/JAVA_HOME
/// is unavailable — the test then skips).
fn krusty_compile(name: &str, src: &str) -> Option<(std::path::PathBuf, String)> {
    let jh = java_home();

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

/// Like [`krusty_compile`] but with the kotlin stdlib on the classpath (for collection/library types).
/// `None` if javap/`JAVA_HOME`/the stdlib jar is unavailable — the test then skips.
fn krusty_compile_stdlib(name: &str, src: &str) -> Option<(std::path::PathBuf, String)> {
    let jh = java_home();

    let stdlib = common::stdlib_jar();
    let dir = std::env::temp_dir().join(format!("krusty_bcp_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let classes = common::compile_in_process(src, "B", &[stdlib], None)
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
fn javap(_jh: &str, class_file: &std::path::Path) -> String {
    common::javap(&["-c", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable")
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

#[test]
fn compare_against_zero_in_value_position_is_single_operand_branch() {
    // Same fusion as above, but where the comparison PRODUCES a Boolean rather than driving a branch:
    // `a != 0` is `iload_0; ifeq` (kotlinc), never `iload_0; iconst_0; if_icmpne`.
    let Some(d) = facade_disasm(
        "cmp0v",
        "fun ne0(a: Int): Boolean = a != 0\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    assert!(
        n.contains("iload_0\nifeq"),
        "value-position `a != 0` must fuse to a single-operand compare-to-zero branch:\n{n}"
    );
    assert!(
        !n.contains("iconst_0\nif_icmp"),
        "value-position comparison against 0 must not materialize iconst_0 then if_icmp:\n{n}"
    );
}

#[test]
fn long_compare_in_value_position_tests_lcmp_without_materialized_zero() {
    // `lcmp` already leaves -1/0/1 on the stack, so the test against it is the single-operand
    // `ifeq`/`ifne`/`ifge`/… family — NOT a materialized `iconst_0` plus a two-operand `if_icmp*`.
    // kotlinc: `lcmp; ifne` for `==`, `lcmp; ifge` for `<`.
    let Some(d) = facade_disasm(
        "lcmpv",
        "fun eq(a: Long, b: Long): Boolean = a == b\n\
         fun ne(a: Long, b: Long): Boolean = a != b\n\
         fun lt(a: Long, b: Long): Boolean = a < b\n\
         fun box() = \"OK\"\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    assert!(
        !n.contains("lcmp\niconst_0"),
        "`lcmp` must not be followed by a materialized zero — it already compares against 0:\n{n}"
    );
    // Polarity matches kotlinc: branch on the NEGATED comparison to the `false` arm, fall through to
    // `iconst_1`. So `==` tests `ifne`, `!=` tests `ifeq`, `<` tests `ifge`.
    for want in ["lcmp\nifne", "lcmp\nifeq", "lcmp\nifge"] {
        assert!(
            n.contains(want),
            "expected fused `{want}` (kotlinc's shape):\n{n}"
        );
    }
}

#[test]
fn double_compare_in_value_position_tests_dcmp_without_materialized_zero() {
    // Covers BOTH NaN variants: `==` uses `dcmpg`, `>` uses `dcmpl` (NaN → -1), so a NaN operand makes
    // either comparison false. A regression that picked the wrong variant would still fuse, so assert
    // the variant too, not just the absence of the zero.
    let Some(d) = facade_disasm(
        "dcmpv",
        "fun eq(a: Double, b: Double): Boolean = a == b\n\
         fun gt(a: Double, b: Double): Boolean = a > b\n\
         fun box() = \"OK\"\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    assert!(
        !n.contains("dcmpg\niconst_0") && !n.contains("dcmpl\niconst_0"),
        "`dcmp*` must not be followed by a materialized zero:\n{n}"
    );
    assert!(
        n.contains("dcmpg\nifne"),
        "expected fused `dcmpg; ifne` for `==` (kotlinc's shape):\n{n}"
    );
    assert!(
        n.contains("dcmpl\nifle"),
        "expected fused `dcmpl; ifle` for `>` — the NaN-correct variant (kotlinc's shape):\n{n}"
    );
}

#[test]
fn float_compare_in_value_position_tests_fcmp_without_materialized_zero() {
    let Some(d) = facade_disasm(
        "fcmpv",
        "fun eq(a: Float, b: Float): Boolean = a == b\n\
         fun gt(a: Float, b: Float): Boolean = a > b\n\
         fun box() = \"OK\"\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    assert!(
        !n.contains("fcmpg\niconst_0") && !n.contains("fcmpl\niconst_0"),
        "`fcmp*` must not be followed by a materialized zero:\n{n}"
    );
    assert!(
        n.contains("fcmpg\nifne") && n.contains("fcmpl\nifle"),
        "expected fused `fcmpg; ifne` for `==` and `fcmpl; ifle` for `>`:\n{n}"
    );
}

#[test]
fn zero_on_the_left_in_value_position_fuses_only_for_equality() {
    // kotlinc fuses `0 == x` / `0 != x` (they are symmetric) but does NOT mirror the ORDERING operators:
    // `0 < x` stays the two-operand `iconst_0; iload x; if_icmpge`. Mirroring it to `iload x; ifle` is
    // shorter but diverges, so the fusion is deliberately restricted to `==`/`!=`.
    let Some(d) = facade_disasm(
        "zeroleft",
        "fun zeq(a: Int): Boolean = 0 == a\n\
         fun zlt(a: Int): Boolean = 0 < a\n\
         fun box() = \"OK\"\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    assert!(
        n.contains("iload_0\nifne"),
        "`0 == a` must fuse to the single-operand `ifne`:\n{n}"
    );
    assert!(
        n.contains("iconst_0\niload_0\nif_icmpge"),
        "`0 < a` must keep kotlinc's two-operand form, NOT be mirrored to `ifle`:\n{n}"
    );
    assert!(
        !n.contains("iload_0\nifle"),
        "`0 < a` must not be mirrored — that fuses shorter than kotlinc and diverges:\n{n}"
    );
}

#[test]
fn zero_on_the_left_in_branch_position_fuses_only_for_equality() {
    // Branch and value consumers share one comparison emitter. This branch-position regression is
    // intentionally separate from the value-position check above: the old branch-only implementation
    // mirrored `0 < a` to the shorter `a > 0`, producing `iload_0; ifle` even though kotlinc retains
    // operand order and emits `iconst_0; iload_0; if_icmpge` for the false edge.
    let Some(d) = facade_disasm(
        "zeroleftbranch",
        "fun zlt(a: Int): String {\n  if (0 < a) return \"t\"\n  return \"f\"\n}\n\
         fun box() = \"OK\"\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    assert!(
        n.contains("iconst_0\niload_0\nif_icmpge"),
        "branch-position `0 < a` must retain kotlinc's two-operand, source-order form:\n{n}"
    );
    assert!(
        !n.contains("iload_0\nifle"),
        "branch-position `0 < a` must not select a positional mirror optimization:\n{n}"
    );
}

#[test]
fn referential_null_comparison_in_value_position_is_single_operand() {
    // `a === null` is a NULL comparison, not a referential one: `ifnonnull`, not `aconst_null;
    // if_acmpne`. The null-literal check must therefore run BEFORE the `===` reference arm.
    let Some(d) = facade_disasm(
        "refnullv",
        "fun isNull(a: Any?): Boolean = a === null\n\
         fun notNull(a: Any?): Boolean = a !== null\n\
         fun box() = \"OK\"\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    assert!(
        !n.contains("aconst_null"),
        "`a === null` must not materialize a null to compare against:\n{n}"
    );
    assert!(
        !n.contains("if_acmpeq") && !n.contains("if_acmpne"),
        "`a === null` must use `ifnull`/`ifnonnull`, not a two-operand reference compare:\n{n}"
    );
}

#[test]
fn value_position_comparison_does_not_poison_a_later_inline_splice() {
    // A value-position comparison used to leave the operand-height tracker over-reporting, so a LATER
    // branchy inline splice in the same expression saw a non-empty baseline and refused — which
    // escalated to a hard "inline splice failed" compile error. Fixing the merge-point accounting in
    // `materialize_cmp_bool` also fixed that, so guard it: this must COMPILE at all. Needs the stdlib
    // on the classpath for `takeIf` — the inline HOF whose splice was the victim.
    let Some((dir, jh)) = krusty_compile_stdlib(
        "splicepoison",
        "fun two(b: Boolean, s: String): String = \"$b/$s\"\n\
         fun t2(a: Any?, b: Any?, x: Int): String = two(a === b, x.takeIf { it > 0 }.toString())\n\
         fun box() = \"OK\"\n",
    ) else {
        return;
    };
    let d = javap(&jh, &dir.join("BKt.class"));
    let _ = fs::remove_dir_all(&dir);
    assert!(
        d.contains("t2"),
        "the comparison-then-splice shape must compile:\n{d}"
    );
}

#[test]
fn unsigned_long_equality_tests_lcmp_without_materialized_zero() {
    // The shape that surfaced this gap: `ULong ==` compares the carriers with `lcmp`, and must test
    // that result directly. Needs the stdlib on the classpath for `ULong`.
    let Some((dir, jh)) = krusty_compile_stdlib(
        "ulcmpv",
        "fun eq(a: ULong, b: ULong): Boolean = a == b\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let d = javap(&jh, &dir.join("BKt.class"));
    let _ = fs::remove_dir_all(&dir);
    let n = normalize(&d);
    assert!(
        !n.contains("lcmp\niconst_0"),
        "ULong `==` must test the `lcmp` result directly, not against a materialized zero:\n{n}"
    );
    assert!(
        n.contains("lcmp\nifne"),
        "expected fused `lcmp; ifne` (kotlinc's shape):\n{n}"
    );
}

#[test]
fn value_position_comparison_polarity_matches_kotlinc() {
    // kotlinc materializes a comparison's Boolean by branching on the NEGATED condition to the
    // `false` arm and falling through to `iconst_1`; the taken branch pushes `iconst_0`. Holds for
    // the null, referential and numeric arms alike, so one shape check covers all three.
    let Some(d) = facade_disasm(
        "polarity",
        "fun isNull(a: Any?): Boolean = a == null\n\
         fun refEq(a: Any, b: Any): Boolean = a === b\n\
         fun intEq(a: Int, b: Int): Boolean = a == b\n\
         fun box() = \"OK\"\n",
    ) else {
        return;
    };
    let n = normalize(&d);
    // `normalize` keeps each branch's target offset (`ifnonnull 8`), so match on the opcode followed by
    // whatever operand, then the next line.
    let followed_by = |opcode: &str, next: &str| {
        n.lines()
            .zip(n.lines().skip(1))
            .any(|(a, b)| (a == opcode || a.starts_with(&format!("{opcode} "))) && b.trim() == next)
    };
    for (opcode, next) in [
        ("ifnonnull", "iconst_1"), // `a == null` → jump away when NON-null
        ("if_acmpne", "iconst_1"), // `a === b`   → jump away when NOT identical
        ("if_icmpne", "iconst_1"), // `a == b`    → jump away when NOT equal
    ] {
        assert!(
            followed_by(opcode, next),
            "expected kotlinc's fall-through-to-true polarity `{opcode}; {next}`:\n{n}"
        );
    }
    for opcode in ["ifnull", "if_acmpeq", "if_icmpeq"] {
        assert!(
            !followed_by(opcode, "iconst_0"),
            "value-position comparisons must not use the inverted (jump-to-true) polarity \
             (`{opcode}; iconst_0`):\n{n}"
        );
    }
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
    let _ = jh;
    let text = common::javap(&["-p", &dir.join("P.class").to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_dir_all(&dir);
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
    let jh = java_home();
    let jdk = common::jdk_modules();
    let Some(libdir) = common::compile_lib(
        "cpiface",
        "package p\ninterface Port { fun handle(s: String): String }\n",
    ) else {
        return;
    };
    let src = "import p.Port\n\
        class Adapter : Port { override fun handle(s: String): String = s + \"!\" }\n\
        fun box() = \"OK\"\n";
    let classes = common::compile_in_process(src, "Main", &[libdir], Some(jdk.as_path()))
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
    let _ = jh;
    let text = common::javap(&["-p", &dir.join("D.class").to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_dir_all(&dir);
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

/// Reference-field hash owners across nullability: a concrete class keeps its OWN class as the
/// owner (guarded when nullable); a source interface — nullable or not — takes `Object`.
#[test]
fn data_class_concrete_ref_field_hashes_via_own_hashcode() {
    for (name, src, want) in [
        (
            "dcrefnn",
            "class D\ndata class C(val x: D)\nfun box() = \"OK\"\n",
            "D.hashCode",
        ),
        (
            "dcrefnull",
            "class D\ndata class C(val x: D?)\nfun box() = \"OK\"\n",
            "D.hashCode",
        ),
        (
            "dcifacenn",
            "interface I\ndata class C(val x: I)\nfun box() = \"OK\"\n",
            "java/lang/Object.hashCode",
        ),
        (
            "dcifacenull",
            "interface I\ndata class C(val x: I?)\nfun box() = \"OK\"\n",
            "java/lang/Object.hashCode",
        ),
    ] {
        let Some((dir, jh)) = krusty_compile(name, src) else {
            return;
        };
        let text = javap(&jh, &dir.join("C.class"));
        let _ = std::fs::remove_dir_all(&dir);
        let hc = &text[text.find("int hashCode").expect("hashCode")..];
        let hc = &hc[..hc[1..].find("\n\n").map(|p| p + 1).unwrap_or(hc.len())];
        assert!(
            hc.contains(want),
            "{name}: reference field must hash via {want}:\n{hc}"
        );
        assert!(
            !hc.contains("Objects.hashCode"),
            "{name}: reference field must NOT use the null-safe Objects.hashCode:\n{hc}"
        );
    }
}

/// A NULLABLE value-class data-class field (`Id?`) hashes via kotlinc's null-guarded ternary
/// `n != null ? Id.hashCode-impl(n) : 0` — an `ifnonnull` branch to the static `hashCode-impl` on the raw
/// underlying, NOT `box-impl` + `Objects.hashCode`.
#[test]
fn data_class_nullable_value_class_field_hashes_via_hashcode_impl_ternary() {
    let Some((dir, jh)) = krusty_compile_stdlib(
        "dcvcn",
        "@JvmInline\nvalue class Id(val v: String)\ndata class C(val n: Id?)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let text = javap(&jh, &dir.join("C.class"));
    let _ = std::fs::remove_dir_all(&dir);
    let hc = &text[text.find("int hashCode").expect("hashCode")..];
    let hc = &hc[..hc[1..].find("\n\n").map(|p| p + 1).unwrap_or(hc.len())];
    assert!(
        hc.contains("hashCode-impl") && hc.contains("ifnonnull"),
        "nullable value-class field must null-guard (ifnonnull) then hashCode-impl:\n{hc}"
    );
    assert!(
        !hc.contains("box-impl") && !hc.contains("Objects.hashCode"),
        "nullable value-class field must NOT box then Objects.hashCode:\n{hc}"
    );
}

/// A collection/library-interface data-class field (`List`/`Map`) hashes via `Object.hashCode` (kotlinc's
/// shape for an interface-typed field), not the null-safe static `Objects.hashCode`.
#[test]
fn data_class_collection_field_hashes_via_object_hashcode() {
    let Some((dir, jh)) = krusty_compile_stdlib(
        "dccoll",
        "data class C(val x: List<String>, val y: Map<String, Int>?)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let text = javap(&jh, &dir.join("C.class"));
    let _ = std::fs::remove_dir_all(&dir);
    let hc = &text[text.find("int hashCode").expect("hashCode")..];
    let hc = &hc[..hc[1..].find("\n\n").map(|p| p + 1).unwrap_or(hc.len())];
    assert!(
        hc.contains("Object.hashCode"),
        "collection field must hash via Object.hashCode:\n{hc}"
    );
    assert!(
        !hc.contains("Objects.hashCode"),
        "collection field must NOT use the null-safe Objects.hashCode:\n{hc}"
    );
}

/// A value-class data-class field goes through the value class's OWN static ABI in every
/// synthesized member, matching kotlinc: `hashCode` via `hashCode-impl(U)I` (never `box-impl` +
/// `Objects.hashCode`) and `equals` via `equals-impl0(U, U)Z` fused to a single `ifne`/`ifeq`
/// (never `Intrinsics.areEqual` on the underlyings).
#[test]
fn data_class_value_class_field_uses_impl_statics() {
    let Some((dir, jh)) = krusty_compile_stdlib(
        "dcvcimpl",
        "@JvmInline\nvalue class Id(val v: String)\ndata class C(val id: Id, val n: Int)\nfun box() = \"OK\"\n",
    ) else {
        return;
    };
    let text = javap(&jh, &dir.join("C.class"));
    let _ = std::fs::remove_dir_all(&dir);
    let hc = &text[text.find("int hashCode").expect("hashCode")..];
    let hc = &hc[..hc[1..].find("\n\n").map(|p| p + 1).unwrap_or(hc.len())];
    assert!(
        hc.contains("hashCode-impl"),
        "value-class field must hash via its static hashCode-impl:\n{hc}"
    );
    assert!(
        !hc.contains("box-impl") && !hc.contains("Objects.hashCode"),
        "value-class field must NOT box then Objects.hashCode:\n{hc}"
    );
    let eq = &text[text.find("boolean equals").expect("equals")..];
    let eq = &eq[..eq[1..].find("\n\n").map(|p| p + 1).unwrap_or(eq.len())];
    assert!(
        eq.contains("equals-impl0"),
        "value-class field must compare via its static equals-impl0:\n{eq}"
    );
    assert!(
        !eq.contains("Intrinsics.areEqual"),
        "value-class field must NOT compare via Intrinsics.areEqual:\n{eq}"
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

// ---- Differential: DECLARATION-POSITION type resolution is BYTE-IDENTICAL to kotlinc ---------
//
// A declaration whose type the signature walk cannot determine is resolved on demand by the engine.
// Compiling is not the bar: the resolved type reaches the field descriptor, the getter's return and
// the `@Metadata`, so the only proof that it is the RIGHT type is that the class kotlinc writes and
// the class krusty writes are the same bytes.

#[test]
fn a_local_class_member_reading_a_capture_is_byte_identical_to_kotlinc() {
    assert_diff("tr_local_class_capture");
}

#[test]
fn a_block_with_unreachable_code_is_byte_identical_to_kotlinc() {
    assert_diff("tr_unreachable_trailing");
}

#[test]
fn an_inferred_unsigned_constant_is_byte_identical_to_kotlinc() {
    assert_diff("tr_unsigned_constant");
}

#[test]
fn a_typealias_constructor_declaration_is_byte_identical_to_kotlinc() {
    assert_diff("tr_typealias_ctor");
}

#[test]
fn a_constructor_bound_by_a_lambda_return_is_byte_identical_to_kotlinc() {
    assert_diff("tr_ctor_lambda_return");
}

#[test]
fn an_inferred_member_return_read_by_a_declaration_is_byte_identical_to_kotlinc() {
    assert_diff("tr_inferred_member_return");
}

#[test]
fn a_property_reference_to_an_inferred_member_is_byte_identical_to_kotlinc() {
    assert_diff("tr_property_reference");
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
        // Declaration-position type resolution. Every one of these is a shape whose type the
        // signature walk cannot determine on its own, so the engine resolves it on demand; the
        // parity check is what proves the resolved type is the type kotlinc writes, byte for byte,
        // rather than merely a type that compiles.
        DiffCase { name: "tr_local_class_capture", file: "TrLocalCapture.kt", class: "TrLocalCaptureKt", marker: None,
            src: "fun box(): String {\n  val s = \"captured\"\n  class A(val p: String) { val s2 = s + p }\n  return if (A(\"OK\").s2 == \"capturedOK\") \"OK\" else \"F\"\n}\n" },
        DiffCase { name: "tr_unreachable_trailing", file: "TrUnreachable.kt", class: "TrUnreachableKt", marker: None,
            src: "val a1 = \"a\".let {\n  if (false) throw Error()\n  it + \"a\"\n}\nfun box(): String = if (a1 == \"aa\") \"OK\" else \"F\"\n" },
        DiffCase { name: "tr_unsigned_constant", file: "TrUnsigned.kt", class: "TrUnsignedKt", marker: None,
            src: "val maxU = UInt.MAX_VALUE\nval minU: UInt = UInt.MIN_VALUE\nfun box(): String = if (minU == 0u && maxU > 0u) \"OK\" else \"F\"\n" },
        DiffCase { name: "tr_typealias_ctor", file: "TrAlias.kt", class: "TrAliasKt", marker: None,
            src: "class Cell<T>(val x: T)\ntypealias AliasedCell<TT> = Cell<TT>\nval cell = AliasedCell(\"OK\")\nfun box(): String = cell.x\n" },
        DiffCase { name: "tr_ctor_lambda_return", file: "TrCtorLambda.kt", class: "TrCtorLambdaKt", marker: None,
            src: "class N<T>(val build: (String) -> T)\nval n = N { it + \"K\" }\nfun box(): String = n.build(\"O\")\n" },
        DiffCase { name: "tr_inferred_member_return", file: "TrMemberReturn.kt", class: "TrMemberReturnKt", marker: None,
            src: "class W<T>(val value: T) { fun get() = value }\nval got = W(\"OK\").get()\nfun box(): String = got\n" },
        DiffCase { name: "tr_property_reference", file: "TrPropRef.kt", class: "TrPropRefKt", marker: None,
            src: "class C { val bar = 42 }\nval unbound = C::bar\nval bound = C()::bar\nfun box(): String = if (unbound.get(C()) == 42 && bound.get() == 42) \"OK\" else \"F\"\n" },
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
            let jh = java_home();
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
            // krusty — one in-process module compile for every case (same driver as `krusty -d`).
            let sources: Vec<(&str, &str)> = cases
                .iter()
                .map(|c| (c.file.trim_end_matches(".kt"), c.src))
                .collect();
            let classes =
                common::compile_in_process_files(&sources, &[], None).expect("krusty batch failed");
            for (internal, bytes) in &classes {
                let path = krout.join(format!("{internal}.class"));
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                fs::write(path, bytes).unwrap();
            }
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
    let jh = java_home();
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

/// `a.equals(b)` where BOTH sides are the same unsigned type is kotlinc's `equals` INTRINSIC: an
/// unsigned value class wraps exactly one field, so its `equals` can only compare the carriers —
/// kotlinc folds the call away to the same instructions `a == b` emits, with no box in sight.
/// (`kotlin/UInt.equals` is what krusty used to emit here, on a receiver it had to box first.)
#[test]
fn same_type_unsigned_equals_compares_carriers_without_boxing() {
    for (name, src, compare) in [
        (
            "ueq",
            "fun p(n: Int): UInt = n.toUInt()\n\
fun box(): String {\n  val a = p(2); val b = p(1)\n  return if (a.equals(b)) \"f\" else \"OK\"\n}\n",
            "if_icmp",
        ),
        (
            "uleq",
            "fun p(n: Int): ULong = n.toULong()\n\
fun box(): String {\n  val a = p(2); val b = p(1)\n  return if (a.equals(b)) \"f\" else \"OK\"\n}\n",
            "lcmp",
        ),
    ] {
        let Some((dir, jh)) = krusty_compile_stdlib(name, src) else {
            return;
        };
        let d = javap(&jh, &dir.join("BKt.class"));
        let _ = fs::remove_dir_all(&dir);
        let n = normalize(&d);
        assert!(
            n.contains(compare),
            "{name}: same-type unsigned `equals` must compare carriers with `{compare}`:\n{n}"
        );
        for gone in ["box-impl", "kotlin/UInt.equals", "kotlin/ULong.equals"] {
            assert!(
                !n.contains(gone),
                "{name}: same-type unsigned `equals` must not emit `{gone}`:\n{n}"
            );
        }
    }
}

/// Every OTHER argument type keeps the value class's own equality — but kotlinc reaches it through the
/// STATIC `kotlin/UInt."equals-impl":(<carrier>Ljava/lang/Object;)Z`, whose receiver slot is the
/// carrier. The `invokevirtual kotlin/UInt.equals` krusty used to emit forces a `box-impl` on the
/// receiver purely to have a reference to invoke on; kotlinc writes that shape in exactly one place
/// (a LITERAL `null` argument, pinned below as a deliberate divergence).
///
/// The cross-carrier pair rides the same static (`UInt.equals-impl` answers `false` for a `kotlin/ULong`
/// argument — a `UInt` is never a `ULong`); only the ARGUMENT boxes, and the receiver stays unboxed.
/// All four unsigned types are covered, since each spells its own carrier in the descriptor.
#[test]
fn unsigned_equals_on_other_argument_types_calls_equals_impl_unboxed() {
    for (name, src, present, absent) in [
        (
            "ueq_any",
            "fun p(n: Int): UInt = n.toUInt()\nfun q(): Any = \"x\"\n\
fun box(): String {\n  val a = p(2)\n  return if (a.equals(q())) \"f\" else \"OK\"\n}\n",
            &["kotlin/UInt.\"equals-impl\":(ILjava/lang/Object;)Z"][..],
            &["kotlin/UInt.equals", "box-impl"][..],
        ),
        (
            "uleq_any",
            "fun p(n: Int): ULong = n.toULong()\nfun q(): Any = \"x\"\n\
fun box(): String {\n  val a = p(2)\n  return if (a.equals(q())) \"f\" else \"OK\"\n}\n",
            &["kotlin/ULong.\"equals-impl\":(JLjava/lang/Object;)Z"][..],
            &["kotlin/ULong.equals", "box-impl"][..],
        ),
        // The narrow pair spells `B`/`S` — the carrier they actually live in, not a widened `I`.
        (
            "ubeq_any",
            "fun p(n: Int): UByte = n.toUByte()\nfun q(): Any = \"x\"\n\
fun box(): String {\n  val a = p(2)\n  return if (a.equals(q())) \"f\" else \"OK\"\n}\n",
            &["kotlin/UByte.\"equals-impl\":(BLjava/lang/Object;)Z"][..],
            &["kotlin/UByte.equals", "box-impl"][..],
        ),
        (
            "useq_any",
            "fun p(n: Int): UShort = n.toUShort()\nfun q(): Any = \"x\"\n\
fun box(): String {\n  val a = p(2)\n  return if (a.equals(q())) \"f\" else \"OK\"\n}\n",
            &["kotlin/UShort.\"equals-impl\":(SLjava/lang/Object;)Z"][..],
            &["kotlin/UShort.equals", "box-impl"][..],
        ),
        (
            "ueq_string",
            "fun p(n: Int): UInt = n.toUInt()\nfun q(): String = \"x\"\n\
fun box(): String {\n  val a = p(2)\n  return if (a.equals(q())) \"f\" else \"OK\"\n}\n",
            &["kotlin/UInt.\"equals-impl\":(ILjava/lang/Object;)Z"][..],
            &["kotlin/UInt.equals", "box-impl"][..],
        ),
        (
            "ueq_nullable",
            "fun p(n: Int): UInt = n.toUInt()\nfun q(n: Int): UInt? = n.toUInt()\n\
fun box(): String {\n  val a = p(2)\n  return if (a.equals(q(1))) \"f\" else \"OK\"\n}\n",
            &["kotlin/UInt.\"equals-impl\":(ILjava/lang/Object;)Z"][..],
            &["kotlin/UInt.equals"][..],
        ),
        (
            "ueq_cross",
            "fun p(n: Int): UInt = n.toUInt()\nfun r(n: Int): ULong = n.toULong()\n\
fun box(): String {\n  val a = p(2)\n  return if (a.equals(r(2))) \"f\" else \"OK\"\n}\n",
            &[
                "kotlin/UInt.\"equals-impl\":(ILjava/lang/Object;)Z",
                "kotlin/ULong.\"box-impl\"",
            ][..],
            &["kotlin/UInt.equals", "kotlin/UInt.\"box-impl\""][..],
        ),
        // A LITERAL `null` is where kotlinc DOES box the receiver and `invokevirtual` (its intrinsic
        // declines the `Nothing?` argument). krusty keeps the static: same constant `false`, no box.
        // Pinned so the divergence is a decision on record rather than a drift nobody noticed.
        (
            "ueq_null_literal",
            "fun p(n: Int): UInt = n.toUInt()\n\
fun box(): String {\n  val a = p(2)\n  return if (a.equals(null)) \"f\" else \"OK\"\n}\n",
            &["kotlin/UInt.\"equals-impl\":(ILjava/lang/Object;)Z"][..],
            &["kotlin/UInt.equals", "box-impl"][..],
        ),
    ] {
        let Some((dir, jh)) = krusty_compile_stdlib(name, src) else {
            return;
        };
        let d = javap(&jh, &dir.join("BKt.class"));
        let _ = fs::remove_dir_all(&dir);
        let n = normalize(&d);
        for want in present {
            assert!(
                n.contains(want),
                "{name}: unsigned `equals` must emit `{want}`:\n{n}"
            );
        }
        for gone in absent {
            assert!(
                !n.contains(gone),
                "{name}: unsigned `equals` must not emit `{gone}`:\n{n}"
            );
        }
    }
}

/// An ordinary virtual call and both unsigned `equals` rewrites evaluate the RECEIVER before the
/// argument, and must keep doing so when the argument SUSPENDS. Coroutine lowering moves everything
/// after the suspension point into the resume block; a receiver left as a nested operand is therefore
/// re-evaluated there, AFTER the argument. All three routes share the same spill helper so origin and
/// operation shape cannot grow independent evaluation-order rules.
///
/// Asserted on instruction ORDER inside the state machine (`p()` before `s()`), which is what a
/// side-effecting receiver observes, and on exactly one invocation of each function so duplicated
/// evaluation cannot hide behind the first matching instruction. kotlinc emits the same order via its
/// own unnamed spill slot.
#[test]
fn member_and_unsigned_equals_evaluate_receiver_before_a_suspending_argument() {
    for (name, receiver_ty, receiver_value, arg_fn, arg_call) in [
        // The ordinary virtual-member path that originally owned the receiver-spill rule …
        (
            "member_suspend_any",
            "String",
            "\"x\"",
            "suspend fun s(): Any? = null",
            "s()",
        ),
        // The `equals-impl` path (argument is `Any`) …
        (
            "ueq_suspend_any",
            "UInt",
            "1u",
            "suspend fun s(): Any = \"x\"",
            "s()",
        ),
        // … and the same-type fold, which is a bare primitive compare with no call to hang the
        // receiver off at all.
        (
            "ueq_suspend_same",
            "UInt",
            "1u",
            "suspend fun s(): UInt = 1u",
            "s()",
        ),
    ] {
        let src = format!(
            "var log: String = \"\"\n\
fun p(): {receiver_ty} {{ log = log + \"p\"; return {receiver_value} }}\n\
{arg_fn}\n\
suspend fun t(): Boolean = p().equals({arg_call})\n"
        );
        let Some((dir, jh)) = krusty_compile_stdlib(name, &src) else {
            return;
        };
        let d = javap(&jh, &dir.join("BKt.class"));
        let _ = fs::remove_dir_all(&dir);
        // Slice `t`'s Code attribute: the two calls live in different blocks of one state machine, and
        // javap prints a method's instructions in offset order, so text order IS bytecode order.
        let body = d
            .split_once("java.lang.Object t(")
            .unwrap_or_else(|| panic!("{name}: no suspend `t` in the dump:\n{d}"))
            .1;
        let recv = body
            // The ordinary member case returns a reference while unsigned cases return their
            // primitive carrier. Match only the selected helper name/empty parameter list: the
            // return descriptor is deliberately varied by this table.
            .find("Method p:()")
            .unwrap_or_else(|| panic!("{name}: `t` never calls the receiver `p()`:\n{d}"));
        let arg = body
            .find("Method s:(Lkotlin/coroutines/Continuation;)")
            .unwrap_or_else(|| panic!("{name}: `t` never calls the suspending `s()`:\n{d}"));
        assert_eq!(
            body.matches("Method p:()").count(),
            1,
            "{name}: the receiver `p()` must be evaluated exactly once:\n{d}"
        );
        assert_eq!(
            body.matches("Method s:(Lkotlin/coroutines/Continuation;)")
                .count(),
            1,
            "{name}: the argument `s()` must be evaluated exactly once:\n{d}"
        );
        assert!(
            recv < arg,
            "{name}: the receiver `p()` must be evaluated BEFORE the suspending argument `s()` \
             (it needs a spill slot to survive the suspension):\n{d}"
        );
    }
}

const EXTENSION_GUARDS_SOURCE: &str = "class H\n\
    class Host {\n\
    \x20 fun H.member(a: String, b: String?, c: Int): String = a\n\
    }\n\
    fun H.ext(a: String, b: String?, c: Int): String = a\n\
    fun H?.optional(a: String): String = a\n\
    fun box() = \"OK\"\n";

const EXTENSION_GUARD_SHAPES_SOURCE: &str = "class H\n\
    fun H.ext(a: String, b: String?, c: Int): String = a\n\
    fun H?.optional(a: String): String = a\n\
    private fun H.hidden(a: String): String = a\n\
    fun <T> T.generic(a: String): String = a\n\
    fun <T : Any> T.bounded(a: String): String = a\n\
    fun box() = \"OK\"\n";

#[test]
fn extension_function_null_checks_its_receiver_and_params() {
    // kotlinc guards an EXTENSION's receiver with `checkNotNullParameter(…, "<this>")` and its
    // non-null reference value parameters just like a plain function's — a nullable receiver, a
    // nullable parameter, and a primitive get none. The receiver's `LocalVariableTable` entry is
    // named `$this$<function>` (not the parameter's own name).
    let Some((dir, jh)) = krusty_compile("extrecvnull", EXTENSION_GUARD_SHAPES_SOURCE) else {
        return;
    };
    // `-v` so the receiver's `LocalVariableTable` name is visible alongside the guards.
    let text = common::javap(&["-v", "-p", &dir.join("BKt.class").to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = &jh;
    let _ = std::fs::remove_dir_all(&dir);
    let method = |name: &str| {
        let start = text.find(name).unwrap_or_else(|| panic!("{name}:\n{text}"));
        let rest = &text[start..];
        rest[..rest[1..]
            .find("public static")
            .map(|i| i + 1)
            .unwrap_or(rest.len())]
            .to_string()
    };
    let ext = method(" ext(");
    assert!(
        ext.contains("// String <this>"),
        "an extension guards its receiver as `<this>`:\n{ext}"
    );
    assert_eq!(
        ext.matches("checkNotNullParameter").count(),
        2,
        "exactly the receiver and the non-null `a` are guarded — not `b: String?`, not `c: Int`:\n{ext}"
    );
    assert!(
        ext.contains("$this$ext"),
        "the receiver's LocalVariableTable name is `$this$<function>`:\n{ext}"
    );
    let optional = method(" optional(");
    assert_eq!(
        optional.matches("checkNotNullParameter").count(),
        1,
        "a NULLABLE receiver is not guarded; only the non-null parameter is:\n{optional}"
    );
    let hidden = method(" hidden(");
    assert_eq!(
        hidden.matches("checkNotNullParameter").count(),
        0,
        "a PRIVATE extension has no entry guards:\n{hidden}"
    );
    let generic = method(" generic(");
    assert_eq!(
        generic.matches("checkNotNullParameter").count(),
        1,
        "an unbounded type-parameter receiver admits null; only the value parameter is guarded:\n{generic}"
    );
    let bounded = method(" bounded(");
    assert_eq!(
        bounded.matches("checkNotNullParameter").count(),
        2,
        "an Any-bounded receiver and its non-null value parameter are guarded:\n{bounded}"
    );
}

#[test]
fn extension_guard_fixture_is_byte_identical_to_kotlinc() {
    for class in ["H", "Host", "ExtensionReceiverGuardsKt"] {
        match common::byte_diff_against_kotlinc(
            "ExtensionReceiverGuards",
            EXTENSION_GUARDS_SOURCE,
            class,
        ) {
            None => panic!("extension guard parity: reference toolchain unavailable"),
            Some(Ok(())) => {}
            Some(Err(error)) => panic!("{error}"),
        }
    }
}

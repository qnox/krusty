//! The relational (`<`, `<=`, `>`, `>=`) lowering seam: every non-builtin relation is emitted from
//! the ONE `compareTo` target the checker recorded in `resolved_operator_calls`, which is the only
//! map `ir_lower`'s relational arm reads. A target recorded anywhere else types the comparison
//! `Boolean` while lowering finds nothing and falls through to the primitive `if_icmp*` — a class
//! file that compiles and then fails to load with `VerifyError: Bad type on operand stack`.
//!
//! Two shapes miss the selected-target block's `Ty::Obj` clause: `String`, which is a `Ty` of its own
//! and so has to be admitted explicitly, and a source `enum class`, whose `compareTo` is INHERITED
//! from `java.lang.Enum` and reported by no lookup on the enum itself. The enum shape only
//! misses when kotlin-stdlib is absent from the classpath — with the stdlib present the enum's
//! `Comparable` supertype carries the member, so the ordinary selected-target block finds it — hence
//! the no-stdlib variants below, which are what actually exercise the enum fallback.
//!
//! The operands are runtime values (locals fed by a function parameter), so nothing is const-folded
//! away before lowering; the assertions therefore pin the emitted call, not the checker's opinion.

use super::common;

/// A source `enum class` and two `String`s, compared with all four relations in both outcomes.
const RELATIONS_SRC: &str = "enum class Color { RED, GREEN, BLUE }\n\
     fun pick(i: Int): Color = if (i == 0) Color.RED else if (i == 1) Color.GREEN else Color.BLUE\n\
     fun s(i: Int): String = if (i == 0) \"apple\" else \"banana\"\n\
     fun box(): String {\n\
     val r = pick(0)\n\
     val g = pick(1)\n\
     val b = pick(2)\n\
     if (!(r < g)) return \"e lt\"\n\
     if (!(b > g)) return \"e gt\"\n\
     if (!(r <= r)) return \"e le\"\n\
     if (!(b >= g)) return \"e ge\"\n\
     if (g < r) return \"e not lt\"\n\
     if (g > b) return \"e not gt\"\n\
     if (b <= g) return \"e not le\"\n\
     if (r >= g) return \"e not ge\"\n\
     val x = s(0)\n\
     val y = s(1)\n\
     if (!(x < y)) return \"s lt\"\n\
     if (!(y > x)) return \"s gt\"\n\
     if (!(x <= x)) return \"s le\"\n\
     if (!(y >= x)) return \"s ge\"\n\
     if (y < x) return \"s not lt\"\n\
     if (x > y) return \"s not gt\"\n\
     if (y <= x) return \"s not le\"\n\
     if (x >= y) return \"s not ge\"\n\
     return \"OK\"\n\
     }\n";

#[test]
fn enum_and_string_relations_run_with_stdlib() {
    common::expect_box_ok_with_stdlib(RELATIONS_SRC, "Relations");
}

#[test]
fn enum_and_string_relations_run_without_stdlib() {
    // Without kotlin-stdlib the enum's `Comparable` supertype resolves from the builtins fallback and
    // carries no `compareTo`, so the enum relation takes the inherited-from-`java.lang.Enum`
    // fallback. That fallback used to record its target in `resolved_calls`, which lowering never
    // reads: `Color.RED < Color.BLUE` emitted `aload; aload; if_icmplt` on two references.
    //
    // Compiled with an empty classpath — that is what puts the front end on the builtins fallback —
    // but RUN with the stdlib, which an enum class needs for its `$ENTRIES` initializer.
    let jdk = common::jdk_modules();
    let classes = common::expect_compile_in_process(
        RELATIONS_SRC,
        "RelationsNoStdlib",
        &[],
        Some(jdk.as_path()),
    );
    let box_class = common::find_box_class(&classes).expect("the compiled classes declare `box()`");
    let out = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
        .expect("the JVM runner is provisioned, so `box()` must run");
    assert_eq!(out, "OK", "RelationsNoStdlib");
}

#[test]
fn relation_with_non_comparable_right_operand_is_rejected() {
    // `String.compareTo` takes a `String`. A former fallback arm resolved the relation through the
    // ERASED `Comparable.compareTo(Object)` whenever the right operand was any reference, so
    // `s < any` type-checked (kotlinc: "argument type mismatch: actual type is 'Any', but 'String'
    // was expected") and then lowered to a primitive comparison on two references.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(
        "fun box(): String {\n\
         val s = \"a\"\n\
         val o: Any = \"b\"\n\
         if (s < o) return \"x\"\n\
         return \"OK\"\n\
         }\n",
        &[stdlib],
        Some(jdk.as_path()),
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("operator cannot be applied to 'String' and 'Any'")),
        "`String < Any` must be rejected as an inapplicable operator, got {diagnostics:?}"
    );
}

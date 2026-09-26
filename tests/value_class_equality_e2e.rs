//! `==`/`!=` with a value class on the left are kotlinc's specialized calls.
//!
//! Two non-null operands of the class compare through `equals-impl0`, whose result is the
//! comparison itself. A boxed `V?`, or any other reference, on the right goes to `equals-impl`. A
//! nullable left operand is null-checked, and unboxed when `V?` is boxed. A nullable right operand
//! carried unboxed (`S?` over a `String`) is null-checked next. `!=` negates the same expression.
//! Two boxed operands, or the class only on the right, keep `areEqual`.

use super::common;

const EQUALITY: &str = r#"@JvmInline value class Z(val x: Int)
@JvmInline value class S(val x: String)
@JvmInline value class F(val x: Float)

fun mk(): Z = Z(1)
fun mks(): S? = null

fun eq(a: Z, b: Z) = a == b
fun ne(a: Z, b: Z) = a != b
fun nullableLeft(a: Z?, b: Z) = a == b
fun nullableRight(a: Z, b: Z?) = a == b
fun bothNullable(a: Z?, b: Z?) = a == b
fun againstAny(a: Z, b: Any?) = a == b
fun anyAgainst(a: Any?, b: Z) = a == b
fun unboxedNullableRight(a: S, b: S?) = a == b
fun unboxedNullableLeft(a: S?, b: S) = a == b
fun unboxedBothNullable(a: S?, b: S?) = a == b
fun unboxedNullableAgainstAny(a: S?, b: Any?) = a == b
fun nullableLeftNe(a: Z?, b: Z) = a != b
fun unboxedNullableRightNe(a: S, b: S?) = a != b
fun againstAnyNe(a: Z, b: Any?) = a != b
fun computedLeft(b: S?) = S(mks()!!.x) == b
fun computedBoth(b: Z) = (if (b.x > 0) b else null) == mk()
fun floats(a: F, b: F) = a == b
fun floatsNullable(a: F, b: F?) = a == b
fun branch(a: Z?, b: Z): String = if (a == b) "y" else "n"
"#;

const BOX: &str = r#"fun box(): String {
    if (!eq(Z(1), Z(1)) || ne(Z(1), Z(1))) return "eq"
    if (nullableLeft(null, Z(1)) || !nullableLeft(Z(1), Z(1))) return "nullableLeft"
    if (nullableRight(Z(1), null) || !nullableRight(Z(1), Z(1))) return "nullableRight"
    if (!bothNullable(null, null) || bothNullable(Z(1), null)) return "bothNullable"
    if (againstAny(Z(1), "1") || !againstAny(Z(1), Z(1))) return "againstAny"
    if (!anyAgainst(Z(2), Z(2))) return "anyAgainst"
    if (unboxedNullableRight(S("a"), null) || !unboxedNullableRight(S("a"), S("a"))) return "sr"
    if (unboxedNullableLeft(null, S("a")) || !unboxedNullableLeft(S("a"), S("a"))) return "sl"
    if (!unboxedBothNullable(null, null) || unboxedBothNullable(S("a"), null)) return "ss"
    if (!unboxedNullableAgainstAny(null, null) || unboxedNullableAgainstAny(S("a"), "a")) return "sa"
    if (!nullableLeftNe(null, Z(1)) || nullableLeftNe(Z(1), Z(1))) return "ne left"
    if (!unboxedNullableRightNe(S("a"), null)) return "ne right"
    if (againstAnyNe(Z(1), Z(1))) return "ne any"
    try {
        computedLeft(S("a"))
        return "computedLeft"
    } catch (e: NullPointerException) {}
    if (computedBoth(Z(0)) || !computedBoth(Z(1))) return "computedBoth"
    if (!floats(F(Float.NaN), F(Float.NaN)) || floats(F(0.0f), F(-0.0f))) return "floats"
    if (floatsNullable(F(1f), null)) return "floatsNullable"
    if (branch(Z(3), Z(3)) != "y" || branch(null, Z(3)) != "n") return "branch"
    return "OK"
}
"#;

/// The class disassembled verbosely without its constant pool, with pool indices masked.
fn disassembled(bytes: &[u8]) -> String {
    let work = common::scratch_dir().expect("a scratch directory");
    let path = work.join("Disassembled.class");
    std::fs::write(&path, bytes).expect("class file");
    let text = common::javap(&["-p", "-v", &path.to_string_lossy()]).expect("javap runs");
    let _ = std::fs::remove_dir_all(work);
    text.lines()
        .skip_while(|line| !line.starts_with("public") && !line.starts_with("final"))
        .filter(|line| !line.trim_start().starts_with('#') && !line.contains("Constant pool:"))
        .map(|line| {
            line.split_whitespace()
                .map(|token| if token.starts_with('#') { "#" } else { token })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything before the `@Metadata` annotation, which is the metadata writer's.
fn before_metadata(disassembly: &str) -> &str {
    disassembly
        .split_once("RuntimeVisibleAnnotations:")
        .map_or(disassembly, |(members, _)| members)
}

#[test]
fn every_comparison_is_kotlincs_specialized_call() {
    let pair = common::ModuleClassPair::compile(
        &[("Equality.kt", EQUALITY), ("Box.kt", BOX)],
        "EqualityKt",
    );
    assert_eq!(
        before_metadata(&disassembled(&pair.krusty)),
        before_metadata(&disassembled(&pair.kotlinc))
    );
}

#[test]
fn every_comparison_runs() {
    common::expect_box_ok_with_stdlib(&format!("{EQUALITY}\n{BOX}"), "ValueClassEquality");
}

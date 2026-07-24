//! Overload resolution must consider SPREADING trailing arguments into a `vararg` extension's packed
//! array. `"a.b.".trimEnd('.')` matches one `Char` against `CharArray`'s ELEMENT — nothing "fit" the
//! array parameter, so the resolver fell through to whichever same-name overload came first,
//! `trimEnd(predicate: (Char) -> Boolean)`, and reported the mis-pick as a bogus type error on the
//! argument ("inferred type is Char but Function was expected").
//!
//! The receiver-specificity half matters just as much: `String.trimEnd(vararg Char): String` and
//! `CharSequence.trimEnd(vararg Char): CharSequence` are both spreadable for a `String` receiver, and
//! picking the `CharSequence` one type-checks and then fails as "expected 'String', actual
//! 'kotlin/CharSequence'". These run on the JVM, so a wrong pick shows up as a wrong VALUE too.

use super::common;

#[test]
fn vararg_extension_spread_over_trailing_args_runs() {
    let src = "fun box(): String {\n\
val one: String = \"a.b.\".trimEnd('.')\n\
if (one != \"a.b\") return \"f1: \" + one\n\
val many: String = \"xxhixx\".trim('x')\n\
if (many != \"hi\") return \"f2: \" + many\n\
val several: String = \"ab..!!\".trimEnd('!', '.')\n\
if (several != \"ab\") return \"f3: \" + several\n\
val none: String = \"a.b\".trimEnd()\n\
if (none != \"a.b\") return \"f4: \" + none\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "VE");
}

/// The vararg candidate is tried only AFTER an exact fit, so the lambda overload still wins when the
/// argument actually is a predicate.
#[test]
fn non_vararg_overload_still_wins_on_exact_fit() {
    let src = "fun box(): String {\n\
val p: String = \"abcxx\".trimEnd { it == 'x' }\n\
if (p != \"abc\") return \"f1: \" + p\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "VF");
}

/// A NON-vararg extension whose last parameter merely IS an array (`Array<out T>?.contentEquals(other:
/// Array<out T>?)`) must pass its argument THROUGH. Keying the packing off the parameter shape instead
/// of the callee's own `vararg` flag wrapped the caller's array in a fresh 1-element array, so two equal
/// arrays compared unequal — a silent wrong ANSWER, not a crash.
#[test]
fn array_parameter_that_is_not_vararg_is_passed_through() {
    let src = "fun box(): String {\n\
val three: Array<Int> = arrayOf(1, 2, 3)\n\
if (!three.contentEquals(arrayOf(1, 2, 3))) return \"f1\"\n\
if (three.contentEquals(arrayOf(1, 2))) return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "VG");
}

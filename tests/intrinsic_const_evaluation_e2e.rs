//! `IntrinsicConstEvaluation`: which standard-library operations a `const val` initializer may
//! apply. Without the feature kotlinc folds only the `kotlin` package operators, conversions,
//! `toString`, `String.get`, `String.length` and `Char.code`, never on an unsigned receiver; with
//! it, every operation in its operation table folds, and so do `Enum.name` on an entry and
//! `KCallable.name` on a callable reference. An initializer with no value is reported where it
//! starts, as a read of a non-`const` `val` when that is why. Each fixture is compiled by both
//! compilers; the complete located diagnostics must be the expected ones for kotlinc and identical
//! for krusty, and an accepted fixture's class must be kotlinc's, `ConstantValue` attributes and
//! folded use sites included.

use super::common;

/// The fixture after its first line, which selects the language settings.
const INITIALIZERS: &str = "enum class E { A }
class K(val p: Int) { fun m() {} }
object O { val v = 3; const val w = v + 1 }
const val folded = \"abc\".length + 'a'.code + 7.floorDiv(2) + 1.plus(2) + \"abc\"[1].code
const val inc = 1.inc()
const val unsigned = 1u + 2u
const val trimmed = \"  x \".trim()
const val upper = \"Ab\".uppercase()
const val char = Char(65)
const val entry = E.A.name
const val property = K::p.name
const val constructor = ::K.name
const val equalUnsigned = 1u == 2u
const val divided = 1 / 0
const val called = O.toString()
";

const NOT_CONSTANT: &str = "const 'val' initializer must be a constant value.";

fn not_constant(position: &str) -> String {
    format!("Main.kt:{position}: {NOT_CONSTANT}")
}

fn non_const_val(position: &str) -> String {
    format!("Main.kt:{position}: only 'const val' can be used in constant expressions.")
}

/// Both compilers' complete errors for the one-file fixture `source`, both receiving the
/// fixture's `// LANGUAGE:` directive as arguments, must be `expected`.
fn assert_errors(source: &str, expected: &[String]) {
    let sources = [("Main.kt", source)];
    let arguments = common::language_directives::kotlinc_args(source);
    assert_eq!(
        common::reference_error_blocks(&sources, &arguments),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::krusty_error_blocks_with_args(&sources, &arguments),
        expected
    );
}

#[test]
fn without_the_feature_only_the_kotlin_operators_fold() {
    assert_errors(
        &format!("// default language settings\n{INITIALIZERS}"),
        &[
            non_const_val("4:37"),
            not_constant("6:17"),
            not_constant("7:22"),
            not_constant("8:21"),
            not_constant("9:19"),
            not_constant("10:18"),
            not_constant("11:19"),
            not_constant("12:22"),
            not_constant("13:25"),
            not_constant("14:27"),
            not_constant("15:21"),
            not_constant("16:20"),
        ],
    );
}

#[test]
fn with_the_feature_the_intrinsic_operations_fold() {
    assert_errors(
        &format!("// LANGUAGE: +IntrinsicConstEvaluation\n{INITIALIZERS}"),
        &[
            non_const_val("4:37"),
            not_constant("15:21"),
            not_constant("16:20"),
        ],
    );
}

/// Every folded initializer is the field's `ConstantValue`, and a read of it is the value.
#[test]
fn folded_initializers_are_kotlincs_constant_values() {
    const SRC: &str = r#"// LANGUAGE: +IntrinsicConstEvaluation
import kotlin.experimental.and
import kotlin.experimental.inv
enum class E { A, B }
class K(val p: Int) { fun m() {} }
object O { const val X = 5; const val S = "s" + X + 'c'; const val T = "  x  ".trim() }
const val length = "abc".length + 'a'.code + "abc"[1].code
const val chars = 'a' + 1
const val text = 1.toString() + 2.5f + 1e10 + 1e-5 + 'c' + true
const val floors = (-7).floorDiv(2) + (-7).mod(3) + 10L.mod(3) + 10.floorDiv(-3L)
const val increment = 1.inc() + 1u.inc().toInt()
const val unsignedSum = 1u + 2u
const val unsignedText = (-1).toULong().toString() + (ULong.MAX_VALUE / 3u)
const val unsignedWrap = 300.toUByte() + 1u
const val unsignedShift = 3u.shl(31)
const val unsignedEqual = 1u == 2u
const val trimmed = "  x ".trim() + "a\nb\r\n c".trimIndent()
const val margin = """
    |x
    |y""".trimMargin()
const val cases = "Ab".uppercase() + "straße".uppercase() + "ÀB".lowercase().length
const val char = Char(65)
const val bytes = 1.toByte().and(3.toByte()).inv()
const val entry = E.A.name + E.B.name
const val names = K::p.name + K::m.name + ::K.name
const val zero = 0.0 == -0.0
const val ordered = -0.0 < 0.0 && Double.NaN > 1.0
const val nan = Double.NaN == Double.NaN
const val promoted = 16777217.compareTo(16777216f)
const val truncated = 1.5.toInt() + 2.9f.toLong() + Double.NaN.toInt()
const val overflow = Long.MIN_VALUE / -1 + Int.MAX_VALUE + 1
const val remainder = 1.0f.rem(0.3f) + 5.0.mod(-3.0)
// Unsigned reads are left out: a template folds them only through kotlinc's IR constant folding.
fun reads() = "$length $chars $text $floors $increment ${O.S} ${O.T} $unsignedText $unsignedEqual " +
    "$trimmed $margin $cases $char $bytes $entry $names $zero $ordered $nan $promoted $truncated " +
    "$overflow $remainder"
"#;
    common::assert_class_code_matches_kotlinc(
        "IntrinsicConstValues",
        SRC,
        "IntrinsicConstValuesKt",
    );
    common::assert_class_code_matches_kotlinc("IntrinsicConstValues", SRC, "O");
}

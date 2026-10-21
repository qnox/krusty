//! Membership in a range built by the stdlib's `Comparable` `rangeTo` is compiled like kotlinc's
//! `RangeContainsLowering` with its `ComparableRangeToHandler`: `x in a..b` allocates no
//! `ClosedRange` and becomes `0 <= x.compareTo(a) && x.compareTo(b) <= 0` through
//! `Comparable.compareTo`. The value is cast to `Comparable` at each use unless its erasure already
//! is one, temporaries follow the numeric membership's side-effect ordering, `!in` and `when`
//! branches negate or branch on the materialized boolean, and a platform-typed end bound keeps its
//! null check while the start bound (the `rangeTo` receiver) does not.

use super::common;

const FUNCTIONS: &str = r#"import java.math.BigDecimal

fun text(s: String) = s in "a".."m"
fun notText(s: String) = s !in "a".."m"
fun branch(s: String): Int = when (s) {
    in "a".."m" -> 1
    in "n".."z" -> 2
    else -> 3
}
fun condition(s: String): String {
    if (s in "a".."m") {
        return "in"
    }
    return "out"
}
fun <T : Comparable<T>> generic(x: T, lo: T, hi: T) = x in lo..hi

class Version(val n: Int) : Comparable<Version> {
    override fun compareTo(other: Version): Int = n.compareTo(other.n)
}

fun user(v: Version) = v in Version(1)..Version(3)

var log = ""
fun pick(): String {
    log += "v"
    return "c"
}
fun lo(): String {
    log += "l"
    return "a"
}
fun hi(): String {
    log += "h"
    return "m"
}
fun valueCall() = pick() in "a".."m"
fun allCalls() = pick() in lo()..hi()
fun boundCalls(s: String) = s in lo()..hi()
fun lowCallOnly(s: String) = s in lo().."m"
fun highCallOnly(s: String) = s in "a"..hi()

inline fun <T : Comparable<T>> T.between(a: T, b: T) = this in a..b
fun inlined(s: String) = s.between("a", "m")

fun platform(d: BigDecimal) = d in BigDecimal.ONE..BigDecimal.TEN

fun local(): Boolean {
    var x = "q"
    x += "r"
    return x in "a".."z"
}
"#;

const BOX: &str = r#"
fun negatedBranch(s: String): Int = when (s) {
    !in "a".."m" -> 1
    else -> 2
}

fun box(): String {
    val results = listOf(
        text("c"), text("z"), notText("c"), branch("c") == 1, branch("p") == 2, branch("~") == 3,
        condition("z") == "out", generic(2, 1, 3), generic(4, 1, 3), user(Version(2)),
        user(Version(0)), valueCall(), allCalls(), boundCalls("~"), lowCallOnly("b"),
        highCallOnly("z"), inlined("c"), platform(BigDecimal("5")), platform(BigDecimal("11")),
        local(), negatedBranch("p") == 1, negatedBranch("b") == 2,
    )
    return if (results == listOf(
            true, false, false, true, true, true, true, true, false, true, false, true, true,
            false, true, false, true, true, false, true, true, true,
        ) && log == "vlhvlhlh") "OK" else "$results $log"
}
"#;

#[test]
fn comparable_range_membership_is_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "ComparableRangeContains",
        FUNCTIONS,
        &["ComparableRangeContainsKt", "Version"],
    );
}

#[test]
fn comparable_range_membership_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(&format!("{FUNCTIONS}{BOX}"), "comparable_range_contains");
}

//! Stepped integer progressions in `for`-loops: single `step`, chained `step … step …`, `downTo`/
//! `until`/`reversed` combined with `step`, and a stored progression re-stepped. The progression's
//! `last` element is recomputed by the stdlib (`getProgressionLastElement`) for each `step`, so the
//! iterated values match kotlinc exactly. Round-tripped on the JVM under `-Xverify:all`.

use super::common;

#[test]
fn stepped_progressions_run() {
    // Each case's expected sum is computed from the exact progression the stdlib produces.
    const SRC: &str = "fun box(): String {\n\
var a = 0; for (i in 0..6 step 2) a += i\n\
if (a != 12) return \"a\"\n\
var b = 0; for (i in 0..6 step 2 step 3) b += i\n\
if (b != 9) return \"b\"\n\
var c = 0; for (i in 0 until 6 step 2 step 3) c += i\n\
if (c != 3) return \"c\"\n\
var d = 0; for (i in 6 downTo 0 step 2) d += i\n\
if (d != 12) return \"d\"\n\
var e = 0; for (i in 6 downTo 0 step 2 step 3) e += i\n\
if (e != 9) return \"e\"\n\
val p = 0..6\n\
var f = 0; for (i in p step 3) f += i\n\
if (f != 9) return \"f\"\n\
var g = 0L; for (i in 0L..6L step 2L step 3L) g += i\n\
if (g != 9L) return \"g\"\n\
var h = \"\"; for (c in 'a'..'g' step 2) h += c\n\
if (h != \"aceg\") return \"h\"\n\
var k = \"\"; for (c in 'g' downTo 'a' step 2) k += c\n\
if (k != \"geca\") return \"k\"\n\
var m = 0u; for (i in 0u..6u step 2) m = m + i\n\
if (m != 12u) return \"m\"\n\
var n = 0u; for (i in 0u..6u step 2 step 3) n = n + i\n\
if (n != 9u) return \"n\"\n\
var o = 0u; for (i in 1u until 5u step 1) o = o + i\n\
if (o != 10u) return \"o\"\n\
var q = 0uL; for (i in 1uL until 5uL step 1L) q = q + i\n\
if (q != 10uL) return \"q\"\n\
val ur = 1u..<3u\n\
if (!ur.contains(1u) || ur.contains(3u)) return \"ur\"\n\
val ulr = 1uL..<3uL\n\
if (!ulr.contains(1uL) || ulr.contains(3uL)) return \"ulr\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "R");
}

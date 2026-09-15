//! `f(*array)` — a spread argument, through krusty's own code generator.
//!
//! A `vararg` call normally builds its array from elements the generator can count, so every one of
//! them has a constant offset. A spread breaks that: `f(a, *xs, b)` is as long as `xs` is, and
//! nothing static knows how long that is. The length is summed at run time, and the elements are
//! placed at a running index rather than at a constant — a spread by copying its elements in, the
//! rest one at a time.
//!
//! The copy is also what keeps the callee's array its own: a `vararg` parameter that shared storage
//! with the caller's array would let a write reach back through it.

use super::common::expect_native_box;

#[test]
fn a_spread_passes_the_elements_of_its_array() {
    expect_native_box(
        "fun join(vararg parts: String): String {\n\
         \x20   var out = \"\"\n\
         \x20   for (part in parts) out += part\n\
         \x20   return out\n\
         }\n\
         fun box(): String {\n\
         \x20   val middle = arrayOf(\"b\", \"c\")\n\
         \x20   if (join(*middle) != \"bc\") return \"fail alone\"\n\
         \x20   if (join(\"a\", *middle) != \"abc\") return \"fail before\"\n\
         \x20   if (join(*middle, \"d\") != \"bcd\") return \"fail after\"\n\
         \x20   if (join(\"a\", *middle, \"d\") != \"abcd\") return \"fail both\"\n\
         \x20   if (join(*middle, *middle) != \"bcbc\") return \"fail twice\"\n\
         \x20   if (join(*arrayOf<String>()) != \"\") return \"fail empty\"\n\
         \x20   if (join(\"a\", *arrayOf<String>(), \"d\") != \"ad\") return \"fail empty between\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "SpreadArgument",
        "OK",
    );
}

#[test]
fn a_spread_of_primitives_keeps_its_element_width() {
    // An `IntArray`'s elements are four bytes apart and a `String` array's are eight. The running
    // index is in ELEMENTS, so it is the stride that turns it into an address — and a spread copies
    // by that same stride rather than by a pointer's width.
    expect_native_box(
        "fun total(vararg values: Int): Int {\n\
         \x20   var sum = 0\n\
         \x20   for (value in values) sum += value\n\
         \x20   return sum\n\
         }\n\
         fun box(): String {\n\
         \x20   val some = intArrayOf(2, 3)\n\
         \x20   if (total(*some) != 5) return \"fail alone: \" + total(*some)\n\
         \x20   if (total(1, *some, 4) != 10) return \"fail mixed: \" + total(1, *some, 4)\n\
         \x20   val longs = longArrayOf(1L, 2L)\n\
         \x20   if (totalLong(*longs, 3L) != 6L) return \"fail long\"\n\
         \x20   return \"OK\"\n\
         }\n\
         fun totalLong(vararg values: Long): Long {\n\
         \x20   var sum = 0L\n\
         \x20   for (value in values) sum += value\n\
         \x20   return sum\n\
         }\n",
        "SpreadPrimitives",
        "OK",
    );
}

#[test]
fn a_spread_copies_rather_than_sharing() {
    // The callee's `vararg` array is its own. Writing through it must not reach the array the
    // caller spread — which a spread that passed the array itself would allow.
    expect_native_box(
        "fun clobber(vararg values: Int): Int {\n\
         \x20   values[0] = 99\n\
         \x20   return values[0]\n\
         }\n\
         fun box(): String {\n\
         \x20   val mine = intArrayOf(1, 2)\n\
         \x20   if (clobber(*mine) != 99) return \"fail callee\"\n\
         \x20   return if (mine[0] == 1) \"OK\" else \"fail shared: \" + mine[0]\n\
         }\n",
        "SpreadCopies",
        "OK",
    );
}

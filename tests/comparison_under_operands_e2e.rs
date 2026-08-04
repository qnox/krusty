//! Comparisons emitted in VALUE position while other operands are already live on the operand stack
//! — an `arrayOf`/vararg element (`[arrayref, arrayref, index]` held across the element), an
//! `Array.set` value (`[arrayref, index]`), a `SpreadBuilder.add` argument. Each comparison branches
//! and merges, so it records stack-map frames, and those frames must type the FULL stack the verifier
//! sees — the held operands included — not just the comparison's own 0/1 result.
//!
//! A frame that omits the held operands still produces a class file (the emitter never fails), so
//! these must ROUND-TRIP on a real JVM: `expect_box_ok_with_stdlib` runs under `-Xverify:all`, where
//! a short frame is rejected with "Inconsistent stackmap frames at branch target N".
//!
//! The relations `<`/`<=`/`>`/`>=` are not exercised as vararg elements: lowering still declines a
//! branchy element on the vararg-pack paths (`is_branchy` in `ir_lower`), so those shapes skip rather
//! than emit. `==`/`!=` is listed there too but only fires when the LHS is syntactically a primitive
//! literal, which is why `listOf(x == y, …)` over parameters reaches emit at all. The arms that DO
//! reach emit — `==`/`!=` (numeric, reference and null) and `===`/`!==` — all share the same
//! frame-recording code, so they cover it.
//!
//! The last test covers the other half of the rule: an operand that must NOT be held (a `try`, whose
//! handler clears the operand stack) spills to a temp where the position allows it.

use super::common;

#[test]
fn vararg_elements_are_comparisons() {
    // Two+ elements: a one-argument `listOf(x)` binds the non-vararg overload, which packs no array at
    // all and so never held anything on the stack — the reported repro needs two.
    common::expect_box_ok_with_stdlib(
        "fun f(x: Int, y: Int) = listOf(x == y, x != y)\n\
         fun box(): String {\n\
         if (f(1, 1).toString() != \"[true, false]\") return \"eq=${f(1, 1)}\"\n\
         if (f(1, 2).toString() != \"[false, true]\") return \"ne=${f(1, 2)}\"\n\
         return \"OK\"\n\
         }\n",
        "CmpUnderVararg",
    );
}

#[test]
fn vararg_elements_cover_every_comparison_arm() {
    // The referential (`if_acmp*`), null (`ifnull`), Long (`lcmp`) and Double (`dcmpg`) arms each
    // record their own merge frames, so each must see the held array too.
    common::expect_box_ok_with_stdlib(
        "fun f(a: Any?, b: Any?, n: Long, d: Double): List<Any> =\n\
         listOf(a === b, a !== b, a == null, b != null, n == 0L, d == 0.5, 1)\n\
         fun box(): String {\n\
         val s = f(null, \"x\", 0L, 1.0).toString()\n\
         val want = \"[false, true, true, true, true, false, 1]\"\n\
         return if (s == want) \"OK\" else \"got=$s\"\n\
         }\n",
        "CmpUnderVarargArms",
    );
}

#[test]
fn array_literal_and_array_set_elements_are_comparisons() {
    // `arrayOf(...)` builds the array with `dup; index; <element>; aastore`, and `a[i] = <cmp>` holds
    // `[arrayref, index]` across the value — both are element stores under live operands.
    common::expect_box_ok_with_stdlib(
        "fun f(x: Int, y: Int): String {\n\
         val a = arrayOf(x == y, x != y)\n\
         val b = BooleanArray(2)\n\
         b[0] = x == y\n\
         b[1] = x != y\n\
         val c = Array(2) { false }\n\
         c[0] = x != y\n\
         c[1] = x == y\n\
         return a.toList().toString() + b.toList().toString() + c.toList().toString()\n\
         }\n\
         fun box(): String {\n\
         val s = f(1, 2)\n\
         val want = \"[false, true][false, true][true, false]\"\n\
         return if (s == want) \"OK\" else \"got=$s\"\n\
         }\n",
        "CmpUnderArrayStore",
    );
}

#[test]
fn primitive_spread_builder_elements_are_comparisons() {
    // A spread (`*xs`) over a PRIMITIVE vararg routes the call through `PrimitiveSpreadBuilder`
    // (`BooleanSpreadBuilder`/`IntSpreadBuilder`), whose `add`/`addSpread` arguments are emitted with
    // the builder held on the stack.
    common::expect_box_ok_with_stdlib(
        "fun flags(vararg fs: Boolean): String = fs.toList().toString()\n\
         fun ints(vararg xs: Int): String = xs.toList().toString()\n\
         fun box(): String {\n\
         val x = 1\n\
         val y = 2\n\
         val rest = booleanArrayOf(true)\n\
         val more = intArrayOf(7)\n\
         val s = flags(x == y, *rest, x != y) + ints(if (x == y) 1 else 2, *more)\n\
         return if (s == \"[false, true, true][2, 7]\") \"OK\" else \"got=$s\"\n\
         }\n",
        "CmpUnderSpread",
    );
}

#[test]
fn reference_spread_builder_elements_are_comparisons() {
    // A spread over a REFERENCE vararg takes the other branch — `kotlin/jvm/internal/SpreadBuilder`
    // with `add(Object)`/`addSpread(Object)` and a `toArray` + `checkcast` tail. Same held
    // `[builder, builder]`, separate emission path, so it needs its own case.
    common::expect_box_ok_with_stdlib(
        "fun sel(b: Boolean): String = if (b) \"t\" else \"f\"\n\
         fun strs(vararg ss: String): String = ss.toList().toString()\n\
         fun f(x: Int, y: Int): String {\n\
         val rest = arrayOf(\"r\")\n\
         return strs(sel(x == y), *rest, sel(x != y))\n\
         }\n\
         fun box(): String {\n\
         val s = f(1, 2)\n\
         return if (s == \"[f, r, t]\") \"OK\" else \"got=$s\"\n\
         }\n",
        "CmpUnderRefSpread",
    );
}

#[test]
fn array_subscript_spills_a_try_instead_of_holding_the_array() {
    // A `try` can't be held under: its handler CLEARS the operand stack, so no frame can describe the
    // array (the handler frame would claim `[array, index, Throwable]` where the JVM has `[Throwable]`
    // — `VerifyError: Stack map does not match the one at exception handler N`). `Array.get`/`.set`
    // start from an empty stack, so they spill to temps rather than hold. Both the index and the value
    // position, and both a primitive and a reference array.
    common::expect_box_ok_with_stdlib(
        "fun g(n: Int): Int = if (n == 0) throw RuntimeException(\"x\") else n\n\
         fun f(n: Int): String {\n\
         val b = BooleanArray(2)\n\
         b[0] = try { g(n) == 1 } catch (e: Exception) { false }\n\
         b[try { g(n) } catch (e: Exception) { 1 }] = true\n\
         val a = arrayOf(\"p\", \"q\")\n\
         a[try { g(n) - 1 } catch (e: Exception) { 0 }] = try { \"v\" + g(n) } catch (e: Exception) { \"c\" }\n\
         return b.toList().toString() + a.toList().toString()\n\
         }\n\
         fun box(): String {\n\
         val ok = f(1)\n\
         if (ok != \"[true, true][v1, q]\") return \"ok=$ok\"\n\
         val thrown = f(0)\n\
         if (thrown != \"[false, true][c, q]\") return \"thrown=$thrown\"\n\
         return \"OK\"\n\
         }\n",
        "TryUnderArrayStore",
    );
}

//! A discarded `if`/`when` joins its value when kotlinc types it, and discards it once.
//!
//! kotlinc's `visitWhen` discards each branch's value in the branch only for a `when` that is not
//! exhaustive or whose type is `Unit`. Any other one, a statement included, materializes every
//! branch at its type and the statement pops the joined value; `PopBackwardPropagation` then drops
//! that `pop` only where the branches' pushes are cheap. fir2ir types a `when` or `if` that ends in
//! an `else if` chain without a final `else` as `Unit`.
//!
//! krusty discarded each branch's value in the branch, so `if (c) next(x) else twice(x)` as a
//! statement had a `pop` in each branch where kotlinc has one after the join.
use super::common;

/// Each statement's branches push through calls, which `PopBackwardPropagation` keeps, so the
/// `pop` after the join stays; `openChain` is not exhaustive and `mixed` joins an `Int` with a
/// `String` as `Any`.
const SOURCE: &str = "package store\n\
    \n\
    fun next(x: Int): Int = x + 1\n\
    fun twice(x: Int): Int = x * 2\n\
    fun wide(x: Long): Long = x + 1L\n\
    fun name(x: Int): String = if (x > 0) \"positive\" else \"other\"\n\
    fun touch(x: Int) {}\n\
    \n\
    fun both(c: Boolean, x: Int) {\n\
    \x20   if (c) next(x) else twice(x)\n\
    \x20   touch(x)\n\
    }\n\
    fun longs(c: Boolean, x: Long) {\n\
    \x20   if (c) wide(x) else x\n\
    \x20   touch(0)\n\
    }\n\
    fun mixed(c: Boolean, x: Int) {\n\
    \x20   if (c) next(x) else name(x)\n\
    \x20   touch(x)\n\
    }\n\
    fun chain(x: Int) {\n\
    \x20   if (x > 2) next(x) else if (x > 1) twice(x) else name(x)\n\
    \x20   touch(x)\n\
    }\n\
    fun openChain(x: Int) {\n\
    \x20   if (x > 2) next(x) else if (x > 1) twice(x)\n\
    \x20   touch(x)\n\
    }\n\
    fun switch(x: Int) {\n\
    \x20   when (x) {\n\
    \x20       1 -> next(x)\n\
    \x20       2 -> twice(x)\n\
    \x20       else -> x\n\
    \x20   }\n\
    \x20   touch(x)\n\
    }\n";

#[test]
fn a_discarded_typed_if_or_when_pops_its_joined_value_like_kotlinc() {
    common::byte_diff_against_kotlinc("DiscardedValue", SOURCE, "store/DiscardedValueKt")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|difference| panic!("{difference}"));
}

/// A `when` over a sealed class needs no `else` to be exhaustive: it joins its value too, after
/// the no-match failure kotlinc adds as its last branch. Only the instructions are compared: the
/// line kotlinc gives an `is` condition is a separate difference.
const SEALED: &str = "package store\n\
    \n\
    sealed class Shape\n\
    class Square(val side: Int) : Shape()\n\
    class Circle(val radius: Int) : Shape()\n\
    \n\
    fun next(x: Int): Int = x + 1\n\
    fun twice(x: Int): Int = x * 2\n\
    fun touch(x: Int) {}\n\
    \n\
    fun shapes(s: Shape) {\n\
    \x20   when (s) {\n\
    \x20       is Square -> next(s.side)\n\
    \x20       is Circle -> twice(s.radius)\n\
    \x20   }\n\
    \x20   touch(0)\n\
    }\n";

#[test]
fn a_discarded_exhaustive_when_without_else_pops_its_joined_value_like_kotlinc() {
    common::method_code_diff_against_kotlinc(
        "DiscardedSealed",
        &[],
        SEALED,
        "store/DiscardedSealedKt",
        "public static final void shapes(store.Shape)",
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|difference| panic!("{difference}"));
}

/// A `when` over a sealed class is exhaustive without an `else` even when every branch is `Unit`:
/// kotlinc still adds the no-match failure as its last branch, and discards each branch's value in
/// the branch. Only the instructions are compared, as for `shapes`.
const UNIT_BRANCHES: &str = "package store\n\
    \n\
    sealed class Shape\n\
    class Square(val side: Int) : Shape()\n\
    class Circle(val radius: Int) : Shape()\n\
    \n\
    fun touch(x: Int) {}\n\
    \n\
    fun shapes(s: Shape) {\n\
    \x20   when (s) {\n\
    \x20       is Square -> touch(s.side)\n\
    \x20       is Circle -> {}\n\
    \x20   }\n\
    \x20   touch(0)\n\
    }\n";

#[test]
fn a_unit_valued_exhaustive_when_without_else_keeps_its_no_match_failure_like_kotlinc() {
    common::method_code_diff_against_kotlinc(
        "UnitBranches",
        &[],
        UNIT_BRANCHES,
        "store/UnitBranchesKt",
        "public static final void shapes(store.Shape)",
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|difference| panic!("{difference}"));
}

//! Unreachable code does not change a block's TYPE.
//!
//! `val a = "a".let { throw Error(); it + "a" }` is `String` to kotlinc — the `throw` makes the rest
//! unreachable, which Kotlin reports as a warning, not as a different type. Typing the block
//! `Nothing` because a statement diverged made the declaration untypeable, and since a property of
//! type `Nothing` is rejected outright, the whole file was refused.
use super::common;

#[test]
fn a_diverging_statement_does_not_retype_the_block() {
    const SRC: &str = "val a1 = \"a\".let {\n\
        \x20   throw Error()\n\
        \x20   it + \"a\"\n\
        }\n\
        fun box(): String = \"OK\"\n";
    common::expect_front_end_ok_files_with_stdlib(&[SRC], "UnreachableTrailing");
}

#[test]
fn a_block_that_yields_nothing_is_still_nothing() {
    // The converse: with no trailing value to fall back on, a diverging body still has no type, so
    // the property is still rejected — as kotlinc rejects it.
    const SRC: &str = "val a2: String = run { throw Error() }\n\
        fun box(): String = \"OK\"\n";
    common::expect_front_end_ok_files_with_stdlib(&[SRC], "DivergingBlockAnnotated");
}

#[test]
fn a_statement_block_after_a_return_still_diverges() {
    // The converse of the rule above, and why it is restricted to VALUE position: a block that
    // begins with `return` transfers control and produces nothing, so typing it by its trailing
    // STATEMENT would say it falls through — which hands the dead code to lowering.
    const SRC: &str = concat!(
        "fun box(): String {\n",
        "    try {\n",
        "        return \"OK\"\n",
        "        if (1 == 1) { val z = 2 }\n",
        "        if (3 == 3) { val z = 4 }\n",
        "    } finally {\n",
        "    }\n",
        "}\n",
    );
    common::expect_box_ok_with_stdlib(SRC, "DeadCodeAfterReturn");
}

#[test]
fn a_member_return_reading_an_undetermined_companion_property() {
    // A return the pre-inference pass cannot determine is not an answer: publishing the marker makes
    // "not resolved yet" look like a type, and it sticks — the later pass finds the return already
    // set. `fun res() = res` reads the private companion property while that property is still being
    // resolved, and the declaration was then rejected with "expected 'String', actual
    // '<not determined>'".
    //
    // Front end only: running this shape hits a separate defect in how a PRIVATE companion's
    // property is accessed at runtime, which predates this rule and is not what it fixes.
    const SRC: &str = concat!(
        "class Test {\n",
        "    private companion object { val res = \"OK\" }\n",
        "    fun res() = res\n",
        "}\n",
        "fun box(): String = Test().res()\n",
    );
    common::expect_front_end_ok_files_with_stdlib(&[SRC], "UndeterminedCompanionReturn");
}

//! `super.f(…)` where the base is, or sits above, a CLASSPATH class. Two facts the checker has to get
//! right before the lowerer can emit anything: which declaration the call targets when the superclass
//! chain leaves source, and what the target's result type is once the base's type arguments are bound.

use super::common;

#[test]
fn super_call_reaches_a_source_base_above_a_classpath_class() {
    // `Mid` is source, but its own base is a library class. The source-hierarchy walk used to discard
    // everything it had collected the moment it reached that library class, so `Mid.tag` read as absent
    // and the call was rejected outright ("unresolved super method").
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        open class Mid : ArrayList<Int>() {
            open fun tag(): String = "mid"
        }

        class Leaf : Mid() {
            override fun tag(): String = super.tag() + "-leaf"
        }

        fun box(): String = Leaf().tag()
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "mid-leaf");
}

#[test]
fn super_call_binds_the_bases_type_arguments() {
    // A generic library member's return is `E`. Resolving against the bare classifier left nothing to
    // bind it to, so the result erased to `Any` and any use of it failed to type-check. The base is
    // `ArrayList<Int>`, so `get`/`set` return `Int` — and the descriptor still says `Object`, which the
    // lowerer must unbox.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        class My : ArrayList<Int>() {
            fun firstPlus(): Int = super.get(0) + 1
            fun swapped(v: Int): Int = super.set(0, v) * 10
        }

        fun box(): String {
            val m = My()
            m.add(7)
            return "${m.firstPlus()} ${m.swapped(3)}"
        }
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "8 70");
}

#[test]
fn super_call_binds_a_reference_type_argument() {
    // The same recovery with a reference element: the erased `Object` narrows by `checkcast`, not by
    // unboxing, so `.length` on the result resolves.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        class Names : ArrayList<String>() {
            fun widthOfFirst(): Int = super.get(0).length
        }

        fun box(): String {
            val n = Names()
            n.add("abcd")
            return n.widthOfFirst().toString()
        }
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "4");
}

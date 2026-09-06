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

// The four `super` dispatch shapes, all through checked FIR. `super` is not a receiver expression:
// the checker fixes one supertype declaration and dispatch must stay non-virtual, so the call
// carries its own FIR target rather than a module/dependency callable id (which would re-resolve to
// the override and recurse forever).
#[test]
fn super_dispatch_shapes_run() {
    const CLASS_SUPER: &str = "open class B { open fun f(): String = \"O\" }\n\
        class D : B() { override fun f(): String = super.f() + \"K\" }\n\
        fun box(): String = D().f()\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(CLASS_SUPER, "Main").expect("class super"),
        "OK"
    );

    const INTERFACE_SUPER: &str = "interface I { fun f(): String = \"O\" }\n\
        class D : I { override fun f(): String = super.f() + \"K\" }\n\
        fun box(): String = D().f()\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(INTERFACE_SUPER, "Main").expect("interface super"),
        "OK"
    );

    const QUALIFIED_SUPER: &str = "interface I { fun f(): String = \"O\" }\n\
        open class B { open fun f(): String = \"X\" }\n\
        class D : B(), I { override fun f(): String = super<I>.f() + \"K\" }\n\
        fun box(): String = D().f()\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(QUALIFIED_SUPER, "Main").expect("qualified super"),
        "OK"
    );

    const DEPENDENCY_SUPER: &str = "class N : ArrayList<Any>() {\n\
        \x20 override fun add(el: Any): Boolean = super.add(el)\n\
        }\n\
        fun box(): String { val n = N(); n.add(\"x\"); return if (n.size == 1) \"OK\" else \"Fail\" }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(DEPENDENCY_SUPER, "Main").expect("dependency super"),
        "OK"
    );
}

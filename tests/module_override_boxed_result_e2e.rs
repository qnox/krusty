//! A primitive override of a declaration whose result is not primitive returns the wrapper
//! wherever its class is declared: a public top-level `Next : Echo<Int>` declares
//! `echo(I)Ljava/lang/Integer;` exactly as a local class does. Every file of the module sees the
//! same choice, so a caller in another file reads the wrapper (and unboxes it where it wants the
//! `Int`), a subclass in another file overrides the wrapper-returning method and reaches it with a
//! `super` call, and nothing bridges between the two. A value class's member is realized as its
//! static `-impl`, which returns the wrapper too, in its own file and in a caller's. A lambda
//! converted to a fun interface whose method returns the wrapper is a class, not an indy.

use super::common;

const ECHO: &str = "interface Echo<T> { fun echo(x: T): T }\n\
    open class Next : Echo<Int> { override fun echo(x: Int): Int = x + 1 }\n\
    @JvmInline value class Shift(val by: Int) : Echo<Int> { override fun echo(x: Int): Int = x + by }\n\
    fun interface Count : Echo<Int> { override fun echo(x: Int): Int }\n\
    fun counted(): Int = Count { it + 3 }.echo(1)\n";

const CALLERS: &str =
    "class Last : Next() { override fun echo(x: Int): Int = super.echo(x) + 10 }\n\
    fun direct(next: Next): Int = next.echo(1) * 2\n\
    fun discarded(next: Next) { next.echo(1) }\n\
    fun reference(next: Next): Any = next.echo(1)\n\
    fun shifted(shift: Shift): Int = shift.echo(1)\n\
    fun countedElsewhere(): Int = Count { it * 3 }.echo(2)\n\
    fun box(): String {\n\
    \x20   if (direct(Next()) != 4) return \"direct\"\n\
    \x20   val last: Echo<Int> = Last()\n\
    \x20   if (last.echo(1) != 12) return \"bridge\"\n\
    \x20   if (reference(Last()) != 12) return \"reference\"\n\
    \x20   if (shifted(Shift(2)) != 3) return \"value class\"\n\
    \x20   if (counted() != 4) return \"fun interface\"\n\
    \x20   if (countedElsewhere() != 6) return \"fun interface elsewhere\"\n\
    \x20   discarded(Next())\n\
    \x20   return \"OK\"\n\
    }\n";

const SOURCES: [(&str, &str); 2] = [("Echo.kt", ECHO), ("Callers.kt", CALLERS)];

/// `method` of `class` as each compiler emits it, constant-pool indices erased.
fn assert_same_instructions(class: &str, method: &str) {
    let pair = common::ModuleClassPair::compile(&SOURCES, class);
    let disassemble = |bytes: &[u8]| {
        let work = common::scratch_dir().expect("a scratch directory for disassembly");
        let path = work.join(format!("{class}.class"));
        std::fs::write(&path, bytes).expect("write the class for disassembly");
        let text = common::javap(&["-v", "-c", "-p", &path.to_string_lossy()]).expect("javap runs");
        let _ = std::fs::remove_dir_all(work);
        common::method_instructions(&text, method)
    };
    let reference = disassemble(&pair.kotlinc);
    assert!(!reference.is_empty(), "kotlinc emits {class}.{method}");
    assert_eq!(disassemble(&pair.krusty), reference, "{class}.{method}");
}

#[test]
fn a_public_override_returns_the_wrapper_like_kotlinc() {
    assert_same_instructions("Next", "public java.lang.Integer echo(int);");
    assert_same_instructions("Next", "public java.lang.Object echo(java.lang.Object);");
}

#[test]
fn override_member_tables_match_kotlinc() {
    for class in ["Next", "Last", "Shift"] {
        let pair = common::ModuleClassPair::compile(&SOURCES, class);
        assert_eq!(
            common::member_table(&pair.krusty),
            common::member_table(&pair.kotlinc),
            "{class}"
        );
    }
}

#[test]
fn a_subclass_in_another_file_overrides_the_wrapper_like_kotlinc() {
    assert_same_instructions("Last", "public java.lang.Integer echo(int);");
}

#[test]
fn a_caller_in_another_file_unboxes_the_wrapper_like_kotlinc() {
    assert_same_instructions("CallersKt", "public static final int direct(Next);");
}

#[test]
fn a_discarded_call_pops_the_wrapper_like_kotlinc() {
    assert_same_instructions("CallersKt", "public static final void discarded(Next);");
}

#[test]
fn a_reference_consumer_keeps_the_wrapper_like_kotlinc() {
    assert_same_instructions(
        "CallersKt",
        "public static final java.lang.Object reference(Next);",
    );
}

#[test]
fn a_value_class_member_returns_the_wrapper_like_kotlinc() {
    assert_same_instructions(
        "Shift",
        "public static java.lang.Integer echo-impl(int, int);",
    );
    assert_same_instructions("CallersKt", "public static final int shifted-");
}

/// kotlinc converts a lambda to a fun interface whose method returns the wrapper through a class of
/// its own rather than `invokedynamic`, in the interface's file and in any other.
#[test]
fn a_fun_interface_with_a_boxed_result_converts_through_a_class_like_kotlinc() {
    assert_same_instructions("EchoKt", "public static final int counted();");
    assert_same_instructions("CallersKt", "public static final int countedElsewhere();");
}

#[test]
fn module_boxed_override_results_run() {
    common::expect_box_ok_files_with_stdlib(&SOURCES, "ModuleOverrideBoxedResult");
}

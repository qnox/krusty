//! A `when` subject read from an existing value is tested through a temporary, as in kotlinc.
//!
//! kotlinc binds every `when` subject to a temporary. It keeps one initialized from a variable read
//! for debugging (`JvmOptimizationLowering`), and its bytecode pass later folds a temporary that is
//! loaded only once, which is why a single test or a `tableswitch` reads the value directly. krusty
//! tested a stable local or parameter in place, so every `when (x)` with two or more tests lacked
//! kotlinc's `astore; aload` pair. The variable a `when (val v = e)` declares is still tested as it
//! is, in both compilers.
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "sealed interface Shape\n\
    class Circle(val r: Int) : Shape\n\
    class Square(val side: Int) : Shape\n\
    fun make(): Shape = Square(1)\n\
    fun twoTests(s: Shape): Int = when (s) {\n\
    \x20   is Circle -> s.r\n\
    \x20   is Square -> s.side\n\
    }\n\
    fun localSubject(x: Any): Int {\n\
    \x20   val s = x as Shape\n\
    \x20   return when (s) { is Circle -> 1; is Square -> 2 }\n\
    }\n\
    fun values(x: Any): Int = when (x) { 1 -> 1; \"s\" -> 2; else -> 3 }\n\
    fun oneTest(s: Shape): Int = when (s) { is Circle -> 1; else -> 2 }\n\
    fun table(n: Int): String = when (n) { 1 -> \"one\"; 2 -> \"two\"; else -> \"many\" }\n\
    fun bound(): Int = when (val s = make()) {\n\
    \x20   is Circle -> s.r\n\
    \x20   is Square -> s.side\n\
    }\n";

#[test]
fn a_when_subject_goes_through_a_temporary_like_kotlinc() {
    let Some(built) = compare_with_kotlinc_plugin(
        "WhenSubjectTemporary",
        SOURCE,
        "WhenSubjectTemporaryKt",
        &[],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "int twoTests(",
        "int localSubject(",
        "int values(",
        "int oneTest(",
        "java.lang.String table(",
        "int bound(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

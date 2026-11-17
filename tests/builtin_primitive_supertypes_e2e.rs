//! A mapped builtin's supertypes are its `.kotlin_builtins` declaration's, plus the
//! `java.io.Serializable` edge the JVM builtins customizer adds. The interfaces its JVM class
//! implements (`java.lang.Integer : Constable, ConstantDesc`) are not Kotlin supertypes.

use super::common;

#[test]
fn a_primitive_is_not_its_wrappers_java_interfaces() {
    let source = "\
import java.io.Serializable
import java.lang.constant.Constable
import java.lang.constant.ConstantDesc
fun constable(x: Int): Constable = x
fun desc(x: Long): ConstantDesc = x
fun enumConstable(x: AnnotationRetention): Constable = x
fun comparable(x: Int): Comparable<Int> = x
fun number(x: Double): Number = x
fun serializable(x: Char): Serializable = x
";
    let sources = [("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:4:36: return type mismatch: expected 'Constable', actual 'Int'.",
            "test.kt:5:35: return type mismatch: expected 'ConstantDesc', actual 'Long'.",
            "test.kt:6:56: return type mismatch: expected 'Constable', actual 'AnnotationRetention'.",
        ]
    );
}

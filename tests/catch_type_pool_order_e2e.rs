//! ASM's `MethodWriter` visits a method's try-catch blocks before its instructions, so kotlinc's
//! pool holds a method's catch types ahead of every constant its code introduces.
use super::common;

#[test]
fn a_method_interns_its_catch_types_before_its_code() {
    let source = "class Payload\n\
                  fun <T> Any?.castTo(): T = this as T\n\
                  fun probe(): Int {\n\
                  \x20   try {\n\
                  \x20       val value = Payload().castTo<Int?>()\n\
                  \x20       return if (value == null) 1 else 2\n\
                  \x20   } catch (e: ClassCastException) {\n\
                  \x20       return 3\n\
                  \x20   }\n\
                  }\n";
    let compared = common::compile_with_kotlinc("CatchOrder", source, &[], &["CatchOrderKt"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "CatchOrderKt differs from kotlinc");
}

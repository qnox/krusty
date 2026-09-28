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

/// The empty string is also in the facade's `@Metadata`, which kotlinc writes after the methods:
/// the constant still follows the catch type, where the code first names it.
#[test]
fn a_constant_the_metadata_also_names_follows_the_catch_types() {
    let source = "class Failure : RuntimeException()\n\
                  fun label(): String {\n\
                  \x20   var text = \"\"\n\
                  \x20   try {\n\
                  \x20       throw Failure()\n\
                  \x20   } catch (e: Failure) {\n\
                  \x20   } finally {\n\
                  \x20       text += \"done\"\n\
                  \x20   }\n\
                  \x20   return text\n\
                  }\n";
    let compared = common::compile_with_kotlinc("CatchLabel", source, &[], &["CatchLabelKt"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "CatchLabelKt differs from kotlinc");
}

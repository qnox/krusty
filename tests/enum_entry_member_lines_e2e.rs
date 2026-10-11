//! An enum entry's body is the `Enum$ENTRY` subclass, and its members are declarations with lines
//! of their own. kotlinc returns from a `Unit` member on its closing `}` line and anchors an accessor
//! on its own header. krusty never attached lines to an entry body's members, so a `{}` method had
//! no `LineNumberTable` and a block body's implicit `return` had no line.
use super::common;

#[test]
fn an_enum_entry_bodys_members_carry_their_declaration_lines() {
    let src = "package entries\n\
               \n\
               enum class Mode(val str: String = \"OK\") {\n\
               \x20   ON {\n\
               \x20       fun empty() {}\n\
               \x20       fun block() {\n\
               \x20           println()\n\
               \x20       }\n\
               \x20       override fun label(): String {\n\
               \x20           return \"on\"\n\
               \x20       }\n\
               \x20       val computed: Int\n\
               \x20           get() {\n\
               \x20               return 1\n\
               \x20           }\n\
               \x20   },\n\
               \x20   OFF;\n\
               \x20   open fun label(): String = \"\"\n\
               }\n\
               \n\
               fun box(): String =\n\
               \x20       Mode.ON.str\n";
    common::byte_diff_against_kotlinc_cp(
        "EnumEntryMemberLines",
        src,
        "entries/Mode$ON",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("entries/Mode$ON byte-identical to kotlinc");
}

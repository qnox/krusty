use super::common;

#[test]
fn classpath_member_defaults_follow_the_descriptor_overload() {
    let library = "package lib\n\
        class Api {\n\
        \x20 fun choose(value: Int): String = \"int\"\n\
        \x20 fun choose(value: Byte = 1): String = \"byte\"\n\
        }\n";
    let main = "import lib.Api\n\
        fun box(): String = if (Api().choose() == \"byte\") \"OK\" else \"fail\"\n";

    common::expect_box_ok_against_ref("membermetadataalignment", library, main);
}

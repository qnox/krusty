use super::common;

#[test]
fn reified_inline_return_inferred_from_expected() {
    const SRC: &str = "enum class Sample { FIRST, SECOND }\n\
        inline fun <reified T : Enum<T>> parse(s: String): T = enumValueOf<T>(s)\n\
        fun box(): String {\n\
            val value: Sample = parse(\"SECOND\")\n\
            return if (value == Sample.SECOND) \"OK\" else \"no\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "reified_inline_return_inferred");
}

#[test]
fn primitive_reified_return_inferred_from_expected() {
    const SRC: &str = "inline fun <reified T> cast(value: Any): T = value as T\n\
        fun box(): String {\n\
            val value: Int = cast(42)\n\
            return if (value == 42) \"OK\" else \"no\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "primitive_reified_return_inferred");
}

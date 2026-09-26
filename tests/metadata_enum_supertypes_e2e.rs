//! The supertypes an enum class's `@Metadata` lists, measured against kotlinc 2.4.20: the
//! interfaces the declaration names, in source order, then the implicit `kotlin.Enum<E>`.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn an_enum_lists_its_interfaces_before_the_implicit_enum_supertype() {
    let src = "package app\n\
        \n\
        interface Named\n\
        interface Holder<T>\n\
        \n\
        enum class Kind : Named, Holder<String> { ONE, TWO }\n";
    assert_identical("EnumSupertypes", src, "app/Kind");
}

#[test]
fn an_enum_without_interfaces_lists_only_the_enum_supertype() {
    let src = "package app\n\
        \n\
        enum class Plain { ONE }\n";
    assert_identical("EnumOnlySupertype", src, "app/Plain");
}

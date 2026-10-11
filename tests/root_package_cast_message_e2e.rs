//! `null cannot be cast to non-null type …` spells a root-package classifier `<root>.Name`.
//!
//! kotlinc's `TypeOperatorLowering` builds the message from `IrType.render()`, which writes a
//! declaration's package with `FqName.toString()`, and the root package reads `<root>`. So a class,
//! a nested class, and the owner of a type parameter declared in the root package all carry the
//! `<root>.` prefix. The source deliberately has no `package` directive: the root package is the
//! subject.
use super::common;

const SOURCE: &str = "class Box\n\
                      class Outer {\n\
                      \x20   class Inner\n\
                      \x20   fun <T : Any> member(a: Any?): T = a as T\n\
                      }\n\
                      class Holder<T : Any>(val item: Any?) {\n\
                      \x20   fun held(): T = item as T\n\
                      }\n\
                      \n\
                      fun box(a: Any?): Box = a as Box\n\
                      fun inner(a: Any?): Outer.Inner = a as Outer.Inner\n\
                      fun generic(a: Any?): Holder<Box> = a as Holder<Box>\n\
                      fun <T : Any> top(a: Any?): T = a as T\n";

#[test]
fn root_package_cast_targets_render_with_the_root_prefix_like_kotlinc() {
    for class in ["RootPackageCastMessageKt", "Outer", "Holder"] {
        common::byte_diff_against_kotlinc_cp(
            "RootPackageCastMessage",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

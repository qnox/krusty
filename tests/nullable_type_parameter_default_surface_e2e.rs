//! A `T?` position of a `-jvm-default` compatibility member carries `@Nullable`, as kotlinc
//! writes it, while a bare `T` stays unannotated: `A$DefaultImpls.f(A, Object, Object)` keeps
//! `@Nullable` on its `x: T?` parameter and its `T?` result.

use super::common;

const SOURCE: &str = "\
interface A<T> {\n\
    fun f(x: T?, y: T): T? = null\n\
}\n\
";

#[test]
fn nullable_type_parameter_positions_keep_nullable_on_default_holders() {
    for class in ["A", "A$DefaultImpls"] {
        let Some(result) = common::byte_diff_against_kotlinc_cp(
            "NullableTypeParameterDefaultSurface",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        ) else {
            eprintln!("skip: kotlinc unavailable for {class}");
            return;
        };
        assert_eq!(result, Ok(()), "{class} must be byte-identical to kotlinc");
    }
}

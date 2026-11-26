//! A `T?` position of a `-jvm-default` compatibility member carries `@Nullable`, as kotlinc
//! writes it, while a bare `T` stays unannotated: `A$DefaultImpls.f(A, Object, Object)` keeps
//! `@Nullable` on its `x: T?` parameter and its `T?` result. A bare `T` whose bound excludes null
//! (`<T : Any>`) is `@NotNull`, and one whose bound admits null stays unannotated.

use super::common;

const SOURCE: &str = "\
interface A<T> {\n\
    fun f(x: T?, y: T): T? = null\n\
}\n\
\n\
interface B<T : Any> {\n\
    fun g(x: T, y: T?): T = x\n\
}\n\
\n\
interface C<T : CharSequence?> {\n\
    fun h(x: T, y: T?): T = x\n\
}\n\
";

#[test]
fn nullable_type_parameter_positions_keep_nullable_on_default_holders() {
    for class in [
        "A",
        "A$DefaultImpls",
        "B",
        "B$DefaultImpls",
        "C",
        "C$DefaultImpls",
    ] {
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

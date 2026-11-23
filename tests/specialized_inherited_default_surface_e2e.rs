//! A sub-interface republishes an inherited default member as its applied supertype substitutes
//! it, as kotlinc does: `interface C<T> : A<T, Int>` writes `C$DefaultImpls.foo(C, Object, int)`
//! with its generic `Signature`, and its `access$foo$jd` bridge takes the same shape, boxing the
//! `int` and casting it to the erased `Number` slot of `A.foo` it calls.

use super::common;

const SOURCE: &str = "\
interface Base<T, U : Number> {\n\
    fun foo(t: T, u: U): String = \"base\"\n\
}\n\
\n\
interface Narrowed<T> : Base<T, Int>\n\
\n\
interface Closed : Base<String, Long>\n\
";

#[test]
fn sub_interfaces_republish_inherited_defaults_with_substituted_shapes() {
    for class in [
        "Narrowed",
        "Narrowed$DefaultImpls",
        "Closed",
        "Closed$DefaultImpls",
    ] {
        let Some(result) = common::byte_diff_against_kotlinc_cp(
            "SpecializedInheritedDefaultSurface",
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

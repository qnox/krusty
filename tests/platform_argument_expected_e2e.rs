//! A platform value (`T!`) passed as an argument is checked for null only when the parameter the
//! callee DECLARES excludes null. kotlinc reads the parameter before the call's own type arguments
//! apply: `same("x", sb.toString())` binds `T := String` yet passes the platform value unchecked,
//! because `T`'s bound admits null, as does `same<String>(…)`. A class's type argument is part of
//! the member's declared signature (`Box<String>().put(…)` is checked), and a bound or
//! definitely-non-null parameter (`T : Any`, `T & Any`) is checked. The callees are this
//! repository's own, compiled by kotlinc.

use super::common;

const LIB: &str = r#"
package lib

fun <T> same(expected: T, actual: T): Boolean = expected == actual

fun <T : Any> present(value: T): Int = 1

fun <T> definitely(value: T & Any): Int = 2

fun <T : CharSequence?> text(value: T): Int = 3

class Box<T> {
    fun <R> put(value: T, extra: R): Int = 4
}
"#;

const MAIN: &str = r#"
import lib.*

val sb = StringBuilder("x")

fun inferred(): Boolean = same("x", sb.toString())

fun explicit(): Boolean = same<String>("x", sb.toString())

fun bounded(): Int = present(sb.toString())

fun nonNull(): Int = definitely(sb.toString())

fun nullableBound(): Int = text(sb.toString())

fun member(): Int = Box<String>().put(sb.toString(), sb.toString())
"#;

#[test]
fn a_platform_argument_is_checked_against_the_declared_parameter() {
    let library =
        common::kotlinc_lib_out(&[("Lib.kt", LIB)]).expect("the reference kotlinc is provisioned");
    let pair = common::ModuleClassPair::compile_with_classpath(
        &[("PlatformArgument.kt", MAIN)],
        &[library],
        "PlatformArgumentKt",
    );
    for method in [
        "inferred",
        "explicit",
        "bounded",
        "nonNull",
        "nullableBound",
        "member",
    ] {
        let (kotlinc, krusty) = pair.method_code("PlatformArgumentKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

#[test]
fn unchecked_platform_arguments_run_like_the_reference_compiler() {
    let main = format!(
        "{MAIN}\n\
         fun box(): String {{\n\
         \x20   if (!inferred() || !explicit()) return \"same\"\n\
         \x20   if (bounded() + nonNull() + nullableBound() + member() != 10) return \"sum\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    let output = common::expect_box_run_against_ref("platform_argument_expected", LIB, &main)
        .expect("the reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

//! Type-parameter arity is a Kotlin overload shape. Equal erased JVM signatures are a later
//! platform declaration clash, compared with kotlinc's complete diagnostic.

use super::common;

#[test]
fn different_erased_returns_stay_independent_overloads() {
    common::assert_accepted_like_kotlinc(
        "fun <T> labeled(): Int = 1\n\
         fun labeled() {}\n",
    );
}

#[test]
fn equal_erasure_is_a_platform_declaration_clash() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "Main.kt",
            "fun <T> id(): Int = 1\n\
             fun id(): Int = 2\n",
        )],
        &[],
    );
}

#[test]
fn erased_generic_arguments_keep_kotlinc_declaration_order() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "Main.kt",
            "class Box<T>\n\
             fun f(x: Box<String>): Int = 1\n\
             fun <T> f(x: Box<T>): Int = 2\n",
        )],
        &[],
    );
}

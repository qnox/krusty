//! An `out`-projected type parameter argument, against kotlinc.
//!
//! `Box<A> <: Box<out E>` holds only when `A <: E`. A type parameter `E` in scope is a fixed type
//! there, never a variable the actual argument may bind, so `Box<String>` is not a `Box<out E>`.

use super::common;

#[test]
fn a_concrete_argument_is_not_an_out_projected_type_parameter() {
    const SRC: &str = "class Box<T>(val t: T)\n\
fun <E> fixed(s: Box<String>): Box<out E> = s\n\
fun <E : Number> bounded(s: Box<Int>): Box<out E> = s\n\
fun <E> local(s: Box<String>) { val b: Box<out E> = s }\n\
fun <E> array(s: Array<String>): Array<out E> = s\n\
fun <E> same(s: Box<E>): Box<out E> = s\n\
fun <E> nothing(s: Box<Nothing>): Box<out E> = s\n\
fun <E> wider(s: Box<out E>): Box<out Any?> = s\n\
fun <E : Number, F : E> lower(s: Box<F>): Box<out E> = s\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Main.kt:2:45: return type mismatch: expected 'Box<out E (of fun <E> fixed)>', \
             actual 'Box<String>'.",
            "Main.kt:3:53: return type mismatch: expected 'Box<out E (of fun <E : Number> \
             bounded)>', actual 'Box<Int>'.",
            "Main.kt:4:51: initializer type mismatch: expected 'Box<out E (of fun <E> local)>', \
             actual 'Box<String>'.",
            "Main.kt:5:49: return type mismatch: expected 'Array<out E (of fun <E> array)>', \
             actual 'Array<String>'.",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

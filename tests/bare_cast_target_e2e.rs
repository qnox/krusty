//! Cast targets written without type arguments, against kotlinc.

use super::common;

#[test]
fn a_bare_cast_target_takes_the_operand_arguments() {
    const SRC: &str = "open class Gen<T>(val v: T)\n\
class Auto<T>(v: T) : Gen<T>(v)\n\
fun b(g: Gen<String>): Auto<Int>? = g as? Auto\n\
fun c(g: Gen<*>): Auto<String> = g as Auto\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Main.kt:3:37: return type mismatch: expected 'Auto<Int>?', actual 'Auto<String>?'.",
            "Main.kt:4:34: return type mismatch: expected 'Auto<String>', actual 'Auto<*>'.",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn an_alias_with_a_fixed_application_keeps_its_arguments() {
    const SRC: &str = "interface Gen<T>\n\
interface Auto<T> : Gen<T>\n\
typealias IntAuto = Auto<Int>\n\
fun wrong(g: Gen<String>): Auto<String>? = g as? IntAuto\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        ["Main.kt:4:44: return type mismatch: expected 'Auto<String>?', actual 'Auto<Int>?'."]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

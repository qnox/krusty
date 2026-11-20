//! What an argument proves for the code after its call. `take(x!!)` leaves `x` non-null, and so
//! does `receiver.take(x!!)`: kotlinc types the receiver and the arguments of a member call as part
//! of the same data flow as the statements around it. A safe call is the exception, because its
//! arguments run only when the receiver is non-null.

use super::common;

fn assert_errors_match_kotlinc(source: &str, errors: &[&str]) {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Argument.kt", source)],
        &[common::stdlib_jar()],
        &[],
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty_errors = common::compiler_errors(&krusty_output);
    let kotlinc_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    let rendered = kotlinc_errors
        .iter()
        .map(|error| format!("{}:{}: {}", error.line, error.column, error.message))
        .collect::<Vec<_>>();
    assert_eq!(rendered, errors);
    assert_eq!(result.krusty_code == 0, errors.is_empty());
    assert_eq!(result.reference_code == 0, errors.is_empty());
}

#[test]
fn a_non_null_assertion_in_a_member_call_argument_proves_the_value_afterwards() {
    assert_errors_match_kotlinc(
        "\
class Item(val n: Int)
class Sink { fun take(item: Item): Int = 1 }
fun find(): Item? = null
fun use(sink: Sink): Int {
    val item = find()
    sink.take(item!!)
    return item.n
}
",
        &[],
    );
}

#[test]
fn a_safe_call_argument_proves_nothing_afterwards() {
    assert_errors_match_kotlinc(
        "\
class Item(val n: Int)
class Sink { fun take(item: Item): Int = 1 }
fun find(): Item? = null
fun use(sink: Sink?): Int {
    val item = find()
    sink?.take(item!!)
    return item.n
}
",
        &["7:16: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'."],
    );
}

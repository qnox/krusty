//! A lifted lambda or local function takes its captures in the order its body first uses them,
//! the dispatch receiver among the captured values: `prefix + count` in a member captures `this`
//! first, `count + prefix` captures `count` first.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

const SOURCE: &str = r#"
suspend fun step(s: String): String = s
fun <T> launchIt(block: suspend (Int) -> T): Any = block
fun <T> plainIt(block: (Int) -> T): Any = block
class Spec(private val prefix: String) {
    fun receiverFirst(count: Int) = launchIt { n -> step(prefix + n + count) }
    fun valueFirst(count: Int) = launchIt { n -> step("" + count + n + prefix) }
    fun between(count: Int, other: String) = launchIt { n -> step(other + prefix + n + count) }
    fun plain(count: Int) = plainIt { n -> prefix + n + count }
    fun local(count: Int): String {
        fun loc(n: Int) = prefix + n + count
        return loc(1)
    }
    fun shared(count: Int): String {
        var total = count
        fun bump(n: Int) { total += n + prefix.length }
        bump(1)
        val r = plainIt { n -> prefix + total + n }
        return r.toString() + total
    }
}
"#;

/// The indy lambdas and local functions lifted into the class, and their call sites.
#[test]
fn lifted_callables_take_the_receiver_where_the_body_first_uses_it() {
    byte_identical("capture_first_use_order", SOURCE, "Spec");
}

/// A suspend lambda's class declares its captures, and its constructor takes them, in that order.
#[test]
fn suspend_lambda_classes_take_the_receiver_where_the_body_first_uses_it() {
    for class in [
        "Spec$receiverFirst$1",
        "Spec$valueFirst$1",
        "Spec$between$1",
    ] {
        byte_identical("capture_first_use_order", SOURCE, class);
    }
}

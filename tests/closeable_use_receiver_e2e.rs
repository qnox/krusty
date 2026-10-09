//! `Closeable.use` stores its type-parameter receiver as `Closeable`.
//!
//! The checkcast is the store of that local. A function argument that is already a local is
//! invoked in place. `invoke` leaves erased `Object`, stored with no preceding null, and the
//! call site narrows that value after `finally`.

use super::common;

const SOURCE: &str = r#"import java.io.Closeable

class Res(val n: Int) : Closeable {
    override fun close() {}
}

fun plain(r: Res, body: (Res) -> String): String = r.use(body)
fun typed(r: Closeable, body: (Closeable) -> String): String = r.use(body)
fun unit(r: Res, body: (Res) -> Unit) { r.use(body) }
fun num(r: Res, body: (Res) -> Int): Int = r.use(body)
fun pass(r: Res, body: (Res) -> String): Int = r.use(body).length
"#;

#[test]
fn closeable_use_stores_the_erased_receiver_and_narrows_after_finally() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "CloseableUseReceiver",
        SOURCE,
        &["CloseableUseReceiverKt", "Res"],
    );
}

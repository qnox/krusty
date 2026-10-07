//! Inlined generic close helper: erased close, generic `Unit`, same-file SMAP, sorted handlers.
//!
//! `T.consumeAndClose` stores its receiver as the erased bound. A method-return receiver is not known
//! non-null after the checkcast, so each `this?.close()` is `dup; ifnull`. A discarded `R = Unit`
//! still stores `kotlin.Unit.INSTANCE` before `finally` and does not reload it. The same file's
//! expansion writes a source map, and only the method that actually inlined sorts its exception
//! table.

use super::common;

const DISCARDED: &str = r#"import java.io.BufferedReader
import java.io.StringReader

class Holder {
    fun bufferedReader(): BufferedReader = BufferedReader(StringReader(""))
}

inline fun <T : AutoCloseable?, R> T.consumeAndClose(block: (T) -> R): R {
    var closed = false
    try {
        return block(this)
    } catch (e: Exception) {
        closed = true
        try {
            this?.close()
        } catch (closeException: Exception) {
        }
        throw e
    } finally {
        if (!closed) {
            this?.close()
        }
    }
}

fun load(holder: Holder): Int {
    var n = 0
    holder.bufferedReader().consumeAndClose { r ->
        n += r.read()
    }
    return n
}

fun box(): String {
    val n = load(Holder())
    return if (n == -1) "OK" else "F:$n"
}
"#;

const USED: &str = r#"import java.io.BufferedReader
import java.io.StringReader

class Holder {
    fun bufferedReader(): BufferedReader = BufferedReader(StringReader("ab"))
}

inline fun <T : AutoCloseable?, R> T.consumeAndClose(block: (T) -> R): R {
    var closed = false
    try {
        return block(this)
    } catch (e: Exception) {
        closed = true
        try {
            this?.close()
        } catch (closeException: Exception) {
        }
        throw e
    } finally {
        if (!closed) {
            this?.close()
        }
    }
}

fun load(holder: Holder): Int {
    val n = holder.bufferedReader().consumeAndClose { r -> r.read() }
    return n
}

fun box(): String {
    val n = load(Holder())
    return if (n == 'a'.code) "OK" else "F:$n"
}
"#;

#[test]
fn discarded_generic_use_is_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk("UseClose", DISCARDED, &["UseCloseKt"]);
}

#[test]
fn used_generic_use_is_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk("UseClose", USED, &["UseCloseKt"]);
}

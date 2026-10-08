//! Inlined generic close helper: erased close, generic `Unit`, same-file SMAP, sorted handlers.
//!
//! `T.consumeAndClose` stores its receiver as the erased bound. A method-return receiver is not known
//! non-null after the checkcast, so each `this?.close()` is `dup; ifnull`. A discarded `R = Unit`
//! still stores `kotlin.Unit.INSTANCE` before `finally` and does not reload it. The same file's
//! expansion writes a source map, and only the method that actually inlined sorts its exception
//! table.

use super::common;

const LIBRARY: &str = r#"package neutral

interface Gate { fun close() }

class Reader(private val result: Int) : Gate {
    fun read(): Int = result
    override fun close() {}
}

class Holder(private val result: Int) {
    fun reader(): Reader = Reader(result)
}
"#;

const DISCARDED: &str = r#"import neutral.Gate
import neutral.Holder

inline fun <T : Gate?, R> T.consumeAndClose(block: (T) -> R): R {
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
    holder.reader().consumeAndClose { r ->
        n += r.read()
    }
    return n
}

fun box(): String {
    val n = load(Holder(-1))
    return if (n == -1) "OK" else "F:$n"
}
"#;

const USED: &str = r#"import neutral.Gate
import neutral.Holder

inline fun <T : Gate?, R> T.consumeAndClose(block: (T) -> R): R {
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
    val n = holder.reader().consumeAndClose { r -> r.read() }
    return n
}

fun box(): String {
    val n = load(Holder(97))
    return if (n == 97) "OK" else "F:$n"
}
"#;

#[test]
fn discarded_generic_use_is_byte_identical_to_kotlinc() {
    let library = common::kotlinc_lib_out(&[("NeutralGate.kt", LIBRARY)])
        .expect("reference kotlinc builds the neutral generic-bound dependency");
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "UseClose",
        DISCARDED,
        &["UseCloseKt"],
        &[library],
    );
}

#[test]
fn used_generic_use_is_byte_identical_to_kotlinc() {
    let library = common::kotlinc_lib_out(&[("NeutralGate.kt", LIBRARY)])
        .expect("reference kotlinc builds the neutral generic-bound dependency");
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "UseClose",
        USED,
        &["UseCloseKt"],
        &[library],
    );
}

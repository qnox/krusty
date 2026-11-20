use super::common;

const LIBRARY: &str = r#"
package lib

import java.io.Closeable

class Reader : Closeable {
    var closed = false
    fun readAll(into: StringBuilder) { into.append("OK") }
    override fun close() { closed = true }
}

abstract class FileSystem {
    abstract fun open(path: String): Reader

    inline fun <T> read(path: String, action: Reader.() -> T): T =
        open(path).use { it.action() }

    inline fun <T> run(action: () -> T): T = action()

    companion object {
        val SYSTEM: FileSystem = object : FileSystem() {
            override fun open(path: String) = Reader()
        }
    }
}
"#;

#[test]
fn a_public_inline_member_returns_non_locally_from_its_lambda() {
    common::expect_box_ok_against(
        "public_inline_member_non_local_return",
        LIBRARY,
        r#"
import lib.FileSystem

fun readBuffer(path: String): StringBuilder {
    FileSystem.SYSTEM.read(path) {
        val result = StringBuilder()
        readAll(result)
        return result
    }
}

fun box(): String = readBuffer("file").toString()
"#,
    );
}

#[test]
fn a_public_inline_member_returns_unit_non_locally_from_its_lambda() {
    common::expect_box_ok_against(
        "public_inline_member_unit_non_local_return",
        LIBRARY,
        r#"
import lib.FileSystem

var seen = ""

fun note(fs: FileSystem) {
    fs.run {
        seen = "OK"
        return
    }
}

fun box(): String {
    note(FileSystem.SYSTEM)
    return seen
}
"#,
    );
}

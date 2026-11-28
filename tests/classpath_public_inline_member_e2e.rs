//! A public inline member of a library class built by kotlinc, called from krusty-compiled code.
//! The portable programs are `tests/box_regressions/publicInlineMemberAcrossModules.kt` and
//! `tests/box_regressions/multimodule/publicInlineMemberNonLocalReturnAcrossModules.kt`.
//!
//! PLATFORM-SPECIFIC: these cases pin JVM facts the shared program cannot: a library compiled by
//! kotlinc to class files (with `java.io.Closeable`), and the synthetic accessor through which a
//! body copied into another package reads an inner class's package-private `this$0`.

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

#[test]
fn a_public_inline_member_reading_its_outer_instance_is_inlined_into_another_package() {
    common::expect_box_ok_against(
        "public_inline_member_outer_instance",
        r#"
package a

class E(val x: String) {
    inner class Inner {
        inline fun foo(y: String) = x + y
    }
}
"#,
        r#"
import a.*

fun box(): String = E("O").Inner().foo("K").let { if (it == "OK") it else "fail: $it" }
"#,
    );
}

const SUBCLASS_RECEIVER_LIBRARY: &str = r#"
package a

open class E(val x: String) {
    open inner class Base {
        inline fun foo(y: String) = x + y
    }

    inner class Sub : Base()
}
"#;

const SUBCLASS_RECEIVER_MAIN: &str = r#"
import a.*

fun box(): String = E("O").Sub().foo("K").let { if (it == "OK") it else "fail: $it" }
"#;

/// The owner of every call `class` makes to a method named `callee`.
fn call_owners(class: &[u8], callee: &str) -> Vec<String> {
    let bodies = krusty::jvm::classreader::ClassBodies::parse(std::sync::Arc::new(class.to_vec()))
        .expect("the compiled class parses");
    let methods = bodies
        .coded_methods()
        .map(|(name, descriptor)| (name.to_string(), descriptor.to_string()))
        .collect::<Vec<_>>();
    let mut owners = Vec::new();
    for (name, descriptor) in methods {
        let code = bodies
            .method_code(&name, &descriptor)
            .expect("a coded method has a body");
        let instructions =
            krusty::jvm::inline::disassemble(&code.code).expect("a coded method disassembles");
        for instruction in &instructions {
            if let Some((owner, method, _, _)) =
                krusty::jvm::inline::invoked_method(instruction, &code.source_cp)
            {
                if method == callee {
                    owners.push(owner.to_string());
                }
            }
        }
    }
    owners
}

#[test]
fn an_inherited_public_inline_member_reads_its_outer_instance_through_the_accessor() {
    // The inherited body reads the package-private `this$0` through `access$getThis$0$p`, as
    // kotlinc's does, so it is inlined into this package too: no call of `foo` remains, and the
    // copied body calls the accessor of the declaring class.
    common::expect_box_ok_against(
        "public_inline_member_subclass_receiver",
        SUBCLASS_RECEIVER_LIBRARY,
        SUBCLASS_RECEIVER_MAIN,
    );
    let build = common::compile_libs_build(
        "public_inline_member_subclass_receiver",
        &[("Lib.kt", SUBCLASS_RECEIVER_LIBRARY)],
    )
    .expect("the library compiles");
    let classpath = [build.krusty_out().to_path_buf(), common::stdlib_jar()];
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        SUBCLASS_RECEIVER_MAIN,
        "Main",
        &classpath,
        Some(jdk.as_path()),
    )
    .expect("the consumer compiles");
    let main = classes
        .iter()
        .find(|(name, _)| name == "MainKt")
        .map(|(_, bytes)| bytes)
        .expect("the consumer has a facade");
    assert_eq!(call_owners(main, "foo"), Vec::<String>::new());
    assert_eq!(call_owners(main, "access$getThis$0$p"), ["a/E$Base"]);
}

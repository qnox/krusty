//! A suspend method's continuation class is nested under its ENCLOSING class (`Svc$work$1`), matching
//! kotlinc — not under the file facade (`<File>Kt$work$1`). A top-level suspend fun's continuation
//! stays under the facade (`FooKt$foo$1`). Emitting the continuation under the wrong owner is an ABI
//! divergence (different class-name set than kotlinc).

use super::common;

fn class_names(src: &str) -> Vec<String> {
    let jdk = common::jdk_modules().expect("jdk modules");
    let sl = common::stdlib_jar().expect("stdlib jar");
    let coro = common::coroutines_jar().expect("coroutines jar");
    let classes = common::compile_in_process(src, "File", &[sl, coro, jdk.clone()], Some(&jdk))
        .unwrap_or_else(|| panic!("krusty failed to compile:\n{src}"));
    classes.into_iter().map(|(name, _)| name).collect()
}

#[test]
fn member_suspend_continuation_nested_under_class() {
    let names = class_names(
        "package demo\n\
         class Svc {\n\
             suspend fun work(x: Int): Int { val a = other(x); return a + 1 }\n\
             suspend fun other(x: Int): Int = x\n\
         }\n",
    );
    assert!(
        names.contains(&"demo/Svc$work$1".to_string()),
        "continuation should nest under the class: {names:?}",
    );
    assert!(
        !names.iter().any(|n| n.contains("FileKt$work$1")),
        "continuation must not nest under the file facade: {names:?}",
    );
}

#[test]
fn top_level_suspend_continuation_nested_under_facade() {
    let names = class_names(
        "package demo\n\
         suspend fun other(x: Int): Int = x\n\
         suspend fun work(x: Int): Int { val a = other(x); return a + 1 }\n",
    );
    assert!(
        names.contains(&"demo/FileKt$work$1".to_string()),
        "a top-level suspend fun's continuation nests under the facade: {names:?}",
    );
}

#[test]
fn mangled_method_continuation_uses_source_name() {
    // A value-class parameter makes kotlinc mangle the JVM method name (`create-<hash>`), but the
    // continuation class keeps the SOURCE name: `Svc$create$1`, not `Svc$create-<hash>$1`.
    let names = class_names(
        "package demo\n\
         @JvmInline value class Id(val v: String)\n\
         class Svc {\n\
             suspend fun create(id: Id, x: Int): Int { val a = other(x); return a + id.v.length }\n\
             suspend fun other(x: Int): Int = x\n\
         }\n",
    );
    assert!(
        names.contains(&"demo/Svc$create$1".to_string()),
        "continuation must use the unmangled source name: {names:?}",
    );
    assert!(
        !names.iter().any(|n| n.contains("Svc$create-")),
        "continuation must not carry the value-class mangle suffix: {names:?}",
    );
}

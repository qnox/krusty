//! Callable-reference classes carry the names kotlinc invents for them.
//!
//! kotlinc numbers lambdas, callable references and anonymous objects in one sequence per enclosing
//! name (`InventNamesForLocalClasses`). The expected names are the class files kotlinc 2.4.10
//! writes for the same sources.

use crate::common;

fn class_names(sources: &[(&str, &str)]) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        sources,
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("krusty compiles the source");
    let result = common::find_box_class(&classes)
        .and_then(|box_class| common::run_box(&classes, &box_class, std::slice::from_ref(&stdlib)));
    assert_eq!(result.as_deref(), Some("OK"));
    let mut names = classes
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !name.starts_with("META-INF/"))
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn callable_reference_classes_take_their_place_in_the_local_class_sequence() {
    let source = r#"
        package n
        class A(val p: Int) { fun m(x: Int) = x + p }
        fun take(x: Any?) = x
        var top = 1
        fun f() {}
        fun run(f: () -> Unit) = f()
        fun other(): Any { run { }; run(::f); return object {} }
        fun box(): String {
            take(A::m)
            take(A(1)::m)
            take({ 1 })
            take(::top)
            take(A::p)
            val r = String::length
            take(r)
            other()
            return "OK"
        }
    "#;
    assert_eq!(
        class_names(&[("Refs", source)]),
        [
            "n/A",
            "n/RefsKt",
            "n/RefsKt$box$1",
            "n/RefsKt$box$2",
            "n/RefsKt$box$4",
            "n/RefsKt$box$5",
            "n/RefsKt$box$r$1",
            "n/RefsKt$other$2",
            "n/RefsKt$other$3",
        ],
    );
}

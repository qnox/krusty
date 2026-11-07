//! A method type parameter bounded by a classifier type parameter erases through that
//! parameter's primary bound.
//!
//! `GraphQlTester.Entity<D, S : Entity<D, S>>.isEqualTo` is `<T : S> T`, so its JVM descriptor
//! returns `GraphQlTester.Entity`, and the override that tightens `T` to `EntityImpl` keeps a bridge
//! of that same descriptor beside the specialized method. The star-projected call shape is the
//! corpus regression. A method-local chain `<T : S, S : Mark>` and an inner class that names its
//! outer class's parameter erase the same way.

use super::common;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SRC: &str = r#"package app

open class Mark

class Marker<T>

interface GraphQlTester {
    interface Entity<D, S : Entity<D, S>> {
        fun <T : S> isEqualTo(expected: Any?): T
    }

    interface Path {
        fun <E : Any> entity(marker: Marker<E>): Entity<E, *>
    }
}

open class EntityImpl<D> : GraphQlTester.Entity<D, EntityImpl<D>> {
    override fun <T : EntityImpl<D>> isEqualTo(expected: Any?): T = this as T
}

class PathImpl : GraphQlTester.Path {
    override fun <E : Any> entity(marker: Marker<E>): GraphQlTester.Entity<E, *> = EntityImpl<E>()
}

fun <U : Any> GraphQlTester.Path.assertEqual(expected: U) {
    entity(Marker<U>()).isEqualTo(expected)
}

interface Chain {
    fun <T : S, S : Mark> take(): T
}

open class Outer<S : Mark> {
    inner class Inner {
        fun <T : S> read(value: T): T = value
    }
}

class Sample : Mark()

fun box(): String {
    val entity: GraphQlTester.Entity<Int, EntityImpl<Int>> = EntityImpl()
    val got: EntityImpl<Int> = entity.isEqualTo(1)
    if (got !== entity) return "interface"
    PathImpl().assertEqual(1)
    val sample = Sample()
    val read = Outer<Sample>().Inner().read(sample)
    if (read !== sample) return "inner"
    return "OK"
}
"#;

fn descriptors_named(dump: &str, method: &str) -> Vec<String> {
    let mut current = "";
    let mut found = Vec::new();
    for line in dump.lines() {
        let trimmed = line.trim();
        if let Some(descriptor) = trimmed.strip_prefix("descriptor: ") {
            if current.contains(method) {
                found.push(descriptor.to_string());
            }
        } else if trimmed.contains('(') {
            current = trimmed;
        }
    }
    found.sort();
    found
}

fn javap_class(root: &Path, class: &str) -> String {
    common::javap(&["-s", "-p", "-cp", &root.to_string_lossy(), class])
        .unwrap_or_else(|| panic!("javap failed for {class}"))
}

fn write_classes(root: &Path, classes: &[(String, Vec<u8>)]) {
    for (name, bytes) in classes {
        let path = root.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent().expect("class parent")).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

fn kotlinc_classes(dir: &Path, src: &Path) -> Option<PathBuf> {
    let out = dir.join("kotlinc");
    std::fs::create_dir_all(&out).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        src.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    Some(out)
}

#[test]
fn a_method_type_parameter_erases_through_its_class_bound() {
    let classes = common::compile_in_process_metadata_cp(SRC, "BoundErasure", &[])
        .expect("krusty failed to compile");
    let dir = common::scratch_dir().expect("scratch directory");
    let krusty = dir.join("krusty");
    write_classes(&krusty, &classes);
    let expected = HashMap::from([
        (
            "app/GraphQlTester$Entity",
            (
                "isEqualTo",
                vec!["(Ljava/lang/Object;)Lapp/GraphQlTester$Entity;".to_string()],
            ),
        ),
        (
            "app/EntityImpl",
            (
                "isEqualTo",
                vec![
                    "(Ljava/lang/Object;)Lapp/EntityImpl;".to_string(),
                    "(Ljava/lang/Object;)Lapp/GraphQlTester$Entity;".to_string(),
                ],
            ),
        ),
        ("app/Chain", ("take", vec!["()Lapp/Mark;".to_string()])),
        (
            "app/Outer$Inner",
            ("read", vec!["(Lapp/Mark;)Lapp/Mark;".to_string()]),
        ),
    ]);
    for (class, (method, descriptors)) in &expected {
        let dump = javap_class(&krusty, class);
        assert_eq!(
            descriptors_named(&dump, method),
            *descriptors,
            "{class}.{method}"
        );
    }

    let src = dir.join("BoundErasure.kt");
    std::fs::write(&src, SRC).unwrap();
    if let Some(kotlinc) = kotlinc_classes(&dir, &src) {
        for (class, (method, _)) in &expected {
            assert_eq!(
                descriptors_named(&javap_class(&krusty, class), method),
                descriptors_named(&javap_class(&kotlinc, class), method),
                "{class}.{method} disagreed with kotlinc"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn calling_through_the_erased_class_bound_returns_the_receiver() {
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "BoundErasureBox"),
        "OK"
    );
}

mod class_formals {
    //! A class type parameter erases to its primary bound in every physical position the class owns:
    //! the constructor parameter, the backing field, the getter, and the members of an inner class
    //! that capture it. For `class Holder<T> where T : Marker, T : Base` that bound is the class
    //! `Base`, although `Marker` is written first, exactly as kotlinc's `erasedUpperBound` chooses.
    //! Callers in another file, or in another module, then call those members with the same
    //! descriptors.
    use super::common;

    /// The reproduction from the issue, unchanged.
    const HOLDER: &str = "interface Marker\n\
                          open class Base\n\
                          class Holder<T>(val value: T) where T : Marker, T : Base {\n\
                          \x20   fun <R> echo(value: R): R where R : Marker, R : Base = value\n\
                          }\n";

    #[test]
    fn an_interface_first_class_formal_erases_to_its_class_bound_like_kotlinc() {
        let classes = common::classes_against_kotlinc_module(&[("Holder.kt", HOLDER)]);
        assert_eq!(classes.differences(), Vec::<String>::new());
    }

    const DECLARATIONS: &str = "package formals\n\
                                \n\
                                interface Marker\n\
                                \n\
                                open class Base\n\
                                \n\
                                class Both : Base(), Marker\n\
                                \n\
                                class Slots<T>(val value: T, var maybe: T?, val items: Array<T>) where T : Marker, T : Base {\n\
                                \x20   fun first(): T = items[0]\n\
                                \n\
                                \x20   fun <R> echo(value: R): R where R : Marker, R : Base = value\n\
                                \n\
                                \x20   fun replace(next: T?): T? {\n\
                                \x20       val old = maybe\n\
                                \x20       maybe = next\n\
                                \x20       return old\n\
                                \x20   }\n\
                                \n\
                                \x20   inner class Inner(val extra: T) {\n\
                                \x20       fun outer(): T = value\n\
                                \x20   }\n\
                                }\n";

    /// Each result reaches a `Both` parameter, so both compilers cast it from the erased carrier.
    const USES: &str = "package formals\n\
                        \n\
                        fun same(actual: Both?, expected: Both?): Boolean = actual === expected\n\
                        \n\
                        fun box(): String {\n\
                        \x20   val both = Both()\n\
                        \x20   val slots = Slots(both, null, arrayOf(both))\n\
                        \x20   if (!same(slots.value, both)) return \"value\"\n\
                        \x20   if (!same(slots.first(), both)) return \"first\"\n\
                        \x20   if (!same(slots.replace(both), null)) return \"replace\"\n\
                        \x20   if (!same(slots.maybe, both)) return \"maybe\"\n\
                        \x20   if (!same(slots.items[0], both)) return \"items\"\n\
                        \x20   if (!same(slots.echo(both), both)) return \"echo\"\n\
                        \x20   val inner = slots.Inner(both)\n\
                        \x20   if (!same(inner.extra, both)) return \"extra\"\n\
                        \x20   return if (same(inner.outer(), both)) \"OK\" else \"outer\"\n\
                        }\n";

    #[test]
    fn class_formal_slots_used_from_another_file_match_kotlinc() {
        let classes = common::classes_against_kotlinc_module(&[
            ("Declarations.kt", DECLARATIONS),
            ("Uses.kt", USES),
        ]);
        assert_eq!(classes.differences(), Vec::<String>::new());
    }

    #[test]
    fn class_formal_slots_used_from_another_file_run() {
        let result = common::compile_and_run_box_files(
            &[("Declarations.kt", DECLARATIONS), ("Uses.kt", USES)],
            &[common::stdlib_jar()],
            Some(&common::jdk_modules()),
        );
        assert_eq!(result.as_deref(), Some("OK"));
    }

    /// The declarations come from a kotlinc-built dependency: its constructor, field getter and inner
    /// members take and return `Base`, and the consumer calls them so.
    #[test]
    fn class_formal_slots_of_a_kotlinc_dependency_are_called_like_kotlinc() {
        let consumer = USES.replacen("package formals\n", "import formals.*\n", 1);
        let results = common::class_bytes_diffs_against_kotlinc(
            "ClassFormalUses",
            &[("Declarations.kt", DECLARATIONS)],
            &consumer,
            "ClassFormalUsesKt",
            &["box"],
        )
        .expect("reference kotlinc is provisioned");
        for result in results {
            result.unwrap_or_else(|diff| panic!("{diff}"));
        }
        assert_eq!(
            common::expect_box_run_against_kotlinc(DECLARATIONS, &consumer)
                .expect("reference kotlinc is provisioned"),
            "OK"
        );
    }
}

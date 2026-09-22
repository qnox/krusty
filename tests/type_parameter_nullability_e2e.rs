//! A bare type parameter's nullability annotation follows its BOUND.
//!
//! `fun get(): T` erases to `Object`. Whether that position is annotated depends on what `T` can
//! be: `<T>` carries the implicit `Any?` bound and may be instantiated with a nullable type, so
//! kotlinc annotates it neither `@NotNull` nor `@Nullable`; `<T : Any>` is known non-null and gets
//! `@NotNull`.
//!
//! krusty stamped `@NotNull` on every reference-erased return, so every unbounded generic interface
//! published a non-null contract its own type parameter does not guarantee — a Java caller reading
//! `Provider<String?>.get()` would be told the result cannot be null.
//!
//! The concrete-method emitter already suppressed the annotation for a type-parameter position; the
//! ABSTRACT one did not, which is why this showed up on interfaces.

use super::common;

const SRC: &str = "interface Unbounded<T> {\n\
                   \x20   fun get(): T\n\
                   }\n\
                   \n\
                   interface Bounded<T : Any> {\n\
                   \x20   fun get(): T\n\
                   }\n\
                   \n\
                   interface Concrete {\n\
                   \x20   fun get(): String\n\
                   }\n";

/// The annotation counts, per class, against kotlinc's own output.
fn not_null_counts(dir: &std::path::Path) -> Vec<(String, usize)> {
    let classpath = dir.to_string_lossy().into_owned();
    ["Unbounded", "Bounded", "Concrete"]
        .iter()
        .map(|class| {
            let text = common::javap(&["-p", "-v", "-cp", classpath.as_str(), class])
                .unwrap_or_else(|| panic!("javap {class}"));
            (
                (*class).to_string(),
                text.matches("annotations.NotNull").count(),
            )
        })
        .collect()
}

#[test]
fn an_unbounded_type_parameter_return_is_not_annotated_non_null() {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skipping: no scratch directory");
        return;
    };
    let reference = dir.join("ref");
    let out = dir.join("out");
    std::fs::create_dir_all(&reference).expect("create reference directory");
    std::fs::create_dir_all(&out).expect("create output directory");
    let source = dir.join("TypeParamNullability.kt");
    std::fs::write(&source, SRC).expect("write fixture");
    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-jvm-target".to_string(),
        "25".to_string(),
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");

    let classes =
        common::compile_in_process(SRC, "TypeParamNullability", &[common::stdlib_jar()], None)
            .expect("the fixture compiles");
    for (internal, bytes) in &classes {
        std::fs::write(out.join(format!("{internal}.class")), bytes).expect("write class");
    }

    let want = not_null_counts(&reference);
    // Stated as well as compared: the unbounded interface carries NONE, and the other two do carry
    // theirs, so a blanket "never annotate a type parameter" would fail this too.
    assert_eq!(
        want,
        vec![
            ("Unbounded".to_string(), 0),
            ("Bounded".to_string(), 1),
            ("Concrete".to_string(), 1),
        ],
        "kotlinc's annotation counts"
    );
    assert_eq!(not_null_counts(&out), want, "krusty's annotation counts");
}

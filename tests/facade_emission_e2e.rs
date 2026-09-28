//! kotlinc emits the `<File>Kt` file-facade class ONLY when the file declares top-level
//! callables/properties. A file of only classes/objects gets no facade — emitting an empty one is a
//! spurious extra class (an ABI divergence a drop-in compiler must not introduce).

use super::common;

fn class_names(src: &str) -> Vec<String> {
    let classes = common::compile_in_process(src, "B", &[], None)
        .unwrap_or_else(|| panic!("krusty failed to compile:\n{src}"));
    classes.into_iter().map(|(name, _)| name).collect()
}

#[test]
fn class_only_file_emits_no_facade() {
    let names = class_names("data class Team(val id: String, val name: String)\n");
    assert!(names.contains(&"Team".to_string()), "classes: {names:?}");
    assert!(
        !names.contains(&"BKt".to_string()),
        "spurious empty facade emitted: {names:?}",
    );
}

#[test]
fn top_level_function_emits_facade() {
    let names = class_names("class C\nfun topFun(): Int = 42\n");
    assert!(
        names.contains(&"BKt".to_string()),
        "facade missing for a file with a top-level function: {names:?}",
    );
}

#[test]
fn top_level_property_emits_facade() {
    let names = class_names("class C\nval answer: Int = 42\n");
    assert!(
        names.contains(&"BKt".to_string()),
        "facade missing for a file with a top-level property: {names:?}",
    );
}

/// Compiles `src` with kotlinc and krusty over the same classpath.
fn classes_against_kotlinc(stem: &str, src: &str) -> common::ClassSets {
    common::classes_against_kotlinc_lib(stem, &[("Lib.kt", "package lib\n\nclass Lib\n")], src)
        .expect("reference kotlinc is provisioned")
}

#[test]
fn a_default_valued_facade_initializer_keeps_an_empty_clinit() {
    // Each store is elided (the JVM already holds the default), but kotlinc still gives the
    // facade a `<clinit>` holding only `return`.
    let src = "val absent: String? = null\n\
        var count: Int = 0\n\
        val disabled: Boolean = false\n\
        var ratio: Double = 0.0\n";
    let classes = classes_against_kotlinc("FacadeDefaults", src);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn a_facade_of_lateinit_and_const_properties_has_no_clinit() {
    // Neither runs code at class initialization: a `lateinit var` has no initializer and a
    // `const val` is a `ConstantValue`.
    let src = "lateinit var name: String\n\
        const val limit = 3\n";
    let classes = classes_against_kotlinc("FacadeNoInitializer", src);
    let (kotlinc, krusty) = classes
        .method_declarations("FacadeNoInitializerKt")
        .expect("both compilers write the facade");
    assert_eq!(krusty, kotlinc);
}

#[test]
fn object_only_file_emits_no_facade() {
    let names = class_names("object Registry {\n    val size: Int = 0\n}\n");
    assert!(
        names.contains(&"Registry".to_string()),
        "classes: {names:?}"
    );
    assert!(
        !names.contains(&"BKt".to_string()),
        "spurious empty facade emitted for an object-only file: {names:?}",
    );
}

/// A reference field's fresh value is `null`, so a boxed zero or `false` is a real store: kotlinc
/// keeps it in `<clinit>` and in the constructor, where a scalar zero would be elided.
#[test]
fn a_boxed_zero_initializer_is_stored_like_kotlinc() {
    let src = "val boxedCount: Int? = 0\n\
        val boxedFlag: Boolean? = false\n\
        var anyLong: Any = 0L\n\
        val absent: String? = null\n\
        val count: Int = 0\n\
        class Holder {\n    val boxed: Int? = 0\n    var ratio: Double? = 0.0\n    val plain = 0\n}\n";
    let pair = common::ModuleClassPair::compile(&[("BoxedDefaults.kt", src)], "BoxedDefaultsKt");
    let (kotlinc, krusty) = pair.method_code("BoxedDefaultsKt", "<clinit>");
    assert_eq!(krusty, kotlinc);
    let pair = common::ModuleClassPair::compile(&[("BoxedDefaults.kt", src)], "Holder");
    let (kotlinc, krusty) = pair.method_code("Holder", "Holder");
    assert_eq!(krusty, kotlinc);
}

/// Value-class storage is judged by the carrier slot the JVM zero-fills. kotlinc stores a
/// non-null `Z(0)` into its `int` carrier through `constructor-impl`, boxes a `Z?` field's value,
/// passes a nullable carrier through `constructor-impl`, and elides only the `null` store into a
/// boxed `S?` slot.
#[test]
fn value_class_initializers_are_stored_by_their_carrier_slot() {
    let src = "@JvmInline value class Z(val x: Int)\n\
        @JvmInline value class S(val s: String?)\n\
        val carrierZero: Z = Z(0)\n\
        val boxedZero: Z? = Z(0)\n\
        val nullCarrier: S = S(null)\n\
        val boxedNull: S? = null\n\
        class Holder {\n    val carrierZero: Z = Z(0)\n    val boxedZero: Z? = Z(0)\n    \
        val nullCarrier: S = S(null)\n    val boxedNull: S? = null\n    val boxedInt: Int? = 0\n}\n";
    let sources = [("ValueDefaults.kt", src)];
    let pair = common::ModuleClassPair::compile(&sources, "ValueDefaultsKt");
    let (kotlinc, krusty) = pair.method_code("ValueDefaultsKt", "<clinit>");
    assert_eq!(krusty, kotlinc);
    let pair = common::ModuleClassPair::compile(&sources, "Holder");
    let (kotlinc, krusty) = pair.method_code("Holder", "Holder");
    assert_eq!(krusty, kotlinc);
}

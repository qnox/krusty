//! A named function-type parameter (`(count: Int) -> Unit`) in `@Metadata`.
//!
//! kotlinc records the name as a `@kotlin.ParameterName(name = "count")` type annotation on that
//! parameter's `Type`, after the annotations written on the parameter type itself. An unnamed
//! parameter, and a lambda's own parameter, record none.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    let result = common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

const PRELUDE: &str = "package app\n\
    \n\
    @Target(AnnotationTarget.TYPE) annotation class Mark\n\
    class Item\n";

#[test]
fn a_named_parameter_is_recorded_beside_an_unnamed_one() {
    let src = format!(
        "{PRELUDE}\n\
         fun run(action: (count: Int, Item) -> Unit): (label: Item) -> Unit = {{ label -> }}\n"
    );
    assert_identical("RunNamed", &src, "app/RunNamedKt");
}

#[test]
fn extension_suspend_and_annotated_parameters_record_their_names() {
    let src = format!(
        "{PRELUDE}\n\
         fun shapes(\n\
             extension: Item.(index: Int) -> Unit,\n\
             suspending: suspend (step: Int) -> Unit,\n\
             annotated: (first: @Mark Item) -> Unit,\n\
             nested: ((inner: Item) -> Unit) -> ((outer: Item) -> Unit),\n\
         ) {{}}\n"
    );
    assert_identical("Shapes", &src, "app/ShapesKt");
}

#[test]
fn a_property_and_constructor_parameter_record_their_names() {
    let src = format!(
        "{PRELUDE}\n\
         class Host(val on: (event: Item) -> Unit) {{\n\
             var handler: ((code: Int) -> Unit)? = null\n\
         }}\n"
    );
    assert_identical("Host", &src, "app/Host");
}

#[test]
fn a_typealias_right_hand_side_records_its_names() {
    let src = format!(
        "{PRELUDE}\n\
         typealias Listener = (event: Item) -> Unit\n\
         fun listen(listener: Listener, direct: (event: Item) -> Unit) {{}}\n"
    );
    assert_identical("Listen", &src, "app/ListenKt");
}

#[test]
fn a_lambda_parameter_records_no_name() {
    let src = format!(
        "{PRELUDE}\n\
         val inferred = {{ item: Item -> item }}\n"
    );
    assert_identical("Inferred", &src, "app/InferredKt");
}

/// The expanded use of a function-type alias keeps the abbreviations its right-hand side spells on
/// the parameters and the return.
#[test]
fn a_function_typealias_use_keeps_its_component_abbreviations() {
    let src = format!(
        "{PRELUDE}\n\
         typealias Cargo = Item\n\
         typealias Handler = (Cargo) -> Cargo\n\
         fun handle(handler: Handler) {{}}\n"
    );
    assert_identical("Handle", &src, "app/HandleKt");
}

const DEPENDENCY: &str = "package dep\n\
    \n\
    class Item\n\
    typealias Listener = (event: Item) -> Unit\n";

/// A dependency's alias carries the names its right-hand side wrote into every expanded use.
#[test]
fn a_dependency_typealias_use_records_its_names() {
    let src = "package app\n\
        \n\
        import dep.Listener\n\
        \n\
        fun listen(listener: Listener) {}\n";
    let result = common::metadata_diff_against_kotlinc_lib(
        "DependencyListen",
        &[("Dep.kt", DEPENDENCY)],
        src,
        "app/DependencyListenKt",
    )
    .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

const GENERIC_ALIASES: &str = "typealias Cargo = Item\n\
    typealias Handler<T> = (event: T) -> Unit\n\
    typealias Tagged<T> = (event: @Mark T) -> Unit\n";

/// A generic function-type alias keeps, at each substituted parameter, both what the right-hand
/// side wrote there (the parameter's name and annotations) and the use site's own spelling of the
/// argument (its alias and annotations).
#[test]
fn a_generic_typealias_use_merges_its_names_with_the_argument_spelling() {
    let src = format!(
        "{PRELUDE}\n\
         @Target(AnnotationTarget.TYPE) annotation class Used\n\
         {GENERIC_ALIASES}\
         fun handle(plain: Handler<Item>, aliased: Handler<@Used Cargo>, tagged: Tagged<@Used Cargo>, annotated: Tagged<@Used Item>) {{}}\n"
    );
    assert_identical("GenericHandle", &src, "app/GenericHandleKt");
}

/// The same for a dependency's generic aliases, whose right-hand-side spellings are decoded from
/// its metadata.
#[test]
fn a_dependency_generic_typealias_use_merges_its_names_with_the_argument_spelling() {
    let dependency = format!(
        "package dep\n\
         \n\
         @Target(AnnotationTarget.TYPE) annotation class Mark\n\
         class Item\n\
         {GENERIC_ALIASES}"
    );
    let src = "package app\n\
        \n\
        import dep.Cargo\n\
        import dep.Handler\n\
        import dep.Item\n\
        import dep.Tagged\n\
        \n\
        @Target(AnnotationTarget.TYPE) annotation class Used\n\
        fun handle(plain: Handler<Item>, aliased: Handler<@Used Cargo>, tagged: Tagged<@Used Cargo>) {}\n";
    let result = common::metadata_diff_against_kotlinc_lib(
        "DependencyGenericHandle",
        &[("Dep.kt", dependency.as_str())],
        src,
        "app/DependencyGenericHandleKt",
    )
    .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

/// A dependency alias's right-hand side keeps its argument-bearing annotations beside the
/// parameter's name: `@Bin(3)` and `@Label("x")` are recorded with their values, and the derived
/// `@ParameterName` only once.
#[test]
fn a_dependency_typealias_keeps_argument_bearing_annotations_beside_the_parameter_name() {
    let dependency = "package dep\n\
         \n\
         @Target(AnnotationTarget.TYPE) annotation class Bin(val value: Int)\n\
         @Target(AnnotationTarget.TYPE) annotation class Label(val text: String)\n\
         class Item\n\
         typealias Cargo = Item\n\
         typealias Binned<T> = (event: @Bin(3) T) -> Unit\n\
         typealias Labelled = (event: @Label(\"x\") @Bin(5) Item, Item) -> Unit\n";
    let src = "package app\n\
        \n\
        import dep.Binned\n\
        import dep.Cargo\n\
        import dep.Item\n\
        import dep.Labelled\n\
        \n\
        @Target(AnnotationTarget.TYPE) annotation class Used(val level: Int)\n\
        fun handle(plain: Binned<Item>, aliased: Binned<@Used(2) Cargo>, labelled: Labelled) {}\n";
    let result = common::metadata_diff_against_kotlinc_lib(
        "DependencyArgumentAnnotations",
        &[("Dep.kt", dependency)],
        src,
        "app/DependencyArgumentAnnotationsKt",
    )
    .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

//! `Type.annotation` (`@Metadata` extension field 100) — an annotation written on a TYPE USE.
//!
//! kotlinc records every non-`SOURCE` annotation applied to a type occurrence in that occurrence's
//! metadata `Type`, at any depth of the type tree, in source order. The record also interns the
//! annotation's descriptor in `d2`, so a missing record shows up as a constant pool one entry short
//! even when the bytecode matches.

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
    @Target(AnnotationTarget.TYPE) @Retention(AnnotationRetention.BINARY) annotation class Kept\n\
    @Target(AnnotationTarget.TYPE) @Retention(AnnotationRetention.SOURCE) annotation class Dropped\n\
    class Item\n\
    class Holder<T>\n";

#[test]
fn a_parameter_and_return_type_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         fun take(item: @Mark Item): @Kept Item = item\n"
    );
    assert_identical("TakeUse", &src, "app/TakeUseKt");
}

#[test]
fn a_type_argument_annotation_is_recorded_on_the_argument() {
    let src = format!(
        "{PRELUDE}\n\
         fun hold(holder: Holder<@Mark Item>): Holder<@Kept @Mark Item> = Holder()\n"
    );
    assert_identical("HoldUse", &src, "app/HoldUseKt");
}

#[test]
fn a_source_retained_type_annotation_is_not_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         fun drop(item: @Dropped Item): @Dropped Item = item\n"
    );
    assert_identical("DropUse", &src, "app/DropUseKt");
}

#[test]
fn a_property_type_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         val stored: @Mark Item = Item()\n"
    );
    assert_identical("StoredUse", &src, "app/StoredUseKt");
}

/// The box corpus's `checkExactType` helper: an internal classpath annotation on a type-parameter
/// use, reached through `@Suppress("INVISIBLE_REFERENCE")`.
#[test]
fn a_classpath_type_annotation_on_a_type_parameter_is_recorded() {
    let src = "package app\n\
        \n\
        @Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\", \"UNUSED_PARAMETER\")\n\
        fun <T> exact(value: @kotlin.internal.Exact T) {}\n";
    assert_identical("ExactUse", src, "app/ExactUseKt");
}

#[test]
fn a_nullable_and_a_function_type_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         fun maybe(item: @Mark Item?, action: @Mark (Item) -> Unit): @Kept Holder<Item>? = null\n"
    );
    assert_identical("MaybeUse", &src, "app/MaybeUseKt");
}

#[test]
fn a_supertype_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         open class Base\n\
         class Derived : @Mark Base()\n"
    );
    assert_identical("DerivedUse", &src, "app/Derived");
}

/// A typealias's own right-hand side is a declared type occurrence: its annotations are recorded on
/// both `TypeAlias.underlying_type` and `expanded_type`, at whatever depth they were written.
#[test]
fn a_typealias_right_hand_side_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         typealias Marked = @Kept Item\n\
         typealias Nested = Holder<@Kept Item>\n"
    );
    assert_identical("AliasUse", &src, "app/AliasUseKt");
}

/// An annotated right-hand side that itself names another alias.
#[test]
fn a_typealias_right_hand_side_annotation_on_an_alias_reference_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         typealias Cargo = Item\n\
         typealias Tagged = @Kept Cargo\n\
         typealias Wrapped = Holder<@Mark Cargo>\n"
    );
    assert_identical("AliasChainUse", &src, "app/AliasChainUseKt");
}

/// A use of an alias inherits its right-hand side's annotations on the EXPANDED type, ahead of the
/// use's own; the abbreviation records only what the use wrote.
#[test]
fn a_use_of_an_annotated_typealias_inherits_its_annotations() {
    let src = format!(
        "{PRELUDE}\n\
         typealias Marked = @Kept Item\n\
         typealias Nested = Holder<@Kept Item>\n\
         typealias Tagged = @Mark Marked\n\
         fun pass(marked: Marked, nested: Nested, tagged: Tagged): @Mark Marked = marked\n"
    );
    assert_identical("AliasPassUse", &src, "app/AliasPassUseKt");
}

const ARGUMENT_PRELUDE: &str = "package app\n\
    \n\
    import kotlin.reflect.KClass\n\
    \n\
    @Target(AnnotationTarget.TYPE) annotation class Bin(val size: Int)\n\
    @Target(AnnotationTarget.TYPE) annotation class Named(val label: String, val wide: Boolean = false)\n\
    @Target(AnnotationTarget.TYPE) annotation class Moded(val mode: Mode, val kind: KClass<*>)\n\
    @Target(AnnotationTarget.TYPE) annotation class Listed(val sizes: IntArray, val names: Array<String>, val inner: Bin)\n\
    @Target(AnnotationTarget.TYPE) annotation class Many(vararg val tags: String)\n\
    @Target(AnnotationTarget.TYPE) annotation class Paired(val first: Int, val second: Int)\n\
    @Target(AnnotationTarget.TYPE) annotation class Wrapping(val inner: Paired, val all: Array<Paired>)\n\
    enum class Mode { On, Off }\n\
    class Item\n\
    class Holder<T>\n\
    const val LIMIT = 4\n";

/// An annotation application's arguments are recorded with it, in kotlinc's argument order.
#[test]
fn a_type_annotation_with_scalar_arguments_is_recorded() {
    let src = format!(
        "{ARGUMENT_PRELUDE}\n\
         fun take(item: @Bin(3) Item, other: @Bin(LIMIT) Item): @Named(\"x\") Item = item\n\
         fun swap(holder: Holder<@Named(wide = true, label = \"y\") Item>): Holder<@Bin(-1) Item> = Holder()\n"
    );
    assert_identical("ScalarUse", &src, "app/ScalarUseKt");
}

#[test]
fn a_type_annotation_with_enum_and_class_arguments_is_recorded() {
    let src = format!(
        "{ARGUMENT_PRELUDE}\n\
         fun mode(item: @Moded(Mode.On, Item::class) Item): @Moded(kind = Holder::class, mode = Mode.Off) Item = item\n"
    );
    assert_identical("ModedUse", &src, "app/ModedUseKt");
}

#[test]
fn a_type_annotation_with_array_and_nested_arguments_is_recorded() {
    let src = format!(
        "{ARGUMENT_PRELUDE}\n\
         fun list(item: @Listed([1, 2], [\"a\"], Bin(5)) Item): @Many(\"p\", \"q\") Item = item\n\
         fun none(item: @Many Item): @Listed(intArrayOf(), arrayOf(), Bin(0)) Item = item\n"
    );
    assert_identical("ListedUse", &src, "app/ListedUseKt");
}

/// A nested annotation's named arguments are recorded as written, too.
#[test]
fn a_nested_type_annotation_argument_keeps_its_written_order() {
    let src = format!(
        "{ARGUMENT_PRELUDE}\n\
         fun wrap(item: @Wrapping(Paired(second = 2, first = 1), [Paired(second = 4, first = 3)]) Item): Item = item\n"
    );
    assert_identical("WrapUse", &src, "app/WrapUseKt");
}

/// A member's application folds in its classifier's scope, where a companion constant is visible.
#[test]
fn a_member_type_annotation_folds_in_its_classifier_scope() {
    let src = format!(
        "{ARGUMENT_PRELUDE}\n\
         open class Base\n\
         class Crate : @Bin(LIMIT) Base() {{\n\
             companion object {{ const val SIZE = 8 }}\n\
             val held: @Named(\"held\") Item = Item()\n\
             fun put(item: @Bin(SIZE) Item): Holder<@Bin(SIZE + 1) Item> = Holder()\n\
         }}\n"
    );
    assert_identical("CrateUse", &src, "app/Crate");
}

/// Declaration annotations with arguments sit beside the type-use ones: publishing the type-use
/// values must leave the declaration applications to the passes that check them.
#[test]
fn a_member_type_annotation_beside_declaration_annotation_arguments_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         @Target(AnnotationTarget.PROPERTY) annotation class Note(val text: String)\n\
         class Box<out V>(private val raw: Any?) {{\n\
             @Note(\"raw\")\n\
             val value: Any? get() = raw\n\
             fun valueOr(fallback: @UnsafeVariance V): V = fallback\n\
             fun kept(item: @Kept Item): Item = item\n\
         }}\n"
    );
    assert_identical("BoxUse", &src, "app/Box");
}

/// A typealias's right-hand side with argument-bearing annotations, and its use.
#[test]
fn an_annotated_typealias_with_arguments_is_recorded() {
    let src = format!(
        "{ARGUMENT_PRELUDE}\n\
         typealias Sized = @Bin(7) Item\n\
         typealias Labeled = Holder<@Named(\"z\") Item>\n\
         fun pass(sized: Sized, labeled: Labeled): @Named(\"w\") Sized = sized\n"
    );
    assert_identical("SizedUse", &src, "app/SizedUseKt");
}

const ANNOTATED_DEPENDENCY: &str = "package dep\n\
    \n\
    @Target(AnnotationTarget.TYPE) annotation class Kept\n\
    @Target(AnnotationTarget.TYPE) annotation class Bin(val size: Int)\n\
    class Item\n\
    class Holder<T>\n\
    typealias Marked = @Kept Item\n\
    typealias Sized = @Bin(2) Item\n\
    typealias Nested = Holder<@Bin(3) Item>\n\
    typealias Tagged = @Bin(9) Marked\n";

/// A use of an annotated typealias read from a compiled dependency's metadata inherits its
/// right-hand side's annotations exactly as a source alias's use does.
#[test]
fn a_use_of_an_annotated_dependency_typealias_inherits_its_annotations() {
    let src = "package app\n\
        \n\
        import dep.*\n\
        \n\
        fun pass(marked: Marked, sized: Sized, nested: Nested, tagged: Tagged): @Bin(1) Marked = marked\n";
    let result = common::metadata_diff_against_kotlinc_lib(
        "DepAliasUse",
        &[("Dep.kt", ANNOTATED_DEPENDENCY)],
        src,
        "app/DepAliasUseKt",
    )
    .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

/// A mistyped type-use annotation argument is reported once, exactly as kotlinc reports it.
#[test]
fn a_mistyped_type_annotation_argument_is_reported_like_kotlinc() {
    let source = "package app\n\
        \n\
        @Target(AnnotationTarget.TYPE) annotation class Bin(val size: Int)\n\
        class Item\n\
        \n\
        fun take(item: @Bin(\"s\") Item): Item = item\n";
    let result = common::compiler_diagnostics(&[("Mistyped.kt", source)], &[]);
    assert_ne!(result.reference_code, 0, "kotlinc accepted the fixture");
    assert_ne!(result.krusty_code, 0, "krusty accepted the fixture");
    let mut krusty = common::compiler_errors(&result.krusty_stdout);
    krusty.extend(common::compiler_errors(&result.krusty_stderr));
    let reference = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    assert_eq!(reference.len(), 1, "{}", result.reference_stderr);
}

//! `@Metadata` version requirements, measured against kotlinc 2.4.0, 2.4.10 and 2.4.20.
//!
//! An inline function null-checks its parameters with `Intrinsics.checkNotNullParameter`, which
//! compilers older than 1.3.50 cannot inline through a functional parameter. kotlinc therefore
//! records a compiler-version requirement on every named, non-private, non-suspend inline function
//! with a value parameter or extension receiver of a function type. Equal requirements share one
//! entry in the owning package's or class's table.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn top_level_inline_functions_with_functional_parameters_require_compiler_1_3_50() {
    const SRC: &str = "package app\n\
        \n\
        inline fun each(block: () -> Unit) = block()\n\
        internal inline fun mapped(block: (Int) -> Int) = block(1)\n\
        private inline fun hidden(block: () -> Unit) = block()\n\
        inline fun (() -> Unit).onReceiver() = this()\n\
        suspend inline fun later(block: () -> Unit) = block()\n\
        inline fun plain(x: Int) = x + 1\n\
        fun eager(block: () -> Unit) = block()\n\
        fun callHidden() = hidden {}\n";
    assert_identical("InlineRequirement", SRC, "app/InlineRequirementKt");
}

#[test]
fn inline_members_share_one_requirement_table_entry() {
    const SRC: &str = "package app\n\
        \n\
        class Holder {\n\
        \x20   fun first() = 1\n\
        \x20   inline fun each(block: () -> Unit) = block()\n\
        \x20   inline fun named(block: (String) -> Unit) = block(\"\")\n\
        }\n";
    assert_identical("member_inline_requirement", SRC, "app/Holder");
}

/// `KFunctionN` and `KSuspendFunctionN` are function types too, as parameters and as receivers. A
/// `KSuspendFunctionN` erases to `KFunction`, which its class id does not map to, so that function
/// records its descriptor as well.
#[test]
fn reflective_function_types_require_compiler_1_3_50() {
    const SRC: &str = "package app\n\
        \n\
        import kotlin.reflect.KFunction1\n\
        import kotlin.reflect.KSuspendFunction0\n\
        \n\
        inline fun viaReference(f: KFunction1<Int, Int>) {}\n\
        inline fun KFunction1<Int, Int>.onReference() {}\n\
        inline fun KSuspendFunction0<Unit>.onSuspendReference() {}\n\
        \n\
        class Holder {\n\
        \x20   inline fun member(f: KFunction1<Int, Int>) {}\n\
        \x20   inline fun KSuspendFunction0<Unit>.onMember() {}\n\
        }\n";
    assert_identical("ReflectiveRequirement", SRC, "app/ReflectiveRequirementKt");
    assert_identical("reflective_member_requirement", SRC, "app/Holder");
}

#[test]
fn a_top_level_property_with_a_definitely_non_null_type_requires_language_1_7() {
    const SRC: &str = "package app\n\
        val <T> T.nonNull: T & Any get() = this!!\n";
    assert_identical("DnnTopLevelProperty", SRC, "app/DnnTopLevelPropertyKt");
}

#[test]
fn a_member_property_with_a_definitely_non_null_type_requires_language_1_7() {
    const SRC: &str = "package app\n\
        interface Holder<T> { val value: T & Any }\n";
    assert_identical("DnnMemberProperty", SRC, "app/Holder");
}

#[test]
fn a_primary_constructor_owns_its_definitely_non_null_requirement() {
    const SRC: &str = "package app\n\
        class Primary<T>(value: T & Any)\n";
    assert_identical("DnnPrimaryConstructor", SRC, "app/Primary");
}

#[test]
fn a_secondary_constructor_owns_its_definitely_non_null_requirement() {
    const SRC: &str = "package app\n\
        class Secondary<T> { constructor(value: T & Any) }\n";
    assert_identical("DnnSecondaryConstructor", SRC, "app/Secondary");
}

#[test]
fn a_class_supertype_with_a_definitely_non_null_argument_requires_language_1_7() {
    const SRC: &str = "package app\n\
        interface Parent<T>\n\
        class Child<T> : Parent<T & Any>\n";
    assert_identical("DnnClassHeader", SRC, "app/Child");
}

#[test]
fn a_type_alias_with_a_definitely_non_null_argument_requires_language_1_7() {
    const SRC: &str = "package app\n\
        class Box<T>\n\
        typealias DnnBox<T> = Box<T & Any>\n";
    assert_identical("DnnTypeAlias", SRC, "app/DnnTypeAliasKt");
}

#[test]
fn data_class_generated_members_keep_their_source_property_dnn_spelling() {
    const SRC: &str = "package app\n\
        data class Data<T>(val value: T & Any)\n";
    assert_identical("DnnDataClass", SRC, "app/Data");
}

#[test]
fn a_property_context_parameter_with_a_dnn_type_requires_language_1_7() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        package app\n\
        class Contextual<T> {\n\
        \x20   context(value: T & Any)\n\
        \x20   val answer: Int get() = 1\n\
        }\n";
    assert_identical("DnnPropertyContext", SRC, "app/Contextual");
}

#[test]
fn a_private_value_class_underlying_dnn_type_is_owned_by_the_class_too() {
    const SRC: &str = "package app\n\
        @JvmInline value class PrivateDnn<T>(private val value: T & Any)\n";
    assert_identical("DnnPrivateValueClass", SRC, "app/PrivateDnn");
}

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

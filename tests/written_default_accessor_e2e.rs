//! A written accessor without a body (`get`, `protected set`, `@A get`, `internal set`) keeps the
//! compiler-generated body, but it is still the source declaration. Its generated JVM method uses
//! the accessor's line, and an internal setter uses the module-mangled name at every call site.

use super::common;

/// Both compilers build `sources` as one module and every class they write is byte-identical.
fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn a_bodiless_accessor_sits_on_its_own_line_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Accessors.kt",
        "package written\n\
         \n\
         annotation class Marked\n\
         \n\
         open class Accessors {\n\
         \x20   val plain: Int = 1\n\
         \x20       get\n\
         \x20   open var guarded: Int = 2\n\
         \x20       protected set\n\
         \x20   var marked: String = \"\"\n\
         \x20       @Marked get\n\
         \x20       @Marked set\n\
         \x20   var both: Long = 3L\n\
         \x20       get\n\
         \x20       set\n\
         }\n",
    )]);
}

#[test]
fn an_internal_setter_links_across_files_with_kotlincs_name() {
    assert_module_matches_kotlinc(&[
        (
            "Owner.kt",
            "package written\n\
             \n\
             class Owner {\n\
             \x20   var name: String = \"\"\n\
             \x20       internal set\n\
             \x20   internal var hidden: Int = 0\n\
             \x20   fun rename(value: String) { name = value }\n\
             }\n",
        ),
        (
            "User.kt",
            "package written\n\
             \n\
             fun use(owner: Owner): String {\n\
             \x20   owner.name = \"next\"\n\
             \x20   owner.hidden = owner.hidden + 1\n\
             \x20   return owner.name\n\
             }\n",
        ),
    ]);
}

#[test]
fn multiline_accessor_prefixes_and_lateinit_tails_keep_exact_lines() {
    assert_module_matches_kotlinc(&[(
        "AccessorLines.kt",
        "package written\n\
         \n\
         class AccessorLines {\n\
         \x20   lateinit var first: String\n\
         \x20       internal set\n\
         \x20   lateinit var split: String\n\
         \x20       internal\n\
         \x20       set\n\
         \x20   var annotated: String = \"\"\n\
         \x20       @Deprecated(\"old\")\n\
         \x20       get\n\
         }\n",
    )]);
}

#[test]
fn top_level_multiline_accessors_and_lateinit_tails_keep_exact_lines() {
    assert_module_matches_kotlinc(&[(
        "TopLevelAccessorLines.kt",
        "package written\n\
         \n\
         lateinit var value: String\n\
         \x20   internal\n\
         \x20   set\n\
         var annotated: String = \"\"\n\
         \x20   @Deprecated(\"old\")\n\
         \x20   get\n",
    )]);
}

#[test]
fn a_hoisted_companion_getter_keeps_its_written_line() {
    assert_module_matches_kotlinc(&[(
        "HoistedAccessorLines.kt",
        "package written\n\
         \n\
         class HoistedAccessorLines {\n\
         \x20   companion object {\n\
         \x20       var value: String = \"\"\n\
         \x20           get\n\
         \x20   }\n\
         }\n",
    )]);
}

#[test]
fn a_hoisted_companion_lateinit_getter_tail_uses_the_declaration_end() {
    assert_module_matches_kotlinc(&[(
        "HoistedLateinitAccessorLines.kt",
        "package written\n\
         \n\
         class HoistedLateinitAccessorLines {\n\
         \x20   companion object {\n\
         \x20       lateinit var value: String\n\
         \x20           internal\n\
         \x20           set\n\
         \x20   }\n\
         }\n",
    )]);
}

//! Features kotlinc enables at language level 2.4 compile with no `-X` flag and no
//! `// LANGUAGE:` directive.

use super::common;

#[test]
fn explicit_backing_field_is_stable_at_language_level_2_4() {
    const SRC: &str = "\
class Inventory {\n\
    val items: List<String>\n\
        field = mutableListOf<String>()\n\
    fun add(item: String) { items.add(item) }\n\
}\n\
fun box(): String {\n\
    val inventory = Inventory()\n\
    inventory.add(\"OK\")\n\
    return if (inventory.items == listOf(\"OK\")) \"OK\" else \"Fail\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "LanguageLevelBackingField");
}

#[test]
fn when_guard_is_stable_at_language_level_2_4() {
    const SRC: &str = "\
sealed interface V\n\
class A(val ok: Boolean) : V\n\
fun f(v: V): Int = when (v) { is A if v.ok -> 1; else -> 0 }\n\
fun box(): String = if (f(A(true)) == 1 && f(A(false)) == 0) \"OK\" else \"Fail\"\n";
    common::expect_box_same_as_kotlinc(SRC, "LanguageLevelWhenGuard");
}

#[test]
fn protected_super_companion_property_is_stable_at_language_level_2_4() {
    const SRC: &str = "\
open class Base {\n\
    companion object { protected const val secret: Int = 1 }\n\
}\n\
class Derived : Base() {\n\
    fun read(): Int = secret\n\
}\n\
fun box(): String = if (Derived().read() == 1) \"OK\" else \"Fail\"\n";
    common::expect_box_same_as_kotlinc(SRC, "LanguageLevelProtectedCompanion");
}

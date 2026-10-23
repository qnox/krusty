//! A `@Serializable` class whose serializable superclass another file of the same module declares
//! compiles like kotlinc compiles it.
//!
//! The subclass's file sees the superclass only through the record the serialization plugin
//! publishes from the module's checked declarations before any file is lowered: its stored
//! properties in declaration order with their types and whether each has a default. The subclass's
//! `write$Self` calls the superclass's public `write$Self`, and its deserialization constructor
//! delegates to the superclass's, which stores the inherited elements.

use super::compiler_plugin_fixture::{assert_same_classes_and_box, PluginFixture};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const PARENTS: &str = "package lib\n\
                       import kotlinx.serialization.Serializable\n\
                       import kotlinx.serialization.Transient\n\
                       @Serializable\n\
                       open class Base(val zeta: Int, var alpha: String = \"t\", val tags: List<String>) {\n\
                       \x20   @Transient val skipped: Int = 3\n\
                       \x20   val mid: Long = 4L\n\
                       \x20   var beta: Int? = null\n\
                       \x20   val derived: Int get() = zeta + 1\n\
                       }\n\
                       @Serializable\n\
                       abstract class Shape(val name: String)\n\
                       @Serializable\n\
                       open class Middle(val m: Int) : Base(m, tags = listOf(\"m\"))\n";

const MAIN: &str = "import kotlinx.serialization.Serializable\n\
                    import kotlinx.serialization.json.Json\n\
                    import lib.Base\n\
                    import lib.Middle\n\
                    import lib.Shape\n\
                    @Serializable\n\
                    class Child(val b: String) : Base(1, tags = listOf(\"x\"))\n\
                    @Serializable\n\
                    class Square(val side: Double) : Shape(\"sq\")\n\
                    @Serializable\n\
                    class Leaf(val l: Long) : Middle(2)\n\
                    fun box(): String {\n\
                    \x20   val child = Json.encodeToString(Child.serializer(), Child(\"c\"))\n\
                    \x20   if (child != \"{\\\"zeta\\\":1,\\\"tags\\\":[\\\"x\\\"],\\\"b\\\":\\\"c\\\"}\") return child\n\
                    \x20   val back = Json.decodeFromString(Child.serializer(), \"{\\\"zeta\\\":5,\\\"alpha\\\":\\\"a\\\",\\\"tags\\\":[],\\\"beta\\\":7,\\\"b\\\":\\\"d\\\"}\")\n\
                    \x20   if (back.zeta != 5 || back.alpha != \"a\" || back.beta != 7 || back.b != \"d\" || back.mid != 4L) return \"child\"\n\
                    \x20   val square = Json.encodeToString(Square.serializer(), Square(2.0))\n\
                    \x20   if (square != \"{\\\"name\\\":\\\"sq\\\",\\\"side\\\":2.0}\") return square\n\
                    \x20   if (Json.decodeFromString(Square.serializer(), square).name != \"sq\") return \"square\"\n\
                    \x20   val leaf = Json.encodeToString(Leaf.serializer(), Leaf(9))\n\
                    \x20   if (leaf != \"{\\\"zeta\\\":2,\\\"tags\\\":[\\\"m\\\"],\\\"m\\\":2,\\\"l\\\":9}\") return leaf\n\
                    \x20   try {\n\
                    \x20       val incomplete = Json.decodeFromString(Child.serializer(), \"{\\\"b\\\":\\\"d\\\"}\")\n\
                    \x20       return incomplete.b\n\
                    \x20   } catch (e: Exception) {\n\
                    \x20       return \"OK\"\n\
                    \x20   }\n\
                    }\n";

#[test]
fn subclasses_inherit_the_elements_a_sibling_file_parent_declares() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-same-module");
    let sources = [("Parents.kt", PARENTS), ("Main.kt", MAIN)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}

const TYPE_USE_PARENT: &str = "package tagged\n\
                               import kotlinx.serialization.KSerializer\n\
                               import kotlinx.serialization.Serializable\n\
                               import kotlinx.serialization.descriptors.PrimitiveKind\n\
                               import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
                               import kotlinx.serialization.encoding.Decoder\n\
                               import kotlinx.serialization.encoding.Encoder\n\
                               class Item(val v: Int)\n\
                               object ItemSerializer : KSerializer<Item> {\n\
                               \x20   override val descriptor = PrimitiveSerialDescriptor(\"Item\", PrimitiveKind.INT)\n\
                               \x20   override fun serialize(encoder: Encoder, value: Item) = encoder.encodeInt(value.v)\n\
                               \x20   override fun deserialize(decoder: Decoder) = Item(decoder.decodeInt())\n\
                               }\n\
                               @Serializable\n\
                               open class Tagged(\n\
                               \x20   val groups: Map<String, List<@Serializable(with = ItemSerializer::class) Item>>,\n\
                               )\n";

const TYPE_USE_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                             @Serializable\n\
                             class Grouped(val n: Int) : tagged.Tagged(mapOf())\n";

/// A sibling-file parent element whose type names its own serializer, nested in a type argument,
/// is serialized by that serializer, which the module's record does not carry; the subclass
/// declines rather than serialize the element with the type's ordinary serializer.
#[test]
fn a_sibling_file_parent_element_with_a_type_use_serializer_declines() {
    let (plugin, runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-same-module-type-use");
    let sources = [("Tagged.kt", TYPE_USE_PARENT), ("Main.kt", TYPE_USE_MAIN)];
    fixture.kotlinc("main", &sources, &runtime, &switches);
    assert_eq!(
        fixture.krusty_rejects("main", &sources, &runtime, &switches),
        "<fixture>/main/src/Main.kt:1:1: error: krusty: this construct is not yet supported by the IR backend\n\
         krusty: 1 error(s)\n"
    );
}

const ACCOUNT: &str = "package bank\n\
                       import kotlinx.serialization.Serializable\n\
                       @Serializable\n\
                       open class Account {\n\
                       \x20   val id: Int = 7\n\
                       \x20   val code: Int = id + 1\n\
                       \x20   var label: String\n\
                       \x20   constructor(label: String) { this.label = label }\n\
                       \x20   constructor() : this(\"none\")\n\
                       }\n\
                       @Serializable\n\
                       class Checking(val fee: Int) : Account(\"c\")\n";

const SAVINGS: &str = "import kotlinx.serialization.Serializable\n\
                       import kotlinx.serialization.json.Json\n\
                       import bank.Account\n\
                       import bank.Checking\n\
                       @Serializable\n\
                       class Savings(val rate: Int) : Account(\"s\")\n\
                       fun box(): String {\n\
                       \x20   val parent = Json.encodeToString(Account.serializer(), Account(\"a\"))\n\
                       \x20   if (parent != \"{\\\"label\\\":\\\"a\\\"}\") return parent\n\
                       \x20   val savings = Json.encodeToString(Savings.serializer(), Savings(2))\n\
                       \x20   if (savings != \"{\\\"label\\\":\\\"s\\\",\\\"rate\\\":2}\") return savings\n\
                       \x20   val back = Json.decodeFromString(Savings.serializer(), \"{\\\"label\\\":\\\"x\\\",\\\"rate\\\":3}\")\n\
                       \x20   if (back.id != 7 || back.code != 8 || back.label != \"x\" || back.rate != 3) return \"defaults\"\n\
                       \x20   val explicit = Json.decodeFromString(Savings.serializer(), \"{\\\"id\\\":1,\\\"code\\\":5,\\\"label\\\":\\\"x\\\",\\\"rate\\\":3}\")\n\
                       \x20   if (explicit.id != 1 || explicit.code != 5) return \"explicit\"\n\
                       \x20   if (Json.decodeFromString(Checking.serializer(), \"{\\\"label\\\":\\\"y\\\",\\\"fee\\\":1}\").id != 7) return \"checking\"\n\
                       \x20   try {\n\
                       \x20       return Json.decodeFromString(Savings.serializer(), \"{\\\"rate\\\":3}\").label\n\
                       \x20   } catch (e: Exception) {\n\
                       \x20       return \"OK\"\n\
                       \x20   }\n\
                       }\n";

/// A parent without a primary constructor stores its body properties from its secondary
/// constructors, and an initialized one is still optional: the deserialization constructor runs
/// the initializer when the input omits it, and `write$Self` compares against it. A subclass in
/// the parent's own file and one in another file inherit the same optional elements.
#[test]
fn a_parent_without_a_primary_constructor_keeps_its_initialized_elements_optional() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-secondary-constructor");
    let sources = [("Account.kt", ACCOUNT), ("Main.kt", SAVINGS)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}

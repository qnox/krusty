//! A `@Serializable` class whose serializable superclass a dependency declares compiles like
//! kotlinc compiles it.
//!
//! The subclass inherits the superclass's elements as the dependency's metadata publishes them:
//! the serialization plugin's `propertiesNamesInProgramOrder` list orders them, each property's
//! metadata types it, and the superclass's primary constructor says which ones the input must
//! contain. The subclass's `write$Self` calls the superclass's public `write$Self` and its
//! deserialization constructor delegates to the superclass's, which stores the inherited elements.
//! The list starts with the elements a class inherits, so `Middle`'s names `Base`'s too.
//! Each compiler builds the library itself, and krusty also compiles against kotlinc's library.
//!
//! A generic superclass's elements are typed by the arguments the subclass gives it.
//!
//! An element keeps the property annotations its `$annotations` marker carries: a serial name, a
//! serializer of its own, `@Contextual`.
//!
//! A superclass whose classfile lacks the generated members (it was compiled without the plugin),
//! or whose elements carry annotations the record does not describe (on a type argument,
//! `@Required`, a `@SerialInfo` annotation), declines. kotlinc reads
//! a library class's own elements by name, so a parent that overrides an inherited element names
//! it twice; kotlinc rejects its subclasses and krusty declines them.

use super::compiler_plugin_fixture::{
    assert_same_classes, assert_same_classes_and_box, PluginFixture,
};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const LIBRARY: &[(&str, &str)] = &[(
    "Parents.kt",
    "package lib\n\
     import kotlinx.serialization.Serializable\n\
     import kotlinx.serialization.Transient\n\
     @Serializable\n\
     open class Base(val zeta: Int, var alpha: String = \"t\", val tags: List<String>) {\n\
     \x20   @Transient val skipped: Int = 3\n\
     \x20   val mid: Long = 4L\n\
     \x20   var beta: Int? = null\n\
     }\n\
     @Serializable\n\
     abstract class Shape(val name: String)\n\
     @Serializable\n\
     open class Middle(val m: Int) : Base(m, tags = listOf(\"m\"))\n\
     @Serializable\n\
     open class EmptyOpen\n\
     @Serializable\n\
     abstract class EmptyAbstract\n",
)];

const MAIN: &str = "import kotlinx.serialization.Serializable\n\
                    import kotlinx.serialization.json.Json\n\
                    import lib.Base\n\
                    import lib.EmptyAbstract\n\
                    import lib.EmptyOpen\n\
                    import lib.Middle\n\
                    import lib.Shape\n\
                    @Serializable\n\
                    class Child(val b: String) : Base(1, tags = listOf(\"x\"))\n\
                    @Serializable\n\
                    class Square(val side: Double) : Shape(\"sq\")\n\
                    @Serializable\n\
                    class Leaf(val l: Long) : Middle(2)\n\
                    @Serializable\n\
                    class FromOpen(val o: Int) : EmptyOpen()\n\
                    @Serializable\n\
                    class FromAbstract(val a: Int) : EmptyAbstract()\n\
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
                    \x20   val fromOpen = Json.encodeToString(FromOpen.serializer(), FromOpen(3))\n\
                    \x20   if (fromOpen != \"{\\\"o\\\":3}\") return fromOpen\n\
                    \x20   if (Json.decodeFromString(FromOpen.serializer(), fromOpen).o != 3) return \"open\"\n\
                    \x20   val fromAbstract = Json.encodeToString(FromAbstract.serializer(), FromAbstract(4))\n\
                    \x20   if (Json.decodeFromString(FromAbstract.serializer(), fromAbstract).a != 4) return fromAbstract\n\
                    \x20   try {\n\
                    \x20       val incomplete = Json.decodeFromString(Child.serializer(), \"{\\\"b\\\":\\\"d\\\"}\")\n\
                    \x20       return incomplete.b\n\
                    \x20   } catch (e: Exception) {\n\
                    \x20       return \"OK\"\n\
                    \x20   }\n\
                    }\n";

#[test]
fn subclasses_inherit_the_elements_a_dependency_parent_publishes() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-dependency");
    let reference_lib = fixture.kotlinc("lib", LIBRARY, &runtime, &switches);
    let krusty_lib = fixture.krusty("lib", LIBRARY, &runtime, &switches);
    assert_same_classes(&reference_lib, &krusty_lib);
    let sources = [("Main.kt", MAIN)];
    let mut reference_cp = runtime.clone();
    reference_cp.push(reference_lib);
    let mut krusty_cp = runtime.clone();
    krusty_cp.push(krusty_lib);
    let reference = fixture.kotlinc("main", &sources, &reference_cp, &switches);
    let krusty = fixture.krusty("main", &sources, &krusty_cp, &switches);
    assert_same_classes_and_box(&reference, &krusty, &krusty_cp);
    let on_reference_lib =
        fixture.krusty("main-on-kotlinc-lib", &sources, &reference_cp, &switches);
    assert_same_classes_and_box(&reference, &on_reference_lib, &reference_cp);
}

const PLAIN_LIBRARY: &[(&str, &str)] = &[(
    "Plain.kt",
    "package plain\n\
     import kotlinx.serialization.Serializable\n\
     @Serializable\n\
     open class Plain\n",
)];

const PLAIN_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                          @Serializable\n\
                          class Sub(val x: Int) : plain.Plain()\n";

/// `@Serializable` on a class compiled without the plugin gives it no `write$Self` and no
/// deserialization constructor, so a subclass has nothing to call and declines.
#[test]
fn a_parent_compiled_without_the_plugin_declines() {
    let (plugin, runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-without-plugin");
    let library = fixture.krusty("lib", PLAIN_LIBRARY, &runtime, &[]);
    let mut classpath = runtime.clone();
    classpath.push(library);
    let diagnostics =
        fixture.krusty_rejects("main", &[("Main.kt", PLAIN_MAIN)], &classpath, &switches);
    assert_eq!(
        diagnostics,
        "<fixture>/main/src/Main.kt:1:1: error: krusty: this construct is not yet supported by the IR backend\n\
         krusty: 1 error(s)\n"
    );
}

const TYPE_USE_LIBRARY: &[(&str, &str)] = &[(
    "Tagged.kt",
    "package tagged\n\
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
     \x20   val items: List<@Serializable(with = ItemSerializer::class) Item>,\n\
     \x20   val groups: Map<String, List<@Serializable(with = ItemSerializer::class) Item>>,\n\
     )\n",
)];

const TYPE_USE_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                             @Serializable\n\
                             class Grouped(val n: Int) : tagged.Tagged(listOf(), mapOf())\n";

/// An element whose type names its own serializer, directly in a type argument or nested deeper,
/// is serialized by that serializer, which the published record does not carry; the subclass
/// declines rather than serialize the element with the type's ordinary serializer.
#[test]
fn a_parent_element_with_a_type_use_serializer_declines() {
    let (plugin, runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-type-use");
    let reference_lib = fixture.kotlinc("lib", TYPE_USE_LIBRARY, &runtime, &switches);
    let krusty_lib = fixture.krusty("lib", TYPE_USE_LIBRARY, &runtime, &switches);
    assert_same_classes(&reference_lib, &krusty_lib);
    let sources = [("Main.kt", TYPE_USE_MAIN)];
    let mut reference_cp = runtime.clone();
    reference_cp.push(reference_lib);
    fixture.kotlinc("main", &sources, &reference_cp, &switches);
    let mut krusty_cp = runtime.clone();
    krusty_cp.push(krusty_lib);
    assert_eq!(
        fixture.krusty_rejects("main", &sources, &krusty_cp, &switches),
        "<fixture>/main/src/Main.kt:1:1: error: krusty: this construct is not yet supported by the IR backend\n\
         krusty: 1 error(s)\n"
    );
    assert_eq!(
        fixture.krusty_rejects("main-on-kotlinc-lib", &sources, &reference_cp, &switches),
        "<fixture>/main-on-kotlinc-lib/src/Main.kt:1:1: error: krusty: this construct is not yet supported by the IR backend\n\
         krusty: 1 error(s)\n"
    );
}

const OVERRIDE_LIBRARY: &[(&str, &str)] = &[(
    "Over.kt",
    "package over\n\
     import kotlinx.serialization.Serializable\n\
     @Serializable\n\
     open class Root(open val x: Int)\n\
     @Serializable\n\
     open class Over : Root(1) {\n\
     \x20   override val x: Int get() = 2\n\
     }\n",
)];

const OVERRIDE_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                             @Serializable\n\
                             class Tip(val y: Int) : over.Over()\n";

/// `Over` compiles, but kotlinc reads its own elements from a library by name: its getter-only
/// override of `x` makes `x` both inherited and its own, so kotlinc rejects a subclass, and krusty
/// declines one rather than accept what kotlinc does not.
#[test]
fn a_parent_overriding_an_inherited_element_declines() {
    let (plugin, runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-override");
    let reference_lib = fixture.kotlinc("lib", OVERRIDE_LIBRARY, &runtime, &switches);
    let krusty_lib = fixture.krusty("lib", OVERRIDE_LIBRARY, &runtime, &switches);
    assert_same_classes(&reference_lib, &krusty_lib);
    let sources = [("Main.kt", OVERRIDE_MAIN)];
    let mut reference_cp = runtime.clone();
    reference_cp.push(reference_lib);
    assert_eq!(
        fixture.kotlinc_rejects("main", &sources, &reference_cp, &switches),
        "<fixture>/main/src/Main.kt:2:1: error: serializable class has a duplicate serial name for property 'x', either in the class itself or in its supertypes.\n\
         @Serializable\n\
         ^^^^^^^^^^^^^\n"
    );
    let mut krusty_cp = runtime.clone();
    krusty_cp.push(krusty_lib);
    assert_eq!(
        fixture.krusty_rejects("main-krusty", &sources, &krusty_cp, &switches),
        "<fixture>/main-krusty/src/Main.kt:1:1: error: krusty: this construct is not yet supported by the IR backend\n\
         krusty: 1 error(s)\n"
    );
}

const GENERIC_LIBRARY: &[(&str, &str)] = &[(
    "Generic.kt",
    "package generic\n\
     import kotlinx.serialization.Serializable\n\
     @Serializable\n\
     open class Holder<T>(val value: T, val count: Int = 0)\n\
     @Serializable\n\
     abstract class Couple<A, B>(val first: A, val rest: List<B>)\n\
     @Serializable\n\
     open class Fixed(val f: Int) : Holder<String>(\"f\")\n\
     @Serializable\n\
     class Item(val i: Int)\n",
)];

const GENERIC_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                            import kotlinx.serialization.builtins.serializer\n\
                            import kotlinx.serialization.json.Json\n\
                            import generic.Couple\n\
                            import generic.Fixed\n\
                            import generic.Holder\n\
                            @Serializable\n\
                            class Named(val n: String) : Holder<Int>(1)\n\
                            @Serializable\n\
                            class Both<U>(val u: U) : Couple<String, U>(\"a\", listOf(u))\n\
                            @Serializable\n\
                            class FromFixed(val x: Long) : Fixed(3)\n\
                            @Serializable\n\
                            open class Middle<U>(val tag: U) : Holder<U>(tag)\n\
                            @Serializable\n\
                            class Leaf(val l: Long) : Middle<Int>(1)\n\
                            fun box(): String {\n\
                            \x20   val named = Json.encodeToString(Named.serializer(), Named(\"s\"))\n\
                            \x20   if (named != \"{\\\"value\\\":1,\\\"n\\\":\\\"s\\\"}\") return named\n\
                            \x20   val back = Json.decodeFromString(Named.serializer(), \"{\\\"value\\\":7,\\\"count\\\":2,\\\"n\\\":\\\"t\\\"}\")\n\
                            \x20   if (back.value != 7 || back.count != 2 || back.n != \"t\") return \"named\"\n\
                            \x20   val serializer = Both.serializer(Int.serializer())\n\
                            \x20   val both = Json.encodeToString(serializer, Both(5))\n\
                            \x20   if (both != \"{\\\"first\\\":\\\"a\\\",\\\"rest\\\":[5],\\\"u\\\":5}\") return both\n\
                            \x20   val decoded = Json.decodeFromString(serializer, both)\n\
                            \x20   if (decoded.rest != listOf(5) || decoded.u != 5) return \"both\"\n\
                            \x20   val fixed = Json.encodeToString(FromFixed.serializer(), FromFixed(4))\n\
                            \x20   if (fixed != \"{\\\"value\\\":\\\"f\\\",\\\"f\\\":3,\\\"x\\\":4}\") return fixed\n\
                            \x20   if (Json.decodeFromString(FromFixed.serializer(), fixed).value != \"f\") return \"fixed\"\n\
                            \x20   val leaf = Json.encodeToString(Leaf.serializer(), Leaf(5))\n\
                            \x20   if (leaf != \"{\\\"value\\\":1,\\\"tag\\\":1,\\\"l\\\":5}\") return leaf\n\
                            \x20   val decodedLeaf = Json.decodeFromString(Leaf.serializer(), \"{\\\"value\\\":2,\\\"count\\\":3,\\\"tag\\\":4,\\\"l\\\":6}\")\n\
                            \x20   if (decodedLeaf.value != 2 || decodedLeaf.count != 3 || decodedLeaf.tag != 4 || decodedLeaf.l != 6L) return \"leaf\"\n\
                            \x20   val middle = Json.encodeToString(Middle.serializer(Int.serializer()), Middle(7))\n\
                            \x20   if (middle != \"{\\\"value\\\":7,\\\"tag\\\":7}\") return middle\n\
                            \x20   return \"OK\"\n\
                            }\n";

const ITEM_ARGUMENT_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                                  import kotlinx.serialization.json.Json\n\
                                  import generic.Holder\n\
                                  import generic.Item\n\
                                  @Serializable\n\
                                  class Child : Holder<Item>(Item(1))\n\
                                  @Serializable\n\
                                  open class Middle<U>(val tag: U) : Holder<U>(tag)\n\
                                  @Serializable\n\
                                  class Leaf(val l: Int) : Middle<Item>(Item(2))\n\
                                  fun box(): String {\n\
                                  \x20   val child = Json.encodeToString(Child.serializer(), Child())\n\
                                  \x20   if (child != \"{\\\"value\\\":{\\\"i\\\":1}}\") return child\n\
                                  \x20   val decoded = Json.decodeFromString(Child.serializer(), \"{\\\"value\\\":{\\\"i\\\":4},\\\"count\\\":5}\")\n\
                                  \x20   if (decoded.value.i != 4 || decoded.count != 5) return \"child\"\n\
                                  \x20   val leaf = Json.encodeToString(Leaf.serializer(), Leaf(3))\n\
                                  \x20   if (leaf != \"{\\\"value\\\":{\\\"i\\\":2},\\\"tag\\\":{\\\"i\\\":2},\\\"l\\\":3}\") return leaf\n\
                                  \x20   val back = Json.decodeFromString(Leaf.serializer(), leaf)\n\
                                  \x20   if (back.value.i != 2 || back.tag.i != 2 || back.l != 3) return \"leaf\"\n\
                                  \x20   return \"OK\"\n\
                                  }\n";

/// A generic parent's elements are typed by the arguments the subclass gives it, and its
/// `write$Self` takes one serializer per type parameter, which the subclass passes for its
/// arguments. A library class with a generic parent inherits the parent's elements typed the same
/// way, and so does a local generic class, whose own subclass sees them typed by its arguments
/// (`Leaf : Middle<Int>` inherits `Holder`'s `value` as `Int`).
#[test]
fn subclasses_inherit_the_elements_of_a_generic_dependency_parent() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-generic-parent-dependency");
    let reference_lib = fixture.kotlinc("lib", GENERIC_LIBRARY, &runtime, &switches);
    let krusty_lib = fixture.krusty("lib", GENERIC_LIBRARY, &runtime, &switches);
    assert_same_classes(&reference_lib, &krusty_lib);
    let sources = [("Main.kt", GENERIC_MAIN)];
    let mut reference_cp = runtime.clone();
    reference_cp.push(reference_lib);
    let mut krusty_cp = runtime.clone();
    krusty_cp.push(krusty_lib);
    let reference = fixture.kotlinc("main", &sources, &reference_cp, &switches);
    let krusty = fixture.krusty("main", &sources, &krusty_cp, &switches);
    assert_same_classes_and_box(&reference, &krusty, &krusty_cp);
    let on_reference_lib =
        fixture.krusty("main-on-kotlinc-lib", &sources, &reference_cp, &switches);
    assert_same_classes_and_box(&reference, &on_reference_lib, &reference_cp);
}

const ANNOTATED_LIBRARY: &[(&str, &str)] = &[(
    "Annotated.kt",
    "package annotated\n\
     import kotlinx.serialization.Contextual\n\
     import kotlinx.serialization.KSerializer\n\
     import kotlinx.serialization.SerialName\n\
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
     open class Labeled(\n\
     \x20   @SerialName(\"zz\") val a: Int,\n\
     \x20   @Serializable(with = ItemSerializer::class) val item: Item,\n\
     \x20   @Contextual val context: Item,\n\
     \x20   val kept: String = \"k\",\n\
     )\n",
)];

const ANNOTATED_MAIN: &str = "import annotated.Item\n\
                              import annotated.ItemSerializer\n\
                              import kotlinx.serialization.Serializable\n\
                              import kotlinx.serialization.json.Json\n\
                              import kotlinx.serialization.modules.SerializersModule\n\
                              @Serializable\n\
                              class Marked(val m: Int) : annotated.Labeled(1, Item(2), Item(3))\n\
                              fun box(): String {\n\
                              \x20   val json = Json { serializersModule = SerializersModule { contextual(Item::class, ItemSerializer) } }\n\
                              \x20   val text = json.encodeToString(Marked.serializer(), Marked(4))\n\
                              \x20   if (text != \"{\\\"zz\\\":1,\\\"item\\\":2,\\\"context\\\":3,\\\"m\\\":4}\") return text\n\
                              \x20   val back = json.decodeFromString(Marked.serializer(), text)\n\
                              \x20   if (back.a != 1 || back.item.v != 2 || back.context.v != 3 || back.kept != \"k\" || back.m != 4) return \"back\"\n\
                              \x20   return \"OK\"\n\
                              }\n";

/// A parent element's property annotations travel with it: `@SerialName` renames it on the wire,
/// and a property-level `@Serializable(with = …)` and `@Contextual` choose its serializer. kotlinc
/// reads them from the property's `$annotations` marker in the library's class file.
#[test]
fn a_parent_element_keeps_its_property_annotations() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-annotated");
    let reference_lib = fixture.kotlinc("lib", ANNOTATED_LIBRARY, &runtime, &switches);
    let krusty_lib = fixture.krusty("lib", ANNOTATED_LIBRARY, &runtime, &switches);
    assert_same_classes(&reference_lib, &krusty_lib);
    let sources = [("Main.kt", ANNOTATED_MAIN)];
    let mut reference_cp = runtime.clone();
    reference_cp.push(reference_lib);
    let mut krusty_cp = runtime.clone();
    krusty_cp.push(krusty_lib);
    let reference = fixture.kotlinc("main", &sources, &reference_cp, &switches);
    let krusty = fixture.krusty("main", &sources, &krusty_cp, &switches);
    assert_same_classes_and_box(&reference, &krusty, &krusty_cp);
    let on_reference_lib =
        fixture.krusty("main-on-kotlinc-lib", &sources, &reference_cp, &switches);
    assert_same_classes_and_box(&reference, &on_reference_lib, &reference_cp);
}

const SERIAL_INFO_LIBRARY: &[(&str, &str)] = &[(
    "Tagged.kt",
    "package info\n\
     import kotlinx.serialization.SerialInfo\n\
     import kotlinx.serialization.Serializable\n\
     @OptIn(kotlinx.serialization.ExperimentalSerializationApi::class)\n\
     @SerialInfo\n\
     @Target(AnnotationTarget.PROPERTY)\n\
     annotation class Tag(val t: String)\n\
     @Serializable\n\
     open class Tagged(@Tag(\"q\") val tagged: String)\n",
)];

const SERIAL_INFO_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                                @Serializable\n\
                                class Child(val c: Int) : info.Tagged(\"t\")\n";

/// A `@SerialInfo` annotation on a parent element is pushed onto the subclass's descriptor, as an
/// instance of a generated implementation class, which the record does not describe; the subclass
/// declines.
#[test]
fn a_parent_element_with_a_serial_info_annotation_declines() {
    assert_parent_declines(
        "serializable-parent-serial-info",
        SERIAL_INFO_LIBRARY,
        SERIAL_INFO_MAIN,
    );
}

const REQUIRED_LIBRARY: &[(&str, &str)] = &[(
    "Kept.kt",
    "package kept\n\
     import kotlinx.serialization.Required\n\
     import kotlinx.serialization.Serializable\n\
     @Serializable\n\
     open class Kept(@Required val k: String = \"k\")\n",
)];

const REQUIRED_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                             @Serializable\n\
                             class Child(val c: Int) : kept.Kept()\n";

/// `@Required` makes a parent element required although it has a default, and changes how the
/// parent's own `write$Self` writes it; the subclass declines rather than mark it optional.
#[test]
fn a_required_parent_element_declines() {
    assert_parent_declines(
        "serializable-parent-required",
        REQUIRED_LIBRARY,
        REQUIRED_MAIN,
    );
}

const SERIALIZER_CLASS_LIBRARY: &[(&str, &str)] = &[(
    "Boxed.kt",
    "package boxed\n\
     import kotlinx.serialization.KSerializer\n\
     import kotlinx.serialization.Serializable\n\
     import kotlinx.serialization.descriptors.PrimitiveKind\n\
     import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
     import kotlinx.serialization.encoding.Decoder\n\
     import kotlinx.serialization.encoding.Encoder\n\
     class Item(val v: Int)\n\
     class ItemSerializer : KSerializer<Item> {\n\
     \x20   override val descriptor = PrimitiveSerialDescriptor(\"Item\", PrimitiveKind.INT)\n\
     \x20   override fun serialize(encoder: Encoder, value: Item) = encoder.encodeInt(value.v)\n\
     \x20   override fun deserialize(decoder: Decoder) = Item(decoder.decodeInt())\n\
     }\n\
     @Serializable\n\
     open class Boxed(@Serializable(with = ItemSerializer::class) val item: Item)\n",
)];

const SERIALIZER_CLASS_MAIN: &str = "import kotlinx.serialization.Serializable\n\
                                     @Serializable\n\
                                     class Child(val c: Int) : boxed.Boxed(boxed.Item(1))\n";

/// A parent element whose serializer is a class rather than an object needs a constructor call,
/// and the checked evidence for one (the frontend's selected constructor) exists only for source
/// annotations. Nothing in the dependency's metadata or class file proves `ItemSerializer()` is
/// the constructor to call, and the plugin never infers one from the classifier name, so the
/// subclass declines.
#[test]
fn a_parent_element_with_a_serializer_class_declines() {
    assert_parent_declines(
        "serializable-parent-serializer-class",
        SERIALIZER_CLASS_LIBRARY,
        SERIALIZER_CLASS_MAIN,
    );
}

/// kotlinc compiles `main` over `library`; krusty declines it over either compiler's build of the
/// library.
fn assert_parent_declines(name: &str, library: &[(&str, &str)], main: &str) {
    let (plugin, runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new(name);
    let reference_lib = fixture.kotlinc("lib", library, &runtime, &switches);
    let krusty_lib = fixture.krusty("lib", library, &runtime, &switches);
    let sources = [("Main.kt", main)];
    let mut reference_cp = runtime.clone();
    reference_cp.push(reference_lib);
    fixture.kotlinc("main", &sources, &reference_cp, &switches);
    let mut krusty_cp = runtime.clone();
    krusty_cp.push(krusty_lib);
    assert_eq!(
        fixture.krusty_rejects("main", &sources, &krusty_cp, &switches),
        "<fixture>/main/src/Main.kt:1:1: error: krusty: this construct is not yet supported by the IR backend\n\
         krusty: 1 error(s)\n"
    );
    assert_eq!(
        fixture.krusty_rejects("main-on-kotlinc-lib", &sources, &reference_cp, &switches),
        "<fixture>/main-on-kotlinc-lib/src/Main.kt:1:1: error: krusty: this construct is not yet supported by the IR backend\n\
         krusty: 1 error(s)\n"
    );
}

/// A generic parent's element typed by a serializable class argument is serialized with that
/// class's serializer, though no property of the subclass names it (`Child : Holder<Item>`), and
/// so is one a local generic parent passes on from the dependency (`Leaf : Middle<Item>`).
#[test]
fn a_generic_dependency_parent_element_takes_its_type_arguments_serializer() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-generic-parent-item-argument");
    let reference_lib = fixture.kotlinc("lib", GENERIC_LIBRARY, &runtime, &switches);
    let krusty_lib = fixture.krusty("lib", GENERIC_LIBRARY, &runtime, &switches);
    assert_same_classes(&reference_lib, &krusty_lib);
    let sources = [("Main.kt", ITEM_ARGUMENT_MAIN)];
    let mut reference_cp = runtime.clone();
    reference_cp.push(reference_lib);
    let mut krusty_cp = runtime.clone();
    krusty_cp.push(krusty_lib);
    let reference = fixture.kotlinc("main", &sources, &reference_cp, &switches);
    let krusty = fixture.krusty("main", &sources, &krusty_cp, &switches);
    assert_same_classes_and_box(&reference, &krusty, &krusty_cp);
    let on_reference_lib =
        fixture.krusty("main-on-kotlinc-lib", &sources, &reference_cp, &switches);
    assert_same_classes_and_box(&reference, &on_reference_lib, &reference_cp);
}

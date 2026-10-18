//! A generic `@Serializable` class and the classes that hold one, compared byte for byte with
//! kotlinc.
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

fn assert_identical(name: &str, source: &str, class: &str) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(name, source, class, &cp, "25", &extra)
        .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} differs from kotlinc\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
        built.reference,
        built.krusty
    );
}

const HOLDERS: &str = "import kotlinx.serialization.Serializable\n\
    @Serializable\n\
    class G<T>(val t: T)\n\
    @Serializable\n\
    class U(val g: G<Int>, val h: G<String>?)\n";

/// `G<Int>` is serialized by `new G.$serializer(IntSerializer.INSTANCE)`, a constructed
/// serializer, so `U` caches it in `$childSerializers` like a collection's.
#[test]
fn a_concrete_generic_element_is_cached() {
    assert_identical("GenericSerializableHolder", HOLDERS, "U");
}

/// The generic class's `$cachedDescriptor` is `@JvmField`, and its `<clinit>` builds it on the
/// class's first line and returns on its last.
#[test]
fn a_generic_class_caches_its_descriptor_like_kotlinc() {
    assert_identical("GenericSerializableClass", HOLDERS, "G");
    assert_identical(
        "GenericSerializableMultiLineClass",
        "import kotlinx.serialization.Serializable\n\
         \n\
         @Serializable\n\
         class G<T>(\n\
         \x20   val t: T,\n\
         ) {\n\
         \x20   fun f() = 1\n\
         }\n",
        "G",
    );
}

const BOUNDED: &str = "import kotlinx.serialization.Serializable\n\
    @Serializable\n\
    class A<T : Any>(val t: T)\n\
    @Serializable\n\
    class B<T : CharSequence?>(val t: T)\n";

/// The companion's `serializer(typeSerial0)` declares unbounded type parameters whatever the
/// class's bounds: kotlinc's metadata records none.
#[test]
fn a_companion_serializer_has_unbounded_type_parameters() {
    assert_identical("GenericSerializableCompanion", HOLDERS, "G$Companion");
    assert_identical("BoundedSerializableNonNull", BOUNDED, "A$Companion");
    assert_identical("BoundedSerializableNullable", BOUNDED, "B$Companion");
}

/// `deserialize` holds a type-parameter element as the `$serializer`'s own type parameter. Before
/// 2.4.21 that parameter is unbounded: the local is `Object`, narrowed to the bound only where the
/// deserialization constructor takes it. From 2.4.21 the plugin copies the class's bounds onto it
/// (KT-89275): the class `Signature` names the bound and each decoded value is cast to it.
#[test]
fn a_bounded_type_parameter_element_is_decoded_into_an_object_local() {
    let source = "import kotlinx.serialization.Serializable\n\
        @Serializable\n\
        class B<T : CharSequence?>(val t: T, val n: Int)\n\
        @Serializable\n\
        class C<T : Number>(val t: T)\n";
    assert_identical("BoundedTypeParameterElement", source, "B$$serializer");
    assert_identical("BoundedTypeParameterElement", source, "C$$serializer");
}

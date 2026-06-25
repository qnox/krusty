//! PURE-KRUSTY serialization round-trip (encode): krusty alone compiles a `@Serializable` class, its
//! `$serializer` (the plugin), the `C.serializer()` accessor (signature phase + static-call lowering),
//! AND the `Json.encodeToString(C.serializer(), C(...))` call (classpath companion-instance call +
//! subtype-aware arg matching) — NO kotlinc anywhere. The JVM then runs `box()` against the published
//! kotlinx-serialization runtime and we assert the JSON. This is the whole serialization extension
//! exercised end-to-end through krusty's own front end + backend.
//!
//! Self-skips if the kotlinx-serialization runtime jars aren't locatable.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// Recursively find a `<prefix>*.jar` (no `-sources`) under `dir`.
fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Option<PathBuf>) {
    if out.is_some() || depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, prefix, depth + 1, out);
        } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
            if n.starts_with(prefix) && n.ends_with(".jar") && !n.contains("sources") {
                *out = Some(p.clone());
                return;
            }
        }
    }
}

/// Locate a serialization runtime jar by prefix across the common cache roots (gradle/m2 + any
/// distribution-bundled gradle lib).
fn find(prefix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let mut roots = vec![
        PathBuf::from(&home).join(".gradle"),
        PathBuf::from(&home).join(".m2"),
    ];
    if let Ok(rd) = std::fs::read_dir("/opt/mise/installs/gradle") {
        roots.extend(rd.flatten().map(|e| e.path()));
    }
    let mut out = None;
    for r in &roots {
        walk(r, prefix, 0, &mut out);
        if out.is_some() {
            break;
        }
    }
    out
}

/// Compile `src` (whose `box(): String` is the entry point) entirely in krusty, run it on the JVM
/// against the kotlinx-serialization runtime, and return the trimmed stdout — or `None` if any runtime
/// dependency is absent (test self-skips). Shared by the encode and the round-trip tests.
fn run_box_in_krusty(src: &str, stem: &str) -> Option<(String, String)> {
    let stdlib = common::stdlib_jar()?;
    let (Some(core), Some(json)) = (
        find("kotlinx-serialization-core-jvm"),
        find("kotlinx-serialization-json-jvm"),
    ) else {
        return None;
    };
    let java_home = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())?;
    let java = PathBuf::from(&java_home).join("bin/java");
    let cp_jars = vec![stdlib.clone(), core.clone(), json.clone()];

    let classes = common::compile_in_process(src, stem, &cp_jars, None)
        .unwrap_or_else(|| panic!("krusty failed to compile the pure-krusty program ({stem})"));

    let out = std::env::temp_dir().join(format!("krusty_ser_{stem}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    for (internal, bytes) in &classes {
        let p = out.join(format!("{internal}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    let launcher = out.join("Run.java");
    std::fs::write(
        &launcher,
        format!(
            r#"public class Run {{ public static void main(String[] a) throws Exception {{
        System.out.println(Class.forName("{stem}Kt").getMethod("box").invoke(null)); }} }}"#
        ),
    )
    .unwrap();
    let javac = PathBuf::from(&java_home).join("bin/javac");
    assert!(Command::new(&javac)
        .args(["-d", out.to_str().unwrap()])
        .arg(&launcher)
        .status()
        .unwrap()
        .success());
    let run = Command::new(&java)
        .arg("-cp")
        .arg(format!(
            "{}:{}:{}:{}",
            out.display(),
            stdlib.display(),
            core.display(),
            json.display()
        ))
        .arg("Run")
        .output()
        .unwrap();
    let res = (
        String::from_utf8_lossy(&run.stdout).trim().to_string(),
        String::from_utf8_lossy(&run.stderr).to_string(),
    );
    let _ = std::fs::remove_dir_all(&out);
    Some(res)
}

#[test]
fn serializable_class_round_trips_through_json_entirely_in_krusty() {
    // The full BIDIRECTIONAL round-trip, no kotlinc: encode `Foo` to JSON, then decode it back and read
    // the reconstructed fields. Decode is the hard half — `decodeFromString(KSerializer<Foo>, String)`
    // returns the generic `T`, which the front end must infer as `Foo` (not the erased `Any`) so that
    // `back.a`/`back.b` resolve.
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
@Serializable
class Foo(val a: Int, val b: String)
fun box(): String {
    val j = Json.encodeToString(Foo.serializer(), Foo(7, "hi"))
    val back = Json.decodeFromString(Foo.serializer(), j)
    return back.b + back.a.toString()
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerRoundTrip") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "hi7",
        "krusty-only serialization round-trip wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty serialization round-trip OK: {stdout}");
}

#[test]
fn serial_name_overrides_json_key_entirely_in_krusty() {
    // `@SerialName("…")` on a constructor property renames its descriptor element (and thus its JSON
    // key) — including a const-folded value (`@SerialName("$prefix.bar")` with `const val prefix`).
    // Mirrors the kotlinc `constValInSerialName` boxIr conformance case (KT-54994). Round-trips and
    // checks data-class equality, all in krusty.
    let src = r#"import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

const val prefix = "foo"

@Serializable
data class Bar(@SerialName("$prefix.bar") val bar: String)

fun box(): String {
    val expected = Bar("hello")
    val json = Json.encodeToString(Bar.serializer(), expected)
    if (json != "{\"foo.bar\":\"hello\"}") return "Fail-encode: $json"
    val actual = Json.decodeFromString(Bar.serializer(), json)
    if (expected != actual) return "Fail-decode: $actual"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerName") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "@SerialName round-trip wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty @SerialName round-trip OK");
}

#[test]
fn reified_serializer_round_trips_entirely_in_krusty() {
    // The REIFIED form `Json.encodeToString(x)` / `Json.decodeFromString<C>(s)` (no explicit serializer
    // argument) — a `reified inline` that can't be called directly. krusty desugars it to the 2-arg
    // member with a synthesized `C.serializer()`, the way kotlinc's inliner would. Full round-trip.
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
@Serializable
class Foo(val a: Int, val b: String)
fun box(): String {
    val j = Json.encodeToString(Foo(1, "x"))
    val back = Json.decodeFromString<Foo>(j)
    return back.b + back.a.toString()
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerReified") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "x1",
        "reified serializer round-trip wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty reified serializer round-trip OK");
}

#[test]
fn custom_serializer_with_clause_entirely_in_krusty() {
    // `@Serializable(with = X::class)`: `serializer()` returns an instance of the explicit serializer X
    // (`new X(C::class)`) instead of a generated `$serializer`, so its descriptor carries X's SerialKind.
    // Mirrors the kotlinx `contextualByDefault` / `polymorphic` boxIr conformance cases. Also exercises a
    // synthetic `static serializer()` on an INTERFACE (no illegal FINAL, InterfaceMethodref invokestatic).
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.ContextualSerializer
import kotlinx.serialization.PolymorphicSerializer

@Serializable(with = ContextualSerializer::class)
class Ref(val id: String)

@Serializable(with = PolymorphicSerializer::class)
interface Poly

fun box(): String {
    val a = Ref.serializer().descriptor.kind.toString()
    if (a != "CONTEXTUAL") return "Ref=$a"
    val b = Poly.serializer().descriptor.kind.toString()
    if (b != "OPEN") return "Poly=$b"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerCustom") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "@Serializable(with=) wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty @Serializable(with=) OK");
}

#[test]
fn descriptor_element_introspection_in_krusty() {
    // The generated `$serializer` now implements `GeneratedSerializer` and builds its descriptor with
    // `this`, so the framework can derive ELEMENT descriptors — `descriptor.getElementDescriptor(i)`
    // returns the i-th property's serializer descriptor (`Int.serializer().descriptor` → "kotlin.Int").
    let src = r#"import kotlinx.serialization.Serializable
@Serializable
class Foo(val a: Int, val b: String)
fun box(): String {
    val d = Foo.serializer().descriptor
    val a = d.getElementDescriptor(0).serialName
    val b = d.getElementDescriptor(1).serialName
    if (a != "kotlin.Int") return "a=$a"
    if (b != "kotlin.String") return "b=$b"
    if (d.getElementName(0) != "a") return "name0"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerIntrospect") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "descriptor element introspection wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty descriptor element introspection OK");
}

#[test]
fn descriptor_element_names_extension_property_in_krusty() {
    // `descriptor.elementNames` / `.elementDescriptors` are CLASSPATH EXTENSION properties (getters
    // `SerialDescriptorKt.getElementNames(d)` / `getElementDescriptors(d)`). krusty resolves a classpath
    // extension property `recv.x` to its static `get<X>(recv)` getter and lowers it to `invokestatic`.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.descriptors.*

@Serializable
class Foo(val a: Int, val b: String)

fun box(): String {
    val names = Foo.serializer().descriptor.elementNames.joinToString()
    if (names != "a, b") return "names=$names"
    val n = Foo.serializer().descriptor.elementDescriptors.count()
    if (n != 2) return "count=$n"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerElemNames") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "elementNames extension property wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty descriptor.elementNames extension property OK");
}

#[test]
fn contextual_serialization_descriptor_kind_in_krusty() {
    // A `@Contextual` property makes its element serializer CONTEXTUAL: krusty emits
    // `ContextualSerializer(<type>::class)`, whose descriptor `kind` is CONTEXTUAL. Uses a plain (non
    // `@Serializable`) user type `Plain` so no JDK type is needed on the harness classpath. The
    // file-level `@file:UseContextualSerialization` + typealias path is covered by the
    // `typealiasesInContextualTest` boxIr corpus case.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.descriptors.*

class Plain(val x: Int)

@Serializable
data class MyClass(@Contextual val p: Plain, @Contextual val q: Plain?)

fun box(): String {
    val d = MyClass.serializer().descriptor
    val k0 = d.getElementDescriptor(0).kind
    val k1 = d.getElementDescriptor(1).kind
    if (k0 != SerialKind.CONTEXTUAL) return "k0=$k0"
    if (k1 != SerialKind.CONTEXTUAL) return "k1=$k1"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerContextual") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "contextual element kinds wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty contextual serialization (descriptor kind CONTEXTUAL) OK");
}

#[test]
fn reified_serializer_free_function_and_data_object_in_krusty() {
    // `serializer<T>()` (the reified free function `kotlinx.serialization.serializer`) can't be called
    // directly (throws at runtime) — krusty desugars it to `T.serializer()`. Also exercises a
    // `@Serializable data object` (the `data object` parse + its generated serializer).
    let src = r#"import kotlinx.serialization.*

@Serializable
class Plain(val x: Int)

@Serializable
data object Obj

fun box(): String {
    if (serializer<Plain>().descriptor.serialName != "Plain") return "plain=" + serializer<Plain>().descriptor.serialName
    if (Obj.serializer().descriptor.serialName != "Obj") return "obj=" + Obj.serializer().descriptor.serialName
    if (serializer<Obj>().descriptor.serialName != "Obj") return "objreified"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerReifiedFree") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "reified serializer<T>() / data object wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty reified serializer<T>() + data object OK");
}

#[test]
fn sealed_typed_field_serializer_in_krusty() {
    // A field whose type is a sealed `@Serializable` base uses `Base.serializer()` (a runtime
    // `SealedClassSerializer`) as its element serializer — `W(val b: Base)` with `b = A(1)` encodes
    // `{"b":{"type":"A","x":1}}` (a sealed class has no `$serializer`, so this was previously a null
    // element serializer → NPE).
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*
@Serializable sealed class Base
@Serializable class A(val x: Int) : Base()
@Serializable class W(val b: Base)
fun box(): String {
    val s = Json.encodeToString(W.serializer(), W(A(1)))
    return if (s == "{\"b\":{\"type\":\"A\",\"x\":1}}") "OK" else s
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerSealedField") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "sealed-typed field serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty sealed-typed field serializer OK");
}

#[test]
fn star_projection_polymorphic_serializer_in_krusty() {
    // A `Box<*>` field (star projection on `Box<T : E>`) derives `Box.serializer(PolymorphicSerializer(
    // E::class))` for its element — the descriptor of the `*` argument is `kotlinx.serialization.
    // Polymorphic<E>`. Mirrors corpus `starProjections`.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.descriptors.*

interface E

@Serializable
class Box<T: E>(val boxed: T)

@Serializable
class Wrapper(val boxed: Box<*>)

fun box(): String {
    val s = Wrapper.serializer().descriptor.elementDescriptors.joinToString()
    return if (s == "Box(boxed: kotlinx.serialization.Polymorphic<E>)") "OK" else s
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerStarProj") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "star-projection polymorphic serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty star-projection polymorphic serializer OK");
}

#[test]
fn abstract_class_field_polymorphic_serializer_in_krusty() {
    // A property whose type is an ABSTRACT `@Serializable` class serializes via open polymorphism:
    // `PolymorphicSerializer(Base::class)`, whose element descriptor `serialName` is
    // `kotlinx.serialization.Polymorphic<Base>`. Covers both a non-null and a nullable such field.
    // Mirrors the `Poly<*>` part of the `starProjectionsSealed` boxIr corpus case.
    let src = r#"import kotlinx.serialization.*

@Serializable
abstract class Base { abstract val v: Int }

@Serializable
class Holder(val b: Base, val c: Base?)

fun box(): String {
    val d = Holder.serializer().descriptor
    val s0 = d.getElementDescriptor(0).serialName
    if (s0 != "kotlinx.serialization.Polymorphic<Base>") return "s0=$s0"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerAbstractPoly") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "abstract-class field polymorphic serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty abstract-class field polymorphic serializer OK");
}

#[test]
fn interface_field_polymorphic_serializer_in_krusty() {
    // A property whose type is an INTERFACE serializes via open polymorphism —
    // `PolymorphicSerializer(Animal::class)` (element descriptor serialName
    // `kotlinx.serialization.Polymorphic<Animal>`) — kotlinx's default for an interface property, no
    // `@Serializable` on the interface required. Covers both a non-null and a nullable interface field.
    let src = r#"import kotlinx.serialization.*

interface Animal

@Serializable
class Zoo(val a: Animal, val b: Animal?)

fun box(): String {
    val d = Zoo.serializer().descriptor
    val s0 = d.getElementDescriptor(0).serialName
    if (s0 != "kotlinx.serialization.Polymorphic<Animal>") return "s0=$s0"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerIfacePoly") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "interface-field polymorphic serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty interface-field polymorphic serializer OK");
}

#[test]
fn sealed_interface_field_serializer_in_krusty() {
    // A property whose type is a `sealed interface` serializes via closed polymorphism — a
    // `SealedClassSerializer` (descriptor kind SEALED) like a `sealed class`, NOT the open
    // `PolymorphicSerializer` (kind OPEN) a plain interface gets. Verifies `sealed interface` carries
    // `is_sealed` through to the element-serializer choice. Mirrors a field of `multipleGenericsPolymorphic`.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.descriptors.*

@Serializable
sealed interface Shape

@Serializable
class Holder(val s: Shape, val t: Shape?)

fun box(): String {
    val d = Holder.serializer().descriptor
    if (d.getElementDescriptor(0).kind.toString() != "SEALED") return d.getElementDescriptor(0).kind.toString()
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerSealedIface") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "sealed-interface field serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty sealed-interface field serializer OK");
}

#[test]
fn classpath_typed_field_serializer_in_krusty() {
    // A `@Serializable` class with a CLASSPATH-typed field (`kotlin.uuid.Uuid`, resolved via a wildcard
    // import) serializes through the kotlinx builtin `UuidSerializer`. Exercises the field-type classpath
    // resolution (`field_ty`: the field decl, ctor param and getter all agree on `Uuid`, not erased `Any`)
    // + the non-null `encodeSerializableElement` path for a builtin-ref element. Mirrors corpus
    // `uuidSerializer`.
    let src = r#"// OPT_IN: kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.*
import kotlinx.serialization.*
import kotlinx.serialization.json.*

@Serializable
class Holder(val u: Uuid)

fun box(): String {
    val h = Holder(Uuid.parse("bc501c76-d806-4578-b45e-97a264e280f1"))
    val s = Json.encodeToString(h)
    return if (s == "{\"u\":\"bc501c76-d806-4578-b45e-97a264e280f1\"}") "OK" else s
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerClasspathField") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "classpath-typed field serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty classpath-typed field serializer OK");
}

#[test]
fn default_value_descriptor_is_optional_in_krusty() {
    // A primary-constructor property with a CONSTANT default (`b: Int = 5`, `t: String? = null`) is an
    // OPTIONAL descriptor element — `descriptor.isElementOptional(i) == true` (matches kotlinc's ABI);
    // a property with no default is not.
    let src = r#"import kotlinx.serialization.*
@Serializable class C(val a: Int, val b: Int = 5, val t: String? = null)
fun box(): String {
    val d = C.serializer().descriptor
    if (d.isElementOptional(0)) return "a should not be optional"
    if (!d.isElementOptional(1)) return "b should be optional"
    if (!d.isElementOptional(2)) return "t should be optional"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerOptional") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "default-value isElementOptional wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty default-value descriptor isOptional OK");
}

#[test]
fn default_value_encode_omission_in_krusty() {
    // An OPTIONAL element (constant default) is OMITTED on encode when it still equals the default
    // (`shouldEncodeElementDefault(desc,i) || value.x != default`), and emitted when it differs —
    // matching kotlinc's default `encodeDefaults=false`.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*
@Serializable class C(val a: Int, val b: Int = 5, val t: String? = null)
fun box(): String {
    val s1 = Json.encodeToString(C.serializer(), C(1))
    if (s1 != "{\"a\":1}") return "s1=$s1"
    val s2 = Json.encodeToString(C.serializer(), C(1, 9, "hi"))
    if (s2 != "{\"a\":1,\"b\":9,\"t\":\"hi\"}") return "s2=$s2"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerEncodeOmit") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "default-value encode-omission wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty default-value encode-omission OK");
}

#[test]
fn default_value_decode_fills_default_in_krusty() {
    // Decoding input that OMITS an optional element fills it from the constant default (the decode local
    // starts at the default, so a never-decoded element keeps it) — `{"a":1}` → `C(1, 5, null)`.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*
@Serializable data class C(val a: Int, val b: Int = 5, val t: String? = null)
fun box(): String {
    val c1 = Json.decodeFromString(C.serializer(), "{\"a\":1}")
    if (c1 != C(1, 5, null)) return "c1=$c1"
    val c2 = Json.decodeFromString(C.serializer(), "{\"a\":1,\"b\":9,\"t\":\"hi\"}")
    if (c2 != C(1, 9, "hi")) return "c2=$c2"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerDecodeDefault") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "default-value decode-fill wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty default-value decode-fill OK");
}

#[test]
fn generic_class_serializer_in_krusty() {
    // A generic `@Serializable class Box<T>(val boxed: T)`: its `$serializer` is a CLASS with one
    // `KSerializer` constructor argument per type parameter; `Box.serializer(Inner.serializer())` builds
    // `new Box$serializer(Inner$serializer.INSTANCE)`, and the `boxed: T` element serializes through that
    // ctor-supplied serializer — `{"boxed":{"n":5}}`.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*

@Serializable class Inner(val n: Int)
@Serializable class Box<T>(val boxed: T)

fun box(): String {
    val s = Json.encodeToString(Box.serializer(Inner.serializer()), Box(Inner(5)))
    return if (s == "{\"boxed\":{\"n\":5}}") "OK" else "enc=$s"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerGeneric") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "generic class serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty generic class serializer OK");
}

#[test]
fn nested_generic_field_serializer_in_krusty() {
    // A class with a NESTED generic field (`Holder(val b: Box<Int>)`): the containing serializer must
    // build `Box.serializer(IntSerializer.INSTANCE)` for that element (caller-side type-argument
    // derivation), recovering the `<Int>` from the field's source type → `{"b":{"boxed":7}}`.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*

@Serializable class Box<T>(val boxed: T)
@Serializable class Holder(val b: Box<Int>)

fun box(): String {
    val s = Json.encodeToString(Holder.serializer(), Holder(Box(7)))
    return if (s == "{\"b\":{\"boxed\":7}}") "OK" else "enc=$s"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerNestedGeneric") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "nested generic field serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty nested generic field serializer OK");
}

#[test]
fn sealed_class_polymorphic_serializer_in_krusty() {
    // A `@Serializable sealed class` base: `Base.serializer()` returns a runtime `SealedClassSerializer`
    // over its `@Serializable` subclasses, so `Json.encodeToString(Base.serializer(), A(1))` emits the
    // polymorphic form `{"type":"A","x":1}` (default `"type"` discriminator = each subclass serialName).
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*

@Serializable
sealed class Base

@Serializable
class A(val x: Int) : Base()

@Serializable
class B(val y: String) : Base()

fun box(): String {
    val a = Json.encodeToString(Base.serializer(), A(1))
    if (a != "{\"type\":\"A\",\"x\":1}") return "a=$a"
    val b = Json.encodeToString(Base.serializer(), B("hi"))
    if (b != "{\"type\":\"B\",\"y\":\"hi\"}") return "b=$b"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerSealed") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "sealed polymorphic serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty sealed polymorphic serializer OK");
}

#[test]
fn property_level_custom_serializer_introspection_in_krusty() {
    // `@Serializable(with = X::class)` on a PROPERTY (not the class): the generated `childSerializers()`
    // must return an instance of `X` for that element (a `new X()` for a no-arg class serializer),
    // wrapped `.nullable` for a nullable property — so `getElementDescriptor(i).serialName` is X's
    // descriptor name (with a trailing `?` when nullable), not an NPE. Mirrors corpus
    // `customFixedNonSerializableArguments`.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.descriptors.*
import kotlinx.serialization.encoding.*

class AnyMapSerializer: KSerializer<Map<String, Any?>> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("AnyMap", PrimitiveKind.STRING)
    override fun serialize(encoder: Encoder, value: Map<String, Any?>) = encoder.encodeString(value.toString())
    override fun deserialize(decoder: Decoder): Map<String, Any?> = emptyMap()
}
@Serializable
data class Test(
    @Serializable(with = AnyMapSerializer::class) val map: Map<String, Any>?,
    @Serializable(with = AnyMapSerializer::class) val map2: Map<String, Any>
)
fun box(): String {
    val d = Test.serializer().descriptor
    if (d.getElementDescriptor(0).serialName != "AnyMap?") return "0=" + d.getElementDescriptor(0).serialName
    if (d.getElementDescriptor(1).serialName != "AnyMap") return "1=" + d.getElementDescriptor(1).serialName
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerPropCustom") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "property-level @Serializable(with=) introspection wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty property-level @Serializable(with=) OK");
}

#[test]
fn serializable_with_object_serializer_in_krusty() {
    // `@Serializable(with = MyObj::class)` where `MyObj` is a user `object : KSerializer<C>`:
    // `C.serializer()` returns `MyObj.INSTANCE` (an object serializer has no ctor). Exercises a user
    // object IMPLEMENTING a classpath generic interface (`object MyObj : KSerializer<C>` now emits
    // `implements KSerializer` + its override members), and the with=-object accessor.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.descriptors.*
import kotlinx.serialization.encoding.*

object MyObj : KSerializer<C> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("my.C", PrimitiveKind.STRING)
    override fun serialize(encoder: Encoder, value: C) { TODO() }
    override fun deserialize(decoder: Decoder): C { TODO() }
}
@Serializable(MyObj::class)
class C(val x: Int)
fun box(): String =
    if (C.serializer().descriptor.serialName == "my.C") "OK" else C.serializer().descriptor.serialName
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerWithObj") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "@Serializable(with=object) wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty @Serializable(with=object) OK");
}

#[test]
fn custom_serializer_object_with_primitive_descriptor_in_krusty() {
    // A user `object X : KSerializer<T>` whose descriptor is `PrimitiveSerialDescriptor(name,
    // PrimitiveKind.STRING)` — exercising classpath nested-object value resolution (`PrimitiveKind.STRING`
    // → getstatic PrimitiveKind$STRING.INSTANCE), wildcard-imported `Encoder`/`Decoder` param types, and
    // a classpath top-level function (`PrimitiveSerialDescriptor`). Mirrors the kotlinx `externalSerialierJava`
    // boxIr case: assert `X.descriptor.toString()`.
    let src = r#"import kotlinx.serialization.KSerializer
import kotlinx.serialization.descriptors.*
import kotlinx.serialization.encoding.*

object MySer : KSerializer<String> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("my.Thing", PrimitiveKind.STRING)
    override fun serialize(encoder: Encoder, value: String) { TODO() }
    override fun deserialize(decoder: Decoder): String { TODO() }
}

fun box(): String {
    return if (MySer.descriptor.toString() == "PrimitiveDescriptor(my.Thing)") "OK"
           else MySer.descriptor.toString()
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerCustomObj") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "custom KSerializer object wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty custom KSerializer object OK");
}

#[test]
fn ambiguous_import_resolves_in_signature_phase() {
    // `Encoder`/`Decoder` collide with `java.beans.Encoder`/`Decoder` once the JDK modules are on the
    // classpath, so the simple name is ambiguity-pruned from the global type seed. An EXPLICIT import
    // must still resolve it — in the SIGNATURE phase (function parameter types), not just the checker.
    // (Prerequisite for custom-serializer files, which declare `serialize(encoder: Encoder, …)`.)
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no stdlib");
        return;
    };
    let Some(core) = find("kotlinx-serialization-core-jvm") else {
        eprintln!("skipping: no serialization core jar");
        return;
    };
    let Some(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())
    else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let modules = PathBuf::from(&java_home).join("lib/modules");
    if !modules.exists() {
        eprintln!("skipping: no JDK lib/modules (needed to reproduce the ambiguity)");
        return;
    }
    // Both explicit AND wildcard imports must resolve the ambiguity-pruned name in the signature phase.
    let explicit = "import kotlinx.serialization.encoding.Encoder\n\
               import kotlinx.serialization.encoding.Decoder\n\
               fun f(e: Encoder, d: Decoder) {}\n\
               fun box(): String = \"OK\"\n";
    let wildcard = "import kotlinx.serialization.encoding.*\n\
               fun f(e: Encoder, d: Decoder) {}\n\
               fun box(): String = \"OK\"\n";
    for (kind, src) in [("explicit", explicit), ("wildcard", wildcard)] {
        let classes = common::compile_in_process(
            src,
            "AmbigImp",
            &[stdlib.clone(), core.clone()],
            Some(&modules),
        );
        assert!(
            classes.is_some(),
            "{kind} import of an ambiguously-named class (Encoder) must resolve in the signature phase \
             even with the JDK on the classpath"
        );
    }
    eprintln!("ambiguous explicit+wildcard imports resolve in signature phase OK");
}

#[test]
fn enum_serializer_entirely_in_krusty() {
    // A `@Serializable enum`'s `serializer()` returns a runtime `EnumSerializer(name, E.values())`
    // (not a generated `$serializer`), so the enum round-trips by entry name: `E.B` → `"B"`.
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
@Serializable
enum class E { A, B }
fun box(): String {
    val s = Json.encodeToString(E.serializer(), E.B)
    if (s != "\"B\"") return "enc: $s"
    return if (Json.decodeFromString(E.serializer(), s) == E.B) "OK" else "dec"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerEnum") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "enum serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty enum serializer OK");
}

#[test]
fn value_class_as_field_entirely_in_krusty() {
    // A `@JvmInline value class` used as a FIELD of a normal `@Serializable` class. krusty unboxes the
    // field to the value class's underlying (`Holder.f: Foo` → `int`), so the serializer encodes/decodes
    // that field AS its underlying — `Holder(Foo(42))` → `{"f":42}`. Mirrors the kotlinx `inlineClasses`
    // boxIr conformance case (its `descriptor.isInline` half is covered above).
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
@Serializable
@JvmInline
value class Foo(val i: Int)
@Serializable
class Holder(val f: Foo)
fun box(): String {
    val s = Json.encodeToString(Holder.serializer(), Holder(Foo(42)))
    return if (s == "{\"f\":42}") "OK" else "enc: $s"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerValueField") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "value-class-as-field wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty value-class-as-field OK");
}

#[test]
fn value_class_inline_serializer_entirely_in_krusty() {
    // A `@JvmInline value class`'s generated serializer uses an `InlinePrimitiveDescriptor`
    // (`descriptor.isInline == true`) and serializes/deserializes inline (encodeInline().encodeInt(),
    // decodeInline().decodeInt()) — so `Foo(42)` round-trips as the bare JSON `42`. (The kotlinx
    // `inlineClasses` corpus case additionally nests a value class in another class, which needs
    // value-class field-representation work beyond the serializer.)
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
@Serializable
@JvmInline
value class Foo(val i: Int)
fun box(): String {
    if (!Foo.serializer().descriptor.isInline) return "not inline"
    val s = Json.encodeToString(Foo.serializer(), Foo(42))
    if (s != "42") return "enc: $s"
    val d = Json.decodeFromString(Foo.serializer(), s)
    return if (d.i == 42) "OK" else "dec: ${d.i}"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerValueClass") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "value-class inline serializer wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty value-class inline serializer OK");
}

#[test]
fn serializable_class_encodes_to_json_entirely_in_krusty() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar located");
        return;
    };
    let (Some(core), Some(json)) = (
        find("kotlinx-serialization-core-jvm"),
        find("kotlinx-serialization-json-jvm"),
    ) else {
        eprintln!("skipping: kotlinx-serialization runtime jars not located");
        return;
    };
    let Some(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())
    else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let java = PathBuf::from(&java_home).join("bin/java");

    let cp_jars = vec![stdlib.clone(), core.clone(), json.clone()];

    // krusty compiles the WHOLE program (no kotlinc): the @Serializable class + $serializer + the
    // serializer() accessor + the Json.encodeToString(...) call.
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
@Serializable
class Foo(val a: Int, val b: String)
fun box(): String = Json.encodeToString(Foo.serializer(), Foo(1, "x"))
"#;
    let Some(classes) = common::compile_in_process(src, "SerRT", &cp_jars, None) else {
        panic!("krusty failed to compile the pure-krusty serialization program");
    };

    let out = std::env::temp_dir().join(format!("krusty_ser_only_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    for (internal, bytes) in &classes {
        let p = out.join(format!("{internal}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    // Reflective launcher: invoke SerRTKt.box() and print the result.
    let launcher = out.join("Run.java");
    std::fs::write(
        &launcher,
        r#"public class Run { public static void main(String[] a) throws Exception {
        System.out.println(Class.forName("SerRTKt").getMethod("box").invoke(null)); } }"#,
    )
    .unwrap();
    let javac = PathBuf::from(&java_home).join("bin/javac");
    assert!(Command::new(&javac)
        .args(["-d", out.to_str().unwrap()])
        .arg(&launcher)
        .status()
        .unwrap()
        .success());

    let run = Command::new(&java)
        .arg("-cp")
        .arg(format!(
            "{}:{}:{}:{}",
            out.display(),
            stdlib.display(),
            core.display(),
            json.display()
        ))
        .arg("Run")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim() == "{\"a\":1,\"b\":\"x\"}",
        "krusty-only serialization encode wrong.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    eprintln!(
        "pure-krusty serialization encode round-trip OK: {}",
        stdout.trim()
    );
    let _ = std::fs::remove_dir_all(&out);
}

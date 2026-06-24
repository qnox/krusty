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

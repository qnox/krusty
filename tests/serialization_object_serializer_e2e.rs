//! A `@Serializable object` is serialized by an `ObjectSerializer`, never by a `$serializer` class.
//!
//! kotlinc gives the object a member `serializer()` returning a cached
//! `ObjectSerializer(serialName, INSTANCE, annotations)` (a synthetic `$cachedSerializer$delegate`
//! read through a private `get$cachedSerializer()`), and every place that needs the object's
//! serializer — a property of the object type, a sealed hierarchy's `object` case — constructs that
//! `ObjectSerializer` in place. krusty generated a `Solo$$serializer` class for it and referenced it
//! from those places, while the class was never emitted: `NoClassDefFoundError` at first use.

use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, member_body, method_instructions, plugin_and_runtime,
};
use super::serialization_test_support::both_compilers_box;

const SOURCE: &str = "import kotlinx.serialization.Serializable\n\
\n\
@Serializable\n\
object Marker\n\
\n\
@Serializable\n\
object Settings {\n\
\x20   val retries = 3\n\
\x20   val tags = listOf(\"a\")\n\
}\n\
\n\
@Serializable\n\
sealed class Signal {\n\
\x20   @Serializable data class Tick(val n: Int) : Signal()\n\
\x20   @Serializable object Idle : Signal()\n\
}\n\
\n\
@Serializable\n\
data class Holder(val marker: Marker, val signals: List<Signal>, val maybe: Marker? = null)\n";

/// The observable contract: each object round-trips as `{}` to its own `INSTANCE`, a sealed
/// hierarchy's `object` case is written with its discriminator, and the descriptor is an `OBJECT`
/// named after the declaration.
#[test]
fn an_object_serializes_under_both_compilers() {
    let src = format!(
        "import kotlinx.serialization.builtins.ListSerializer\n\
         import kotlinx.serialization.json.Json\n\
         {SOURCE}\n\
         fun box(): String {{\n\
         \x20   val signals = ListSerializer(Signal.serializer())\n\
         \x20   val list: List<Signal> = listOf(Signal.Tick(1), Signal.Idle)\n\
         \x20   val text = Json.encodeToString(signals, list)\n\
         \x20   val holder = Json.encodeToString(Holder.serializer(), Holder(Marker, list, Marker))\n\
         \x20   val back = Json.decodeFromString(Holder.serializer(), holder)\n\
         \x20   return listOf(\n\
         \x20       text,\n\
         \x20       Json.decodeFromString(signals, text) == list,\n\
         \x20       Json.encodeToString(Marker.serializer(), Marker),\n\
         \x20       Json.decodeFromString(Settings.serializer(), \"{{}}\") === Settings,\n\
         \x20       holder,\n\
         \x20       back.marker === Marker && back.maybe === Marker,\n\
         \x20       Marker.serializer() === Marker.serializer(),\n\
         \x20       Settings.serializer().descriptor.serialName,\n\
         \x20       Settings.serializer().descriptor.kind,\n\
         \x20   ).joinToString(\" | \")\n\
         }}\n"
    );
    let outcome = both_compilers_box(&src, "object_serializer");
    assert_eq!(
        outcome,
        "[{\"type\":\"Signal.Tick\",\"n\":1},{\"type\":\"Signal.Idle\"}] | true | {} | true | \
         {\"marker\":{},\"signals\":[{\"type\":\"Signal.Tick\",\"n\":1},{\"type\":\"Signal.Idle\"}],\
         \"maybe\":{}} | true | true | Settings | OBJECT"
    );
}

/// `javap -v` of a whole class with what legitimately differs between two builds erased: the file
/// header, the constant pool, and constant-pool indices. Member order, flags, signatures, code,
/// debug tables, `@Metadata` and bootstrap methods all remain.
fn class_shape(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter(|line| {
            !["Classfile ", "Last modified", "SHA-256", "MD5"]
                .iter()
                .any(|header| line.starts_with(header))
        })
        .filter(|line| {
            // A pool entry: `#12 = Utf8 …`.
            line.strip_prefix('#')
                .and_then(|rest| rest.split_once(" = "))
                .is_none_or(|(index, _)| index.trim().parse::<u32>().is_err())
        })
        .map(|line| {
            let mut out = String::with_capacity(line.len());
            let mut chars = line.chars().peekable();
            while let Some(c) = chars.next() {
                out.push(c);
                if c == '#' {
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                }
            }
            out
        })
        .collect()
}

/// The object class itself is kotlinc's: `serializer()` after the declared members, the private
/// synthetic helper and delegate, the lazy initializer, and a `<clinit>` that initializes the
/// delegate after the object's own properties, with kotlinc's line table.
#[test]
fn an_object_class_is_the_one_kotlinc_generates() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    for class in ["Marker", "Settings", "Signal$Idle", "Holder$$serializer"] {
        let Some(built) =
            compare_with_kotlinc_plugin("ObjectSerializer", SOURCE, class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        assert!(
            !built.krusty.contains("$$serializer") || class.ends_with("$$serializer"),
            "{class}: an object has no generated serializer class"
        );
        let want = class_shape(&built.reference);
        let got = class_shape(&built.krusty);
        let first_difference = want
            .iter()
            .zip(&got)
            .position(|(want, got)| want != got)
            .unwrap_or(want.len().min(got.len()));
        assert_eq!(
            got, want,
            "{class}: class shape, first difference at line {first_difference}"
        );
    }
}

/// Where an object is an element, kotlinc constructs its `ObjectSerializer` in place: in a
/// property's cached child serializer and in a sealed hierarchy's case serializers.
#[test]
fn an_object_element_constructs_its_object_serializer_in_place() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) =
        compare_with_kotlinc_plugin("ObjectSerializer", SOURCE, "Holder", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "_childSerializers$_anonymous_()",
        "_childSerializers$_anonymous_$0()",
        "_childSerializers$_anonymous_$1()",
    ] {
        let want = member_body(&built.reference, member);
        assert!(
            want.len() > 1,
            "Holder.{member}: the reference must declare it"
        );
        assert_eq!(member_body(&built.krusty, member), want, "Holder.{member}");
    }
    assert!(
        built
            .reference
            .contains("kotlinx/serialization/internal/ObjectSerializer"),
        "the reference constructs an ObjectSerializer for the object property"
    );
}

/// A sealed hierarchy's `object` case is registered with an `ObjectSerializer` constructed in place,
/// not through a serializer class. (Where the sealed serializer itself lives differs: kotlinc caches
/// it on the base class, krusty builds it in the companion's `serializer()`; this compares the case.)
#[test]
fn a_sealed_object_case_constructs_its_object_serializer_in_place() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let build = |class: &str, method: &str, reference: bool| {
        let built =
            compare_with_kotlinc_plugin("ObjectSerializer", SOURCE, class, &cp, "25", &extra)?;
        let disassembly = if reference {
            built.reference
        } else {
            built.krusty
        };
        let instructions: Vec<String> = method_instructions(&disassembly, method)
            .iter()
            .map(|row| {
                row.split_once(": ")
                    .map_or(row.as_str(), |(_, i)| i)
                    .to_string()
            })
            .collect();
        let start = instructions
            .iter()
            .position(|i| i.ends_with("class kotlinx/serialization/internal/ObjectSerializer"))
            .unwrap_or_else(|| panic!("{class}.{method}: no ObjectSerializer: {instructions:?}"));
        Some(instructions[start..(start + 7).min(instructions.len())].to_vec())
    };
    let Some(want) = build("Signal", "_init_$_anonymous_()", true) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let Some(got) = build("Signal$Companion", "serializer()", false) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert!(
        want.iter().any(|i| i.contains("Signal$Idle.INSTANCE")),
        "the reference registers the object case: {want:?}"
    );
    assert_eq!(got, want, "the object case's serializer construction");
}

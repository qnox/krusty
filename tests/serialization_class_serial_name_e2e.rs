//! A class-level `@SerialName` names the class itself in the serial form.
//!
//! `@SerialName("created") data class Created(…) : Event()` is the discriminator value a sealed
//! hierarchy writes for that subclass — `{"type":"created",…}` — and the `serialName` of every
//! descriptor built for the class, an enum or a sealed base. krusty read `@SerialName` from
//! properties and enum entries only, so every class-level one was ignored: the discriminator was the
//! qualified name `Event.Created`, and a document kotlinc wrote could not be read back.

use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions, plugin_and_runtime,
};
use super::serialization_test_support::both_compilers_box;

const SOURCE: &str = "import kotlinx.serialization.SerialName\n\
import kotlinx.serialization.Serializable\n\
\n\
@Serializable\n\
@SerialName(\"ev\")\n\
sealed class Event {\n\
\x20   @Serializable @SerialName(\"created\") data class Created(val at: Long) : Event()\n\
\x20   @Serializable @SerialName(\"moved\") data class Moved(val to: String) : Event()\n\
}\n\
\n\
@Serializable @SerialName(\"pt\") data class Point(val x: Int)\n\
@Serializable @SerialName(\"lvl\") enum class Level { LOW, HIGH }\n\
@Serializable @SerialName(\"box\") data class Box<T>(val v: T)\n";

/// The observable contract: the discriminator each subclass writes, a round trip through it, and
/// the `serialName` of each kind of generated descriptor.
#[test]
fn a_class_serial_name_is_the_serial_form_under_both_compilers() {
    let src = format!(
        "import kotlinx.serialization.builtins.ListSerializer\n\
         import kotlinx.serialization.json.Json\n\
         {SOURCE}\n\
         fun box(): String {{\n\
         \x20   val serializer = ListSerializer(Event.serializer())\n\
         \x20   val events: List<Event> = listOf(Event.Created(1), Event.Moved(\"b\"))\n\
         \x20   val text = Json.encodeToString(serializer, events)\n\
         \x20   val back = Json.decodeFromString(serializer, text)\n\
         \x20   return listOf(\n\
         \x20       text,\n\
         \x20       back.toString(),\n\
         \x20       Point.serializer().descriptor.serialName,\n\
         \x20       Level.serializer().descriptor.serialName,\n\
         \x20       Box.serializer(Point.serializer()).descriptor.serialName,\n\
         \x20       Event.serializer().descriptor.serialName,\n\
         \x20   ).joinToString(\" | \")\n\
         }}\n"
    );
    let outcome = both_compilers_box(&src, "class_serial_name");
    assert_eq!(
        outcome,
        "[{\"type\":\"created\",\"at\":1},{\"type\":\"moved\",\"to\":\"b\"}] | \
         [Created(at=1), Moved(to=b)] | pt | lvl | box | ev"
    );
}

/// The name is a constant of the generated class, loaded where kotlinc loads it: the
/// `$serializer`'s descriptor, the enum's serializer factory and a generic class's cached
/// descriptor. The generic class is compared in its class initializer, where that descriptor is
/// built; the rest of it is not what this test is about.
#[test]
fn a_class_serial_name_is_the_constant_kotlinc_loads() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    for (class, method, expected) in [
        ("Point$$serializer", None, "pt"),
        ("Event$Created$$serializer", None, "created"),
        ("Level", None, "lvl"),
        ("Box", Some("static {}"), "box"),
    ] {
        let Some(built) =
            compare_with_kotlinc_plugin("ClassSerialName", SOURCE, class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let strings = |disassembly: &str| {
            let rows = match method {
                Some(method) => method_instructions(disassembly, method),
                None => disassembly.lines().map(str::to_string).collect(),
            };
            rows.iter()
                .filter(|line| line.contains(": ldc"))
                .filter_map(|line| line.split_once("// String "))
                .map(|(_, value)| value.trim().to_string())
                .collect::<Vec<_>>()
        };
        let want = strings(&built.reference);
        assert!(
            want.iter().any(|value| value == expected),
            "{class}: the reference must load the serial name {expected}: {want:?}"
        );
        assert_eq!(
            strings(&built.krusty),
            want,
            "{class}: loaded string constants"
        );
    }
}

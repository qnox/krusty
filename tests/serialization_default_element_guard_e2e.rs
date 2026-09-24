//! A defaulted element's write is guarded by a SHORT-CIRCUIT disjunction.
//!
//! kotlinx.serialization writes a property that has a default only when the encoder asks for
//! defaults or the value differs from the default:
//!
//! ```text
//! if (output.shouldEncodeElementDefault(desc, i) || self.x != <default>) { … }
//! ```
//!
//! Both compilers agree on the SEMANTICS — a round trip through `Json` produces the same text on
//! either — but krusty built that condition with `IrBinOp::Or`, the EAGER form, which holds the
//! left operand in a temporary and combines with `ior`. kotlinc branches: ask the encoder, answer
//! `true` without reading the property at all, otherwise fall through to the comparison.
//!
//! Every defaulted property of every `@Serializable` class carries one of these, so the two
//! compilers' `write$Self` differed on essentially every generated model class in a real project.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

/// The instruction rows of one method, pool indices erased.
fn method_body(disassembly: &str, marker: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in disassembly.lines() {
        let line = raw.trim();
        if line.contains(marker) && line.ends_with(");") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.starts_with("LineNumberTable") || line.starts_with("LocalVariableTable") {
            break;
        }
        let Some((pc, rest)) = line.split_once(": ") else {
            continue;
        };
        if pc.parse::<u32>().is_err() {
            continue;
        }
        out.push(format!("{pc}: {}", regex_free_erase_pool(rest)));
    }
    out
}

/// Erase `#123` constant-pool indices without pulling in a regex dependency.
fn regex_free_erase_pool(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '#' {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        }
    }
    out
}

#[test]
fn a_defaulted_element_is_guarded_the_way_kotlinc_guards_it() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    // A primitive default and a nullable-reference default: the comparison against the default is
    // a different instruction in each, and both sit under the same guard.
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Opt(val a: Int = 1, val b: String? = null)\n";
    let Some(built) =
        compare_with_kotlinc_plugin("DefaultElementGuard", src, "Opt", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_body(&built.reference, "write$Self$main");
    assert!(
        want.iter()
            .any(|row| row.contains("shouldEncodeElementDefault")),
        "the fixture must exercise the defaulted-element guard: {want:?}"
    );
    assert!(
        !want.iter().any(|row| row.contains(": ior")),
        "kotlinc's guard branches rather than combining with `ior`: {want:?}"
    );
    assert_eq!(
        method_body(&built.krusty, "write$Self$main"),
        want,
        "Opt.write$Self$main"
    );
}

/// A default that is not a constant — an expression over an earlier property — is still a default.
/// kotlinc re-evaluates the initializer in `write$Self`, reading the earlier property off the object
/// being written, and skips the element when the value still equals it:
///
/// ```text
/// if (output.shouldEncodeElementDefault(desc, i) || self.next != self.x + 1) { … }
/// ```
///
/// krusty guarded constant defaults only, so every computed default was written unconditionally and
/// `Json.encodeToString` (which does not encode defaults) produced a different document.
#[test]
fn a_computed_default_is_guarded_the_way_kotlinc_guards_it() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Computed(val x: Int, val next: Int = x + 1, val scaled: Long = x * 2L)\n";
    let Some(built) =
        compare_with_kotlinc_plugin("ComputedDefaultGuard", src, "Computed", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_body(&built.reference, "write$Self$main");
    assert_eq!(
        want.iter()
            .filter(|row| row.contains("shouldEncodeElementDefault"))
            .count(),
        2,
        "the fixture must exercise one guard per computed default: {want:?}"
    );
    assert_eq!(
        method_body(&built.krusty, "write$Self$main"),
        want,
        "Computed.write$Self$main"
    );
}

/// The observable half: with `Json`'s default configuration (`encodeDefaults = false`) an element
/// still holding its computed default is left out, and decoding the shorter document rebuilds it.
#[test]
fn a_computed_default_is_omitted_from_json_like_kotlinc() {
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.json.Json\n\
               \n\
               @Serializable\n\
               data class Computed(\n\
               \x20   val x: Int,\n\
               \x20   val tags: List<String> = listOf(\"a\"),\n\
               \x20   val next: Int = x + 1,\n\
               \x20   val label: String = \"n$x\",\n\
               )\n\
               \n\
               fun box(): String {\n\
               \x20   val defaults = Json.encodeToString(Computed.serializer(), Computed(1))\n\
               \x20   val changed = Json.encodeToString(Computed.serializer(), Computed(1, listOf(\"b\"), 5, \"z\"))\n\
               \x20   val all = Json { encodeDefaults = true }.encodeToString(Computed.serializer(), Computed(1))\n\
               \x20   val back = Json.decodeFromString(Computed.serializer(), defaults)\n\
               \x20   return \"$defaults $changed $all $back\"\n\
               }\n";
    let outcome = super::serialization_test_support::both_compilers_box(src, "computed_default");
    assert_eq!(
        outcome,
        "{\"x\":1} {\"x\":1,\"tags\":[\"b\"],\"next\":5,\"label\":\"z\"} \
         {\"x\":1,\"tags\":[\"a\"],\"next\":2,\"label\":\"n1\"} \
         Computed(x=1, tags=[a], next=2, label=n1)"
    );
}

/// A property declared in the class body with an initializer is a default too. kotlinc marks its
/// element optional in the descriptor, leaves it out of the missing-field mask, applies the
/// initializer in the deserialization constructor when the element is absent (reading an earlier
/// property off the object it is building), and compares against it in `write$Self`.
#[test]
fn a_body_property_initializer_is_a_default_the_way_kotlinc_treats_it() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               fun seed() = 5\n\
               @Serializable\n\
               class Counted(val x: Int, val step: Int = x + 1) {\n\
               \x20   var hits: Int = x * 2\n\
               \x20   val limit: Long = seed() + 10L\n\
               \x20   val tags = listOf(\"a\")\n\
               }\n";
    for class in ["Counted", "Counted$$serializer"] {
        let Some(built) =
            compare_with_kotlinc_plugin("BodyInitializerDefault", src, class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let methods: &[&str] = if class == "Counted" {
            &["SerializationConstructorMarker"]
        } else {
            // The descriptor: each body property's element is optional.
            &["Counted$$serializer()"]
        };
        for &method in methods {
            let want = method_body(&built.reference, method);
            assert!(want.len() > 1, "{class}: the reference declares {method}");
            assert_eq!(
                method_body(&built.krusty, method),
                want,
                "{class}: {method}"
            );
        }
        if class != "Counted" {
            continue;
        }
        // `write$Self`, up to the `tags` guard: the primitive comparisons are exact. `tags` is
        // compared with `Intrinsics.areEqual`, whose negation is emitted differently (a separate
        // gap), so past its guard only the re-evaluated initializer is checked.
        let before_tags = |rows: Vec<String>| -> Vec<String> {
            let tags = rows
                .iter()
                .position(|row| row.contains("Intrinsics.areEqual"))
                .expect("the tags element is compared");
            let guard = rows[..tags]
                .iter()
                .rposition(|row| row.contains("shouldEncodeElementDefault"))
                .expect("the tags element is guarded");
            rows[..guard].to_vec()
        };
        let want = method_body(&built.reference, "write$Self$main");
        let got = method_body(&built.krusty, "write$Self$main");
        assert_eq!(
            want.iter()
                .filter(|row| row.contains("shouldEncodeElementDefault"))
                .count(),
            4,
            "one guard per defaulted element: {want:?}"
        );
        assert_eq!(
            before_tags(got.clone()),
            before_tags(want),
            "Counted.write$Self$main"
        );
        assert!(
            got.iter().any(|row| row.contains("CollectionsKt.listOf")),
            "write$Self compares tags with its re-evaluated initializer: {got:?}"
        );
    }
}

/// The observable half: an untouched body property is left out of the document, a changed one is
/// written, and decoding a document without it runs its initializer, reading earlier properties
/// and the receiver as the primary constructor does.
#[test]
fn a_body_property_initializer_is_omitted_and_restored_like_kotlinc() {
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.json.Json\n\
               \n\
               @Serializable\n\
               class Profile(val id: Int, val name: String = \"n$id\") {\n\
               \x20   val tags = listOf(\"a\", name)\n\
               \x20   var score: Int = id * 2\n\
               \x20   val label: String = run { val t = tags.size; \"$name:$t\" }\n\
               \x20   val alias = tags.firstOrNull() ?: \"none\"\n\
               \x20   val size = run { val n = name.length; if (n == 0) -1 else n + score }\n\
               \x20   override fun toString() = \"$id $name $tags $score $label $alias $size\"\n\
               }\n\
               \n\
               fun box(): String {\n\
               \x20   val profile = Profile(3)\n\
               \x20   val untouched = Json.encodeToString(Profile.serializer(), profile)\n\
               \x20   profile.score = 9\n\
               \x20   val changed = Json.encodeToString(Profile.serializer(), profile)\n\
               \x20   val all = Json { encodeDefaults = true }.encodeToString(Profile.serializer(), profile)\n\
               \x20   val restored = Json.decodeFromString(Profile.serializer(), \"{\\\"id\\\":4}\")\n\
               \x20   val partial = Json.decodeFromString(\n\
               \x20       Profile.serializer(), \"{\\\"id\\\":4,\\\"name\\\":\\\"q\\\",\\\"score\\\":1}\",\n\
               \x20   )\n\
               \x20   return listOf(untouched, changed, all, restored, partial).joinToString(\" | \")\n\
               }\n";
    let outcome = super::serialization_test_support::both_compilers_box(src, "body_initializer");
    assert_eq!(
        outcome,
        "{\"id\":3} | {\"id\":3,\"score\":9,\"size\":8} | \
         {\"id\":3,\"name\":\"n3\",\"tags\":[\"a\",\"n3\"],\"score\":9,\"label\":\"n3:2\",\
         \"alias\":\"a\",\"size\":8} | 4 n4 [a, n4] 8 n4:2 a 10 | 4 q [a, q] 1 q:2 a 2"
    );
}

//! A nullable value class held as its carrier keeps its nullability on the JVM declarations.
//!
//! `T?` over a non-null reference underlying (`value class T(val s: String)`) needs no box: the
//! carrier `String` holds `T`'s `null`. The projection onto the carrier dropped the `?`, so every
//! declaration of that type read as a non-null `String`:
//!
//! ```text
//! kotlinc:  @Nullable private final String t;   @Nullable public final String getT-…();
//! krusty:   @NotNull  private final String t;   @NotNull  public final String getT-…();
//! ```
//!
//! — fields, accessors, parameters and returns alike, and a `var`'s setter checked its argument
//! for null.
use super::common;
use super::common::compare_with_kotlinc_plugin;

const SOURCE: &str = "@JvmInline value class T(val s: String)\n\
    class H(val t: T?, var u: T? = null) {\n\
    \x20   fun f(x: T?): T? = x ?: t\n\
    }\n\
    fun g(x: T?): T? = x\n";

/// Each non-constructor member's header with the nullability annotations javap lists under it.
fn member_nullability(disassembly: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in disassembly.lines() {
        let member_header = line.starts_with("  ")
            && !line.starts_with("   ")
            && !line.trim_start().starts_with('#')
            && line.trim_end().ends_with(';');
        if member_header {
            out.push((line.trim().to_string(), Vec::new()));
        } else if let Some(annotation) = line.trim().strip_prefix("org.jetbrains.annotations.") {
            if let Some((_, annotations)) = out.last_mut() {
                annotations.push(annotation.to_string());
            }
        }
    }
    // Constructors differ for an unrelated reason (a hidden constructor's annotations).
    out.retain(|(header, _)| !header.contains(" H(") && !header.starts_with("H("));
    out
}

#[test]
fn a_nullable_value_class_carrier_is_annotated_nullable() {
    for class in ["H", "NullableValueCarrierKt"] {
        let Some(built) = compare_with_kotlinc_plugin(
            "NullableValueCarrier",
            SOURCE,
            class,
            &[common::stdlib_jar()],
            "25",
            &[],
        ) else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let reference = member_nullability(&built.reference);
        assert!(
            reference
                .iter()
                .any(|(_, annotations)| annotations.iter().any(|a| a == "Nullable")),
            "{class}: {reference:?}"
        );
        assert_eq!(member_nullability(&built.krusty), reference, "{class}");
    }
}

#[test]
fn a_nullable_value_class_carrier_runs() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   val h = H(T(\"a\"), null)\n\
         \x20   if (h.f(null)?.s != \"a\") return \"FAIL elvis\"\n\
         \x20   if (g(null) != null) return \"FAIL null\"\n\
         \x20   h.u = null\n\
         \x20   h.u = T(\"b\")\n\
         \x20   return if (h.u?.s == \"b\") \"OK\" else \"FAIL var\"\n\
         }}\n"
    );
    common::expect_box_ok_with_stdlib(&src, "a nullable value class carried unboxed");
}

/// A non-null value class whose carrier admits null is still `@NotNull` where it is declared.
/// `val b: N` over `value class N(val s: String?)` stores a `String?`, and `val a: Boxed<String>`
/// stores the `Object` its `T` erases to; kotlinc annotates the backing field, the getter and a
/// data class's `copy` parameter and `componentN` by the property's declared type, not that
/// storage.
const DECLARED_SOURCE: &str = "@JvmInline value class Boxed<T>(val value: T)\n\
    @JvmInline value class N(val s: String?)\n\
    @JvmInline value class NI(val x: Int?)\n\
    @JvmInline value class I(val x: Int)\n\
    class Holder(val a: Boxed<String>, val b: N, val c: Boxed<String?>, val d: N?)\n\
    data class Record(val a: Boxed<String>, val b: N, val i: I, val ni: NI, var v: N, val n: N?)\n";

#[test]
fn a_value_class_over_a_nullable_carrier_is_annotated_by_its_declared_type() {
    for class in ["Holder", "Record"] {
        match common::byte_diff_against_kotlinc_cp(
            "ValueClassDeclaredNullability",
            DECLARED_SOURCE,
            class,
            &[common::stdlib_jar()],
        ) {
            None => {
                eprintln!("skip (ValueClassDeclaredNullability: reference toolchain unavailable)")
            }
            Some(Ok(())) => {}
            Some(Err(why)) => panic!("{class}: {why}"),
        }
    }
}

/// The same rule on storage that is not an instance field of an ordinary class: a top-level
/// property's facade field, a named object's field, an enum's field and a companion property
/// hoisted onto its outer class. A top-level `var` of such a type keeps no setter guard, because
/// the stored `String?` admits null.
const DECLARED_STATIC_SOURCE: &str = "@JvmInline value class Boxed<T>(val value: T)\n\
    @JvmInline value class N(val s: String?)\n\
    val topB: N = N(null)\n\
    val topA: Boxed<String> = Boxed(\"a\")\n\
    var topV: N = N(\"v\")\n\
    object Single { val b: N = N(null); val a: Boxed<String> = Boxed(\"a\") }\n\
    enum class Kind(val b: N) { X(N(null)) }\n\
    class Outer { companion object { val b: N = N(null) } }\n";

#[test]
fn value_class_typed_statics_are_annotated_by_their_declared_type() {
    for class in ["Single", "Outer"] {
        match common::byte_diff_against_kotlinc_cp(
            "ValueClassDeclaredStaticNullability",
            DECLARED_STATIC_SOURCE,
            class,
            &[common::stdlib_jar()],
        ) {
            None => eprintln!(
                "skip (ValueClassDeclaredStaticNullability: reference toolchain unavailable)"
            ),
            Some(Ok(())) => {}
            Some(Err(why)) => panic!("{class}: {why}"),
        }
    }
    // The facade and the enum differ from kotlinc only in constant-pool order, which is a separate
    // gap; their members' annotations and guards must already agree.
    for class in ["ValueClassDeclaredStaticNullabilityKt", "Kind"] {
        let Some(built) = compare_with_kotlinc_plugin(
            "ValueClassDeclaredStaticNullability",
            DECLARED_STATIC_SOURCE,
            class,
            &[common::stdlib_jar()],
            "25",
            &[],
        ) else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        assert_eq!(
            member_annotations_and_guards(&built.krusty),
            member_annotations_and_guards(&built.reference),
            "{class}"
        );
    }
}

/// Every member header with the nullability annotations and parameter guards javap lists under it.
fn member_annotations_and_guards(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .filter(|line| {
            let member_header = line.starts_with("  ")
                && !line.starts_with("   ")
                && !line.trim_start().starts_with('#')
                && line.trim_end().ends_with(';');
            member_header
                || line.trim().starts_with("org.jetbrains.annotations.")
                || line.contains("checkNotNullParameter:")
        })
        .map(|line| match line.find("// Method") {
            Some(at) => line[at..].to_string(),
            None => line.trim().to_string(),
        })
        .collect()
}

/// An explicit backing field declares its own type: `val x: Any? field: String` stores a non-null
/// `String`, so the field is `@NotNull` while the property is `Any?`.
#[test]
fn an_explicit_backing_field_is_annotated_by_its_own_type() {
    let settings = krusty::language_settings::LanguageSettings::new(
        krusty::language_version::LanguageVersion::default(),
        None,
        &[krusty::features::FeatureSetting {
            feature: "ExplicitBackingFields".to_string(),
            enabled: true,
        }],
    )
    .expect("explicit backing fields enable under the default language version");
    let Some(built) = common::compare_with_kotlinc_plugin_language_settings(
        "ExplicitBackingFieldNullability",
        "class Backed {\n    val x: Any?\n        field: String = \"x\"\n}\n",
        "Backed",
        &[common::stdlib_jar()],
        "25",
        &settings,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let fields = |disassembly: &str| {
        let lines: Vec<&str> = disassembly.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.trim() == "private final java.lang.String x;")
            .expect("the backing field is listed");
        lines[at..]
            .iter()
            .take_while(|line| !line.trim().starts_with("public "))
            .filter(|line| line.trim().starts_with("org.jetbrains.annotations."))
            .map(|line| line.trim().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        fields(&built.reference),
        ["org.jetbrains.annotations.NotNull"]
    );
    assert_eq!(fields(&built.krusty), fields(&built.reference));
}

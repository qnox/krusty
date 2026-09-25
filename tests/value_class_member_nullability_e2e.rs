//! Which members of a value class carry `@NotNull`/`@Nullable`, as kotlinc decides it.
//!
//! kotlinc annotates the declared surface only:
//! - the structural `toString`/`hashCode`/`equals`, their static `-impl` bodies and `equals-impl0`
//!   are generated and carry no nullability annotation;
//! - a member lowered to a static `-impl` over the carrier leaves its former receiver (parameter 0)
//!   unannotated, while its own parameters and return keep theirs;
//! - the primary `constructor-impl` annotates its parameter as well as its return;
//! - the private synthetic `<init>` carries none.
//!
//! krusty annotated every reference in all of them, and dropped the `constructor-impl` parameter's.
use super::common;
use super::common::compare_with_kotlinc_plugin;

/// Every member header with the nullability annotations javap lists under it, in order.
fn member_nullability(disassembly: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in disassembly.lines() {
        let member_header = line.starts_with("  ")
            && !line.starts_with("   ")
            && line
                .trim_start()
                .starts_with(|c: char| c.is_ascii_alphabetic())
            && line.trim_end().ends_with(';');
        if member_header {
            out.push((line.trim().to_string(), Vec::new()));
        } else if let Some((_, annotations)) = out.last_mut() {
            let entry = line.trim();
            if let Some(annotation) = entry.strip_prefix("org.jetbrains.annotations.") {
                annotations.push(annotation.to_string());
            } else if entry.starts_with("parameter ") {
                annotations.push(entry.to_string());
            }
        }
    }
    out
}

#[test]
fn a_value_class_annotates_only_its_declared_surface() {
    for (tag, source, class) in [
        (
            "ValueClassNullabilityPlain",
            "@JvmInline value class N(val n: Int)\n",
            "N",
        ),
        (
            "ValueClassNullabilityMembers",
            "@JvmInline value class S(val s: String) {\n\
             \x20   init { require(s.isNotEmpty()) }\n\
             \x20   val len: Int get() = s.length\n\
             \x20   fun twice(): String = s + s\n\
             \x20   fun join(other: String?): String = s + other\n\
             \x20   companion object { fun of(x: String) = S(x) }\n\
             }\n",
            "S",
        ),
    ] {
        let Some(built) =
            compare_with_kotlinc_plugin(tag, source, class, &[common::stdlib_jar()], "25", &[])
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let mut krusty = member_nullability(&built.krusty);
        let mut reference = member_nullability(&built.reference);
        // Member order is compared elsewhere; this compares each member's annotations.
        krusty.sort();
        reference.sort();
        assert_eq!(krusty, reference, "{class}");
    }
}

#[test]
fn repository_emission_records_the_exact_value_class_annotation_policy() {
    use krusty::jvm::classreader::JavaNullability::{NotNull, Nullable};

    let classes = common::expect_classes_with_stdlib(
        "@JvmInline value class FixtureTicket(val text: String) {\n\
         \x20   fun join(other: String?): String = text + other\n\
         }\n",
        "ValueClassNullabilityRepository",
    );
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "FixtureTicket").then_some(bytes.as_slice()))
        .expect("FixtureTicket class");
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse FixtureTicket");
    let method = |name: &str, descriptor: &str| {
        class
            .methods
            .iter()
            .find(|method| method.name == name && method.descriptor == descriptor)
            .unwrap_or_else(|| panic!("missing {name}{descriptor}"))
    };

    let primary = method("<init>", "(Ljava/lang/String;)V");
    assert_eq!(primary.return_nullability, None);
    assert_eq!(primary.parameter_nullability, Vec::new());

    let constructor = method("constructor-impl", "(Ljava/lang/String;)Ljava/lang/String;");
    assert_eq!(constructor.return_nullability, Some(NotNull));
    assert_eq!(constructor.parameter_nullability, vec![Some(NotNull)]);

    let structural = method("toString-impl", "(Ljava/lang/String;)Ljava/lang/String;");
    assert_eq!(structural.return_nullability, None);
    assert_eq!(structural.parameter_nullability, Vec::new());

    let declared = method(
        "join-impl",
        "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
    );
    assert_eq!(declared.return_nullability, Some(NotNull));
    assert_eq!(declared.parameter_nullability, vec![None, Some(Nullable)]);
}

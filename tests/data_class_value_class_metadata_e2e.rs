//! `@Metadata` of a data class whose properties are value classes.
//!
//! Value-class mangling renames the synthesized `componentN`/`copy` (`component3-fs9h3jg`,
//! `copy-lpbsxsU`). kotlinc describes each once, under its source name, with a
//! `JvmMethodSignature` carrying the mangled name — and the descriptor too when the value class
//! erases to its carrier. krusty described the synthesized member without the signature name and
//! then described the renamed method a second time as a declared function, so `d1` grew extra
//! function records and `d2` gained the mangled names at its end.
use super::common;
use super::common::compare_with_kotlinc_plugin;

/// The `d1`/`d2` lines of the class's `@Metadata`.
fn metadata(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("d1=") || line.starts_with("d2="))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_data_class_over_value_classes_describes_each_member_once() {
    let source = "@JvmInline value class FixtureText(val text: String)\n\
                  @JvmInline value class FixtureCount(val count: Int)\n\
                  @JvmInline value class FixtureMaybeText(val text: String?)\n\
                  data class FixtureNullable(val a: Int? = null, val m: String? = null, val text: FixtureText? = null)\n\
                  data class FixturePair(val text: FixtureText, val count: FixtureCount)\n\
                  data class FixtureMixed(val count: FixtureCount?, val text: FixtureMaybeText, val maybe: FixtureMaybeText?, val x: Long)\n\
                  data class FixtureDefault(val a: String, val text: FixtureText = FixtureText(\"x\"))\n";
    for class in [
        "FixtureNullable",
        "FixturePair",
        "FixtureMixed",
        "FixtureDefault",
    ] {
        let Some(built) = compare_with_kotlinc_plugin(
            "DataClassValueClassMetadata",
            source,
            class,
            &[common::stdlib_jar()],
            "25",
            &[],
        ) else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let reference = metadata(&built.reference);
        assert_eq!(reference.len(), 2, "{class}: {}", built.reference);
        assert_eq!(metadata(&built.krusty), reference, "{class}");
    }
}

#[test]
fn repository_emission_binds_each_data_member_to_one_exact_jvm_realization() {
    let source = "@JvmInline value class FixtureText(val text: String)\n\
                  @JvmInline value class FixtureCount(val count: Int)\n\
                  data class FixtureRecord(val text: FixtureText, val count: FixtureCount)\n";
    let classes = common::compile_in_process_metadata_cp(
        source,
        "DataClassMemberIdentity",
        &[common::stdlib_jar()],
    )
    .expect("the repository compiler emits the fixture with class metadata");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "FixtureRecord").then_some(bytes))
        .expect("FixtureRecord.class is emitted");
    let class = krusty::jvm::classreader::parse_class(bytes).expect("FixtureRecord.class parses");
    let functions = &class.meta.class_functions;
    assert_eq!(
        functions
            .iter()
            .map(|function| function.kotlin_name.as_str())
            .collect::<Vec<_>>(),
        [
            "component1",
            "component2",
            "copy",
            "equals",
            "hashCode",
            "toString",
        ]
    );

    for (source_name, descriptor) in [
        ("component1", "()Ljava/lang/String;"),
        ("component2", "()I"),
        ("copy", "(Ljava/lang/String;I)LFixtureRecord;"),
    ] {
        let exact = functions
            .iter()
            .filter(|function| function.kotlin_name == source_name)
            .collect::<Vec<_>>();
        let [function] = exact.as_slice() else {
            panic!("one metadata declaration must own {source_name}: {exact:?}")
        };
        assert_ne!(function.jvm_name, function.kotlin_name);
        assert_eq!(function.jvm_desc, Some(descriptor));
        assert_eq!(
            class
                .methods
                .iter()
                .filter(|method| {
                    method.name == function.jvm_name && method.descriptor == descriptor
                })
                .count(),
            1,
            "{source_name} names one exact physical method"
        );
    }
}

//! Secondary constructors are source declarations. Their JVM methods must be interleaved with
//! property accessors and functions by the same stable source-order key; `ACC_SYNTHETIC` is only a
//! representation flag and must not move a source constructor into a generated-member phase.

use super::common;

const SOURCE: &str = r#"
class Mixed(val seed: Int) {
    val before: Int get() = seed

    @Deprecated("source synthetic", level = DeprecationLevel.HIDDEN)
    constructor(seed: Int, tag: String) : this(seed)

    fun after(): String = seed.toString()
}
"#;

const VALUE_CLASS_SOURCE: &str = r#"
@JvmInline
value class InlineOrder(val seed: Int) {
    val before: Int get() = seed

    constructor(tag: String) : this(tag.length)

    fun after(): String = seed.toString()
}
"#;

fn member_declarations(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.ends_with(';')
                && !line.starts_with('#')
                && !line.contains(':')
                && line.contains('(')
        })
        .map(str::to_owned)
        .collect()
}

fn value_class_source_member_identities(disassembly: &str) -> Vec<String> {
    const SOURCE_IDENTITIES: &[&str] = &[
        "getSeed();",
        "getBefore-impl(int);",
        "constructor-impl(java.lang.String);",
        "after-impl(int);",
    ];
    member_declarations(disassembly)
        .into_iter()
        .filter_map(|declaration| declaration.split_whitespace().last().map(str::to_owned))
        .filter(|identity| SOURCE_IDENTITIES.contains(&identity.as_str()))
        .collect()
}

fn build_both(source_text: &str, stem: &str, class: &str) -> Option<(String, String)> {
    let root = common::scratch_dir()?;
    let reference_dir = root.join("reference");
    let krusty_dir = root.join("krusty");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let source = root.join(format!("{stem}.kt"));
    std::fs::write(&source, source_text).expect("write fixture");

    let (status, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(status, 0, "kotlinc failed: {stderr}");

    let stdlib = common::stdlib_jar();
    let classes = common::compile_in_process_metadata_cp_module_target(
        source_text,
        stem,
        &[stdlib],
        "main",
        Some(69),
    )
    .expect("krusty compiles the fixture");
    for (internal, bytes) in classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("krusty class directory");
        }
        std::fs::write(path, bytes).expect("write krusty class");
    }

    let reference = common::javap(&["-p", "-cp", &reference_dir.to_string_lossy(), class])?;
    let krusty = common::javap(&["-p", "-cp", &krusty_dir.to_string_lossy(), class])
        .expect("javap reads krusty output");
    let _ = std::fs::remove_dir_all(root);
    Some((reference, krusty))
}

#[test]
fn a_source_secondary_constructor_is_interleaved_with_declared_members() {
    let Some((reference, krusty)) = build_both(SOURCE, "SecondaryConstructorMemberOrder", "Mixed")
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_eq!(
        member_declarations(&krusty),
        member_declarations(&reference),
        "complete method declaration order"
    );
}

#[test]
fn a_value_class_secondary_constructor_keeps_its_source_order_after_realization() {
    let Some((reference, krusty)) = build_both(
        VALUE_CLASS_SOURCE,
        "ValueClassSecondaryConstructorMemberOrder",
        "InlineOrder",
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let expected = vec![
        "getSeed();".to_string(),
        "getBefore-impl(int);".to_string(),
        "constructor-impl(java.lang.String);".to_string(),
        "after-impl(int);".to_string(),
    ];
    assert_eq!(
        value_class_source_member_identities(&reference),
        expected,
        "reference source-owned realization order"
    );
    assert_eq!(
        value_class_source_member_identities(&krusty),
        expected,
        "krusty source-owned realization order"
    );
}

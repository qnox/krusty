//! A `lateinit` backing field holds null until it is assigned, so kotlinc's `AnnotationCodegen`
//! gives it no nullability annotation, whatever its declared type. krusty marked it `@NotNull`.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and each
//! class's fields with their invisible annotations are compared exactly.
use std::fs;

use super::common;

const SOURCE: &str = r#"
lateinit var top: String
private lateinit var hidden: String

class Holder {
    lateinit var name: String
    private lateinit var secret: List<String>
    var plain: String = "p"
    fun fill() { name = "n"; secret = listOf("s") }
    fun read() = name + secret[0] + plain
}

object Single { lateinit var value: String }

enum class Kind { A; lateinit var label: String }

fun box(): String {
    top = "t"; hidden = "h"; Single.value = "v"; Kind.A.label = "k"
    val h = Holder(); h.fill()
    val r = top + hidden + h.read() + Single.value + Kind.A.label
    return if (r == "thnspvk") "OK" else r
}
"#;

/// Each field of `class` as its name followed by the annotations `javap -v` lists on it.
fn field_annotations(dir: &std::path::Path, class: &str) -> Vec<String> {
    let path = dir.join(format!("{class}.class"));
    let listing = common::javap(&["-v", "-p", &path.to_string_lossy()]).expect("pooled javap");
    let mut fields = Vec::new();
    let mut members = listing.lines().skip_while(|line| *line != "{").skip(1);
    while let Some(header) = members.next() {
        // Fields come first; the first method header ends the field table.
        if header.contains('(') {
            break;
        }
        let Some(name) = header
            .trim()
            .strip_suffix(';')
            .and_then(|declaration| declaration.split(' ').next_back())
        else {
            continue;
        };
        let mut field = name.to_string();
        for line in members.by_ref().take_while(|line| !line.is_empty()) {
            if line.trim().starts_with("org.jetbrains.annotations.") {
                field.push(' ');
                field.push_str(line.trim());
            }
        }
        fields.push(field);
    }
    fields
}

#[test]
fn a_lateinit_field_carries_no_nullability_annotation() {
    let base = std::env::temp_dir().join(format!("krusty_lateinit_field_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let krusty_dir = base.join("krusty");
    let kotlinc_dir = base.join("kotlinc");
    fs::create_dir_all(&krusty_dir).unwrap();
    fs::create_dir_all(&kotlinc_dir).unwrap();
    let source = base.join("Late.kt");
    fs::write(&source, SOURCE).unwrap();
    let Some((code, stderr)) = common::kotlinc_compile(&[
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ]) else {
        return; // toolchain not provisioned
    };
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let classes = common::compile_in_process(
        SOURCE,
        "Late",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty failed to compile the fixture");
    for (internal, bytes) in &classes {
        fs::write(krusty_dir.join(format!("{internal}.class")), bytes).unwrap();
    }
    for class in ["LateKt", "Holder", "Single", "Kind"] {
        let expected = field_annotations(&kotlinc_dir, class);
        assert!(!expected.is_empty(), "{class}: kotlinc's field table");
        assert_eq!(
            field_annotations(&krusty_dir, class),
            expected,
            "{class}: field annotations must match kotlinc's"
        );
    }
}

#[test]
fn unannotated_lateinit_fields_still_initialize() {
    assert_eq!(
        common::compile_and_run_box(
            SOURCE,
            "Late",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path())
        )
        .as_deref(),
        Some("OK")
    );
}

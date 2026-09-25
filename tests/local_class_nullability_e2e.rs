//! The nullability annotations of the declarations of a local class, as kotlinc writes them.
//!
//! kotlinc annotates a reference-typed member, field and parameter with `@NotNull`/`@Nullable` so
//! that a caller outside Kotlin can read the contract. A class declared in executable code (a local
//! class or an anonymous object) and every class nested in one gets no
//! such annotation: nothing outside the enclosing body can reach it. Its parameters are still
//! guarded. A named suspend function's continuation is not local and keeps them. Outside a local
//! class, the compiler's own storage is not annotated either: an inner class's outer instance has
//! no annotation on its field, and takes no slot in the constructor's parameter annotations.

use super::common;

const SOURCE: &str = "class Top(val name: String) {\n\
\x20   fun greet(other: String?): String = name + other\n\
\x20   inner class Member(val piece: String?) { fun join(tail: String): String = name + piece + tail }\n\
}\n\
fun outer(label: String): String {\n\
\x20   class Local(val first: String, val second: String?) {\n\
\x20       inner class Part(val piece: String) {\n\
\x20           fun join(tail: String): String = piece + first + tail\n\
\x20       }\n\
\x20       fun tag(prefix: String): String = prefix + label + second\n\
\x20   }\n\
\x20   val probe = object {\n\
\x20       val size: String = label\n\
\x20       fun echo(text: String?): String = text + size\n\
\x20   }\n\
\x20   val nested = run {\n\
\x20       data class Pair2(val left: String, val right: String?)\n\
\x20       Pair2(\"l\", null).copy(right = \"r\").right\n\
\x20   }\n\
\x20   val local = Local(\"a\", null)\n\
\x20   return local.tag(\"p\") + local.Part(\"x\").join(\"y\") +\n\
\x20       probe.echo(null) + nested\n\
}\n\
suspend fun echo(text: String): String = text\n\
suspend fun fetch(text: String): String {\n\
\x20   val first = echo(text)\n\
\x20   return first + echo(text)\n\
}\n\
fun box(): String {\n\
\x20   val result = outer(\"q\") + Top(\"t\").greet(null) + Top(\"u\").Member(null).join(\"v\")\n\
\x20   return if (result == \"pqnullxaynullqrtnullunullv\") \"OK\" else \"fail: $result\"\n\
}\n";

/// Classes kotlinc writes with no nullability annotation.
const LOCAL: &[&str] = &[
    "LocalNullabilityKt$outer$Local",
    "LocalNullabilityKt$outer$Local$Part",
    "LocalNullabilityKt$outer$probe$1",
    "LocalNullabilityKt$outer$nested$1$Pair2",
];

/// Classes kotlinc annotates, for contrast.
const ANNOTATED: &[&str] = &[
    "Top",
    "Top$Member",
    "LocalNullabilityKt$fetch$1",
    "LocalNullabilityKt",
];

struct Compiled {
    dir: std::path::PathBuf,
    reference: std::path::PathBuf,
    ours: std::path::PathBuf,
}

impl Drop for Compiled {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn compile_both() -> Option<Compiled> {
    let dir = common::scratch_dir()?;
    let reference = dir.join("ref");
    let ours = dir.join("out");
    std::fs::create_dir_all(&reference).ok()?;
    std::fs::create_dir_all(&ours).ok()?;
    let source_path = dir.join("LocalNullability.kt");
    std::fs::write(&source_path, SOURCE).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let emitted =
        common::compile_in_process_metadata_cp(SOURCE, "LocalNullability", &[common::stdlib_jar()])
            .expect("krusty compiles the local classes");
    for (name, bytes) in &emitted {
        std::fs::write(ours.join(format!("{name}.class")), bytes).ok()?;
    }
    Some(Compiled {
        dir,
        reference,
        ours,
    })
}

/// Each nullability annotation of `class` under `root`, with the name and descriptor of the member
/// it annotates and, for a parameter, its slot in the parameter annotation table.
fn nullability(root: &std::path::Path, class: &str) -> Vec<String> {
    let dump = common::javap(&["-v", "-p", "-cp", &root.to_string_lossy(), class])
        .unwrap_or_else(|| panic!("javap {class}"));
    let mut name = String::new();
    let mut member = String::new();
    let mut parameter = String::new();
    let mut annotations = Vec::new();
    for line in dump.lines() {
        if line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(';') {
            let declaration = line.split('(').next().unwrap_or(line);
            name = declaration
                .trim_end_matches(';')
                .split(' ')
                .last()
                .unwrap_or("")
                .to_string();
        } else if let Some(descriptor) = line.trim().strip_prefix("descriptor: ") {
            member = format!("{name}{descriptor}");
            parameter.clear();
        } else if line.trim().starts_with("parameter ") {
            parameter = line.trim().to_string();
        } else if line.contains("org.jetbrains.annotations.") && !line.contains("//") {
            annotations.push(format!("{member} {parameter} {}", line.trim()));
        }
    }
    annotations
}

#[test]
fn local_class_declarations_carry_no_nullability_annotations() {
    let Some(compiled) = compile_both() else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    for class in LOCAL {
        assert_eq!(
            nullability(&compiled.reference, class),
            Vec::<String>::new(),
            "{class}: kotlinc annotated it"
        );
        assert_eq!(
            nullability(&compiled.ours, class),
            Vec::<String>::new(),
            "{class}: nullability differs from kotlinc"
        );
    }
    for class in ANNOTATED {
        let reference = nullability(&compiled.reference, class);
        assert!(!reference.is_empty(), "{class}: kotlinc annotated nothing");
        assert_eq!(
            nullability(&compiled.ours, class),
            reference,
            "{class}: nullability differs from kotlinc"
        );
    }
}

#[test]
fn local_classes_run() {
    common::expect_box_same_as_kotlinc(SOURCE, "LocalNullabilityRun");
}

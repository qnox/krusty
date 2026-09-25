//! Shared differential class-building and disassembly helpers.

use std::path::PathBuf;

pub struct ReferenceComparison {
    pub reference: String,
    pub krusty: String,
    pub reference_bytes: Vec<u8>,
    pub krusty_bytes: Vec<u8>,
}

/// Build one class with kotlinc and krusty under the same classpath, target and kotlinc options.
pub fn compare_with_kotlinc_plugin(
    name: &str,
    src: &str,
    class: &str,
    cp_jars: &[PathBuf],
    jvm_target: &str,
    kotlinc_extra: &[String],
) -> Option<ReferenceComparison> {
    let dir = super::common_core::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&krusty_dir).ok()?;
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).ok()?;

    let mut arguments = vec![
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        jvm_target.to_string(),
    ];
    if !cp_jars.is_empty() {
        arguments.push("-classpath".to_string());
        arguments.push(
            cp_jars
                .iter()
                .map(|jar| jar.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(":"),
        );
    }
    arguments.extend(kotlinc_extra.iter().cloned());
    arguments.push(source.to_string_lossy().into_owned());
    let (code, stderr) = super::common_core::kotlinc_compile(&arguments)?;
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");

    let class_major = jvm_target
        .parse::<u16>()
        .ok()
        .filter(|target| (9..=99).contains(target))
        .map(|target| target + 44)
        .unwrap_or_else(|| panic!("unknown -jvm-target {jvm_target}"));
    let classes = super::common_core::compile_in_process_metadata_cp_module_target(
        src,
        name,
        cp_jars,
        "main",
        Some(class_major),
    )
    .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(path, bytes).ok()?;
    }

    let reference = super::common_core::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &reference_dir.to_string_lossy(),
        class,
    ])?;
    let krusty = super::common_core::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &krusty_dir.to_string_lossy(),
        class,
    ])?;
    let reference_bytes = std::fs::read(reference_dir.join(format!("{class}.class"))).ok()?;
    let krusty_bytes = std::fs::read(krusty_dir.join(format!("{class}.class"))).ok()?;
    let _ = std::fs::remove_dir_all(dir);
    Some(ReferenceComparison {
        reference,
        krusty,
        reference_bytes,
        krusty_bytes,
    })
}

/// Instruction rows for one method, with only constant-pool indices erased. Javap comments retain
/// the exact selected owner/member/descriptor identity.
pub fn method_instructions(disassembly: &str, marker: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in disassembly.lines() {
        let line = raw.trim();
        if line.ends_with(';') && line.contains(marker) {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if [
            "LineNumberTable",
            "LocalVariableTable",
            "StackMapTable",
            "Exception table",
        ]
        .iter()
        .any(|table| line.starts_with(table))
            || (line.starts_with("descriptor:") && !out.is_empty())
        {
            break;
        }
        let Some((pc, rest)) = line.split_once(": ") else {
            continue;
        };
        if pc.parse::<u32>().is_err() {
            continue;
        }
        let code = rest
            .split_whitespace()
            .map(|token| if token.starts_with('#') { "#" } else { token })
            .collect::<Vec<_>>()
            .join(" ");
        out.push(format!("{}: {code}", pc.trim()));
    }
    out
}

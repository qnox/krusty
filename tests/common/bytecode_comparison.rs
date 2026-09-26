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

/// Every class kotlinc and krusty write for `src`, compiled over one library that kotlinc compiled
/// from `lib`, by internal name.
pub struct ClassSets {
    pub reference: std::collections::BTreeMap<String, Vec<u8>>,
    pub krusty: std::collections::BTreeMap<String, Vec<u8>>,
}

impl ClassSets {
    /// The method declarations of `class` that kotlinc and krusty each write, in class-file order,
    /// as `javap -p` prints them; `None` when either compiler wrote no such class.
    pub fn method_declarations(&self, class: &str) -> Option<(Vec<String>, Vec<String>)> {
        let declarations =
            |bytes: &Vec<u8>| declared_methods(&disassemble_with(class, bytes, &["-p"]));
        Some((
            declarations(self.reference.get(class)?),
            declarations(self.krusty.get(class)?),
        ))
    }

    /// Each class that differs between the two, or that only one of them wrote, with both
    /// disassemblies of a class they both wrote.
    pub fn differences(&self) -> Vec<String> {
        let names: std::collections::BTreeSet<&String> =
            self.reference.keys().chain(self.krusty.keys()).collect();
        names
            .into_iter()
            .filter_map(
                |name| match (self.reference.get(name), self.krusty.get(name)) {
                    (Some(reference), Some(krusty)) if reference == krusty => None,
                    (Some(reference), Some(krusty)) => Some(format!(
                        "{name} differs\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
                        disassemble(name, reference),
                        disassemble(name, krusty)
                    )),
                    (Some(_), None) => Some(format!("{name}: only kotlinc writes it")),
                    (None, _) => Some(format!("{name}: only krusty writes it")),
                },
            )
            .collect()
    }

    /// krusty's `javap -c -p -l` of `class`, with pool indices erased (see [`Self::code_differences`]).
    pub fn krusty_code(&self, class: &str) -> String {
        let bytes = self
            .krusty
            .get(class)
            .unwrap_or_else(|| panic!("krusty wrote no {class}"));
        code_listing(class, bytes)
    }

    /// Each class whose methods differ in their code, line numbers or local variables, with
    /// constant-pool indices erased: the two classes may lay out their pools differently.
    pub fn code_differences(&self) -> Vec<String> {
        let names: std::collections::BTreeSet<&String> =
            self.reference.keys().chain(self.krusty.keys()).collect();
        names
            .into_iter()
            .filter_map(
                |name| match (self.reference.get(name), self.krusty.get(name)) {
                    (Some(reference), Some(krusty)) => {
                        let reference = code_listing(name, reference);
                        let krusty = code_listing(name, krusty);
                        (reference != krusty).then(|| {
                            format!("{name} differs\n--- kotlinc ---\n{reference}\n--- krusty ---\n{krusty}")
                        })
                    }
                    (Some(_), None) => Some(format!("{name}: only kotlinc writes it")),
                    (None, _) => Some(format!("{name}: only krusty writes it")),
                },
            )
            .collect()
    }
}

/// Compile `src` (file `<name>.kt`) with kotlinc and krusty over the library kotlinc compiles from
/// `lib`. `None` when the reference toolchain is not provisioned.
pub fn classes_against_kotlinc_lib(
    name: &str,
    lib: &[(&str, &str)],
    src: &str,
) -> Option<ClassSets> {
    let library = super::common_core::kotlinc_lib_out(lib)?;
    let dir = super::common_core::scratch_dir()?;
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).ok()?;
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).ok()?;
    let arguments = vec![
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-cp".to_string(),
        library.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = super::common_core::kotlinc_compile(&arguments)?;
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let mut reference = std::collections::BTreeMap::new();
    collect_classes(&reference_dir, &reference_dir, &mut reference);
    let classpath = [library, super::common_core::stdlib_jar()];
    let krusty = super::common_core::compile_in_process_metadata_cp(src, name, &classpath)
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"))
        .into_iter()
        .collect();
    let _ = std::fs::remove_dir_all(dir);
    Some(ClassSets { reference, krusty })
}

fn collect_classes(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut std::collections::BTreeMap<String, Vec<u8>>,
) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_classes(root, &path, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "class")
        {
            let relative = path
                .strip_prefix(root)
                .expect("a class under the output root");
            let name = relative.with_extension("").to_string_lossy().into_owned();
            out.insert(
                name,
                std::fs::read(&path).expect("a written class reads back"),
            );
        }
    }
}

/// `javap -p` lines that declare a method or constructor.
fn declared_methods(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|line| line.contains('(') && line.ends_with(';'))
        .map(str::to_string)
        .collect()
}

/// `javap -c -p -l` of a class with every `#N` pool index erased.
fn code_listing(name: &str, bytes: &[u8]) -> String {
    disassemble_with(name, bytes, &["-p", "-c", "-l"])
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|token| match token.strip_prefix('#') {
                    Some(rest) if rest.trim_end_matches(',').parse::<u32>().is_ok() => "#",
                    _ => token,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn disassemble(name: &str, bytes: &[u8]) -> String {
    disassemble_with(name, bytes, &["-p", "-c", "-v"])
}

fn disassemble_with(name: &str, bytes: &[u8], flags: &[&str]) -> String {
    let Some(dir) = super::common_core::scratch_dir() else {
        return "(no scratch directory)".to_string();
    };
    let path = dir.join(format!("{name}.class"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, bytes);
    let dir_arg = dir.to_string_lossy().into_owned();
    let mut args = flags.to_vec();
    args.extend(["-cp", &dir_arg, name]);
    let text = super::common_core::javap(&args).unwrap_or_default();
    let _ = std::fs::remove_dir_all(dir);
    text
}

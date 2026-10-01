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
    let inputs =
        super::common_core::byte_dump::class_dump_inputs(src, jvm_target, kotlinc_extra, cp_jars);
    let reference_bytes = super::common_core::byte_dump::kotlinc_class_dumps(
        name,
        jvm_target,
        &inputs.variant,
        inputs.fingerprint,
        &[class],
        || {
            let dir = super::common_core::scratch_dir()?;
            let reference_dir = dir.join("ref");
            std::fs::create_dir_all(&reference_dir).ok()?;
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
            let bytes = std::fs::read(reference_dir.join(format!("{class}.class"))).ok()?;
            let _ = std::fs::remove_dir_all(&dir);
            let mut produced = std::collections::BTreeMap::new();
            produced.insert(class.to_string(), bytes);
            Some(produced)
        },
    )?
    .pop()?;

    let dir = super::common_core::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&krusty_dir).ok()?;
    let reference_path = reference_dir.join(format!("{class}.class"));
    if let Some(parent) = reference_path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(&reference_path, &reference_bytes).ok()?;

    let class_major = match jvm_target {
        "1.6" | "6" => 50,
        "1.7" | "7" => 51,
        "1.8" | "8" => 52,
        other => other
            .parse::<u16>()
            .ok()
            .filter(|target| (9..=99).contains(target))
            .map(|target| target + 44)
            .unwrap_or_else(|| panic!("unknown -jvm-target {other}")),
    };
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

/// One method's verbose disassembly — flags, instructions and its line and local-variable tables —
/// from its `javap` header line (`public static final int access$f(int);`) to the blank line
/// ending it, with only constant-pool indices erased. Empty when the class has no such method.
pub fn method_block(disassembly: &str, header: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != header)
        .take_while(|line| !line.is_empty())
        .map(|line| {
            line.split_whitespace()
                .map(|token| if token.starts_with('#') { "#" } else { token })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// A class's verbose member disassembly — every field and method with its flags, instructions and
/// debug tables — with only constant-pool indices erased.
pub fn member_blocks(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .skip_while(|line| *line != "{")
        .take_while(|line| *line != "}")
        .map(|line| {
            line.split_whitespace()
                .map(|token| if token.starts_with('#') { "#" } else { token })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// A class file's fields and methods in class-file order, each with its access flags, descriptor
/// and generic signature, and a field with its `ConstantValue`.
pub fn member_table(bytes: &[u8]) -> Vec<String> {
    let info = krusty::jvm::classreader::parse_class(bytes).expect("a readable class file");
    let fields = info.fields.iter().map(|field| {
        format!(
            "field {:#06x} {} {} {:?} {:?}",
            field.access, field.name, field.descriptor, field.signature, field.const_value
        )
    });
    let methods = info.methods.iter().map(|method| {
        format!(
            "method {:#06x} {}{} {:?}",
            method.access, method.name, method.descriptor, method.signature
        )
    });
    fields.chain(methods).collect()
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

    /// The `javap -c -p -l` block of the method `class` declares as `declaration` (javap's spelling,
    /// such as `public static final int f();`), from kotlinc and from krusty, with constant-pool
    /// indices erased as in [`Self::code_differences`].
    pub fn method_listing(&self, class: &str, declaration: &str) -> (String, String) {
        let block = |bytes: Option<&Vec<u8>>| {
            let bytes = bytes.unwrap_or_else(|| panic!("{class} was not written"));
            let listing = code_listing(class, bytes);
            let mut lines = listing.lines().skip_while(|line| *line != declaration);
            assert!(
                lines.next().is_some(),
                "{class} declares no `{declaration}`"
            );
            lines
                .take_while(|line| !line.is_empty() && *line != "}")
                .collect::<Vec<_>>()
                .join("\n")
        };
        (
            block(self.reference.get(class)),
            block(self.krusty.get(class)),
        )
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
    classes_against_kotlinc_lib_target(name, lib, src, None)
}

/// [`classes_against_kotlinc_lib`] with `src` compiled for kotlinc's `-jvm-target` `jvm_target`
/// (the library keeps the default target).
pub fn classes_against_kotlinc_lib_target(
    name: &str,
    lib: &[(&str, &str)],
    src: &str,
    jvm_target: Option<u16>,
) -> Option<ClassSets> {
    let library = super::common_core::kotlinc_lib_out(lib)?;
    let target = match jvm_target {
        Some(target) => target.to_string(),
        None => "default".to_string(),
    };
    let inputs = super::common_core::byte_dump::class_dump_inputs(
        src,
        &target,
        &[],
        std::slice::from_ref(&library),
    );
    let reference = super::common_core::byte_dump::kotlinc_class_tree(
        name,
        &target,
        &inputs.variant,
        inputs.fingerprint,
        || {
            let dir = super::common_core::scratch_dir()?;
            let reference_dir = dir.join("ref");
            std::fs::create_dir_all(&reference_dir).ok()?;
            let source = dir.join(format!("{name}.kt"));
            std::fs::write(&source, src).ok()?;
            let mut arguments = vec![
                "-d".to_string(),
                reference_dir.to_string_lossy().into_owned(),
                "-cp".to_string(),
                library.to_string_lossy().into_owned(),
            ];
            if let Some(target) = jvm_target {
                arguments.push("-jvm-target".to_string());
                arguments.push(target.to_string());
            }
            arguments.push(source.to_string_lossy().into_owned());
            let (code, stderr) = super::common_core::kotlinc_compile(&arguments)?;
            assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
            let mut reference = std::collections::BTreeMap::new();
            collect_classes(&reference_dir, &reference_dir, &mut reference);
            let _ = std::fs::remove_dir_all(&dir);
            Some(reference)
        },
    )?;
    let classpath = [library, super::common_core::stdlib_jar()];
    let krusty = super::common_core::compile_in_process_metadata_cp_module_target(
        src,
        name,
        &classpath,
        "main",
        jvm_target.map(|target| target + 44),
    )
    .unwrap_or_else(|| panic!("{name}: krusty failed to compile"))
    .into_iter()
    .collect();
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

/// Compile `source` with both compilers and require `class` to be kotlinc's: its header, every
/// member with its code and debug tables, and its `@Metadata`.
pub fn assert_class_matches_kotlinc(stem: &str, source: &str, class: &str) -> ReferenceComparison {
    let comparison = assert_class_code_matches_kotlinc(stem, source, class);
    assert_eq!(
        super::common_core::raw_kotlin_metadata(&comparison.krusty_bytes),
        super::common_core::raw_kotlin_metadata(&comparison.reference_bytes),
        "{class}: kotlinc's @Metadata"
    );
    comparison
}

/// Compile `source` with both compilers and require `class`'s header and every member, with its
/// code and debug tables, to be kotlinc's. The class's `@Metadata` is not compared.
pub fn assert_class_code_matches_kotlinc(
    stem: &str,
    source: &str,
    class: &str,
) -> ReferenceComparison {
    let comparison = compare_with_kotlinc_plugin(
        stem,
        source,
        class,
        &[super::common_core::stdlib_jar()],
        "17",
        &super::common_core::language_directives::kotlinc_args(source),
    )
    .expect("reference kotlinc and javap are provisioned");
    let header = |bytes: &[u8]| {
        let info = krusty::jvm::classreader::parse_class(bytes).expect("a readable class file");
        (
            info.access,
            info.this_class,
            info.super_class,
            info.interfaces(),
            info.signature.clone(),
        )
    };
    assert_eq!(
        header(&comparison.krusty_bytes),
        header(&comparison.reference_bytes),
        "{class}: kotlinc's class header"
    );
    assert_eq!(
        member_table(&comparison.krusty_bytes),
        member_table(&comparison.reference_bytes),
        "{class}: kotlinc's member table"
    );
    assert_eq!(
        member_blocks(&comparison.krusty),
        member_blocks(&comparison.reference),
        "{class}: kotlinc's members"
    );
    comparison
}

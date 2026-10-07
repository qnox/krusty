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
    compare_with_kotlinc_plugin_full(
        name,
        src,
        class,
        cp_jars,
        cp_jars,
        jvm_target,
        kotlinc_extra,
    )
}

/// [`compare_with_kotlinc_plugin`] with the JDK modules beside the stdlib on the KRUSTY side
/// only. A fixture subclassing a JDK-hierarchy stdlib class (`AbstractMutableSet` →
/// `java.util.AbstractSet`, `Number`) does not resolve for krusty without them; the reference
/// kotlinc always has its own JDK, so the recorded dump keeps the stdlib-only key and the
/// `-classpath` the jimage cannot join stays off it.
pub fn compare_with_kotlinc_plugin_jdk(
    name: &str,
    src: &str,
    class: &str,
    jvm_target: &str,
    kotlinc_extra: &[String],
) -> Option<ReferenceComparison> {
    compare_with_kotlinc_plugin_jdk_cp(name, src, class, &[], jvm_target, kotlinc_extra)
}

/// [`compare_with_kotlinc_plugin_jdk`] with `extra_cp` (a javac-built fixture directory, a project
/// library) on BOTH compilers' classpaths; the JDK modules still join only the krusty side.
pub fn compare_with_kotlinc_plugin_jdk_cp(
    name: &str,
    src: &str,
    class: &str,
    extra_cp: &[PathBuf],
    jvm_target: &str,
    kotlinc_extra: &[String],
) -> Option<ReferenceComparison> {
    let stdlib = super::common_core::stdlib_jar();
    let mut reference_cp = vec![stdlib];
    reference_cp.extend(extra_cp.iter().cloned());
    let mut krusty_cp = reference_cp.clone();
    krusty_cp.push(super::common_core::jdk_modules());
    compare_with_kotlinc_plugin_full(
        name,
        src,
        class,
        &reference_cp,
        &krusty_cp,
        jvm_target,
        kotlinc_extra,
    )
}

fn compare_with_kotlinc_plugin_full(
    name: &str,
    src: &str,
    class: &str,
    cp_jars: &[PathBuf],
    krusty_cp_jars: &[PathBuf],
    jvm_target: &str,
    kotlinc_extra: &[String],
) -> Option<ReferenceComparison> {
    compare_with_kotlinc_plugin_request(ComparisonRequest {
        name,
        src,
        class,
        cp_jars,
        krusty_cp_jars,
        jvm_target,
        kotlinc_extra,
        language_settings: None,
    })
}

/// [`compare_with_kotlinc_plugin`] under one explicit public language/API configuration supplied
/// to both compilers. The JDK modules sit beside the stdlib on the krusty side only (see
/// [`compare_with_kotlinc_plugin_jdk`]); the reference kotlinc always has its own JDK, so the
/// recorded dump keeps the requested classpath.
pub fn compare_with_kotlinc_plugin_language_settings(
    name: &str,
    src: &str,
    class: &str,
    cp_jars: &[PathBuf],
    jvm_target: &str,
    language_settings: &krusty::language_settings::LanguageSettings,
) -> Option<ReferenceComparison> {
    let kotlinc_extra = kotlinc_language_options(language_settings);
    let mut krusty_cp = cp_jars.to_vec();
    krusty_cp.push(super::common_core::jdk_modules());
    compare_with_kotlinc_plugin_request(ComparisonRequest {
        name,
        src,
        class,
        cp_jars,
        krusty_cp_jars: &krusty_cp,
        jvm_target,
        kotlinc_extra: &kotlinc_extra,
        language_settings: Some(language_settings),
    })
}

fn kotlinc_language_options(
    language_settings: &krusty::language_settings::LanguageSettings,
) -> Vec<String> {
    let mut options = vec![
        "-language-version".to_owned(),
        language_settings.language_version.to_string(),
        "-api-version".to_owned(),
        language_settings.api_version.to_string(),
    ];
    let baseline = krusty::features::LangFeatures::for_versions(
        language_settings.language_version,
        language_settings.api_version,
    );
    let feature_names = baseline
        .iter()
        .chain(language_settings.features.iter())
        .collect::<std::collections::BTreeSet<_>>();
    for feature in feature_names {
        let enabled = language_settings.features.has(feature);
        if enabled != baseline.has(feature) {
            options.push(format!(
                "-XXLanguage:{}{feature}",
                if enabled { '+' } else { '-' }
            ));
        }
    }
    options
}

/// One class compared against the reference compiler: what to build, the classpaths each side
/// sees, and the public language/API settings both compiler invocations take.
struct ComparisonRequest<'a> {
    name: &'a str,
    src: &'a str,
    class: &'a str,
    cp_jars: &'a [PathBuf],
    krusty_cp_jars: &'a [PathBuf],
    jvm_target: &'a str,
    kotlinc_extra: &'a [String],
    language_settings: Option<&'a krusty::language_settings::LanguageSettings>,
}

fn compare_with_kotlinc_plugin_request(
    request: ComparisonRequest<'_>,
) -> Option<ReferenceComparison> {
    let ComparisonRequest {
        name,
        src,
        class,
        cp_jars,
        krusty_cp_jars,
        jvm_target,
        kotlinc_extra,
        language_settings,
    } = request;
    let inputs =
        super::common_core::byte_dump::class_dump_inputs(src, jvm_target, kotlinc_extra, cp_jars);
    let reference_bytes = super::common_core::byte_dump::kotlinc_class_dumps(
        name,
        jvm_target,
        &inputs.variant,
        inputs.fingerprint,
        inputs.legacy_fingerprint,
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
    let classes = match language_settings {
        Some(language_settings) => super::common_core::source_set_compile::compile(
            &[(name, src)],
            krusty_cp_jars,
            None,
            Some(class_major),
            Some(language_settings.language_version.metadata_version()),
            language_settings,
        ),
        None => super::common_core::compile_in_process_metadata_cp_module_target(
            src,
            name,
            krusty_cp_jars,
            "main",
            Some(class_major),
        ),
    }
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

/// A `javap -v -p` listing with its constant-pool indices erased to `#`, and nothing else changed,
/// read line by line. javap prints a pool index in two positions only: as a whole token (an
/// operand `#12,`, a `BootstrapMethods` argument `#30`, an `InnerClasses` row `#10= #2 of #4;`),
/// and inside an encoded annotation row (`0: #72()`, `1: #154(#155=[I#156])`,
/// `default_value: I#12`), whose grammar holds only pool indices. The decoded annotation javap
/// prints beneath an encoded row, indented deeper (`Ticket(value="ticket #123")`), is compared
/// exactly, as is every other token.
#[derive(Default)]
pub struct ListingPoolIndices {
    /// The indentation of the encoded annotation row whose decoded block is being read.
    decoded_below: Option<usize>,
}

impl ListingPoolIndices {
    pub fn erase(&mut self, line: &str) -> String {
        let indentation = line.len() - line.trim_start().len();
        let tokens = line.split_whitespace().collect::<Vec<_>>();
        if self
            .decoded_below
            .is_some_and(|encoded| indentation > encoded && !tokens.is_empty())
        {
            return tokens.join(" ");
        }
        self.decoded_below = None;
        let kind = match tokens.as_slice() {
            ["default_value:", ..] => Some(EncodedRowKind::ElementValue),
            [position, ..] if position.strip_suffix(':').is_some_and(is_decimal) => {
                Some(EncodedRowKind::Annotation)
            }
            _ => None,
        };
        let mut erased = tokens
            .iter()
            .map(|token| erase_pool_reference(token).unwrap_or_else(|| (*token).to_owned()))
            .collect::<Vec<_>>();
        if let (Some(kind), Some(token)) = (kind, tokens.get(1)) {
            if let Some(encoded) = EncodedAnnotation::erase(token, kind) {
                erased[1] = encoded;
                self.decoded_below = Some(indentation);
            }
        }
        erased.join(" ")
    }
}

fn is_decimal(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// A token that is exactly one pool index, with the punctuation javap writes after it.
fn erase_pool_reference(token: &str) -> Option<String> {
    let index = token.strip_prefix('#')?;
    let digits = index.trim_end_matches([',', ':', ';', '=']);
    (is_decimal(digits) && index.len() - digits.len() <= 1)
        .then(|| format!("#{}", &index[digits.len()..]))
}

/// What an encoded annotation row holds: an annotation (`0: #72()`), or an `AnnotationDefault`
/// element value (`default_value: I#12`).
#[derive(Clone, Copy)]
enum EncodedRowKind {
    Annotation,
    ElementValue,
}

/// A recursive-descent reader of javap's encoded annotation grammar, writing it back with every
/// pool index erased. A token that is not exactly that grammar is left alone.
struct EncodedAnnotation<'a> {
    rest: &'a str,
    erased: String,
}

impl<'a> EncodedAnnotation<'a> {
    fn erase(token: &'a str, kind: EncodedRowKind) -> Option<String> {
        let mut reader = Self {
            rest: token,
            erased: String::with_capacity(token.len()),
        };
        match kind {
            EncodedRowKind::Annotation => reader.annotation()?,
            EncodedRowKind::ElementValue => reader.element_value()?,
        }
        // A type annotation's row continues after a colon (`0: #24(): FIELD`).
        if matches!(kind, EncodedRowKind::Annotation) {
            reader.optional(':');
        }
        reader.rest.is_empty().then_some(reader.erased)
    }

    fn optional(&mut self, expected: char) -> bool {
        match self.rest.strip_prefix(expected) {
            Some(rest) => {
                self.rest = rest;
                self.erased.push(expected);
                true
            }
            None => false,
        }
    }

    fn expect(&mut self, expected: char) -> Option<()> {
        self.optional(expected).then_some(())
    }

    fn pool_index(&mut self) -> Option<()> {
        let index = self.rest.strip_prefix('#')?;
        let digits = index.len() - index.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return None;
        }
        self.rest = &index[digits..];
        self.erased.push('#');
        Some(())
    }

    /// `#type(#name=value,...)`
    fn annotation(&mut self) -> Option<()> {
        self.pool_index()?;
        self.expect('(')?;
        if !self.optional(')') {
            loop {
                self.pool_index()?;
                self.expect('=')?;
                self.element_value()?;
                if self.optional(')') {
                    break;
                }
                self.expect(',')?;
            }
        }
        Some(())
    }

    /// A constant (`I#12`, `s#158`), an enum (`e#12.#13`), a class (`c#14`), a nested annotation
    /// (`@#72()`) or an array (`[I#156,I#157]`).
    fn element_value(&mut self) -> Option<()> {
        let tag = self.rest.chars().next()?;
        match tag {
            'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z' | 's' | 'c' => {
                self.expect(tag)?;
                self.pool_index()
            }
            'e' => {
                self.expect('e')?;
                self.pool_index()?;
                self.expect('.')?;
                self.pool_index()
            }
            '@' => {
                self.expect('@')?;
                self.annotation()
            }
            '[' => {
                self.expect('[')?;
                if !self.optional(']') {
                    loop {
                        self.element_value()?;
                        if self.optional(']') {
                            break;
                        }
                        self.expect(',')?;
                    }
                }
                Some(())
            }
            _ => None,
        }
    }
}

/// Complete output inventory from both compilers for one checked multi-file module.
pub fn classes_against_kotlinc_module(sources: &[(&str, &str)]) -> ClassSets {
    classes_against_kotlinc_source_set(sources, 0)
}

/// [`classes_against_kotlinc_module`] for a multiplatform JVM module whose first `common` sources
/// are common sources: kotlinc gets `-Xmulti-platform -Xcommon-sources=…`, krusty marks the same
/// sources common.
pub fn classes_against_kotlinc_source_set(sources: &[(&str, &str)], common: usize) -> ClassSets {
    let common_root = super::common_core::scratch_dir().expect("allocate module class inventory");
    let output = common_root.join("reference");
    let mut arguments = vec!["-d".to_owned(), output.to_string_lossy().into_owned()];
    let mut common_paths = Vec::new();
    let mut paths = Vec::new();
    for (index, (name, source)) in sources.iter().enumerate() {
        let path = common_root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create module source directory");
        }
        std::fs::write(&path, source).expect("write module fixture source");
        if index < common {
            common_paths.push(path.to_string_lossy().into_owned());
        }
        paths.push(path.to_string_lossy().into_owned());
    }
    if common > 0 {
        arguments.push("-Xmulti-platform".to_owned());
        arguments.push(format!("-Xcommon-sources={}", common_paths.join(",")));
    }
    arguments.extend(paths);
    let (code, diagnostics) = super::common_core::byte_dump::with_recorded_diagnostics(|| {
        super::common_core::kotlinc_compile(&arguments).expect("reference compiler is provisioned")
    });
    assert_eq!(
        code, 0,
        "kotlinc rejected module inventory fixture: {diagnostics}"
    );
    let mut reference = std::collections::BTreeMap::new();
    collect_classes(&output, &output, &mut reference);
    let _ = std::fs::remove_dir_all(common_root);
    let stdlib = super::common_core::stdlib_jar();
    let jdk = super::common_core::jdk_modules();
    let krusty = super::e2e_support::compile_in_process_files_common(
        sources,
        common,
        &[stdlib],
        Some(jdk.as_path()),
    )
    .expect("krusty accepted module inventory fixture")
    .into_iter()
    // The in-process emitter also returns `META-INF/<module>.kotlin_module`. The reference
    // side is class files only, so the inventory comparison is class identity.
    .filter(|(_, bytes)| bytes.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE]))
    .collect();
    ClassSets { reference, krusty }
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

    /// The whole `javap -v -p` listing of `class` from kotlinc and from krusty: its header, every
    /// field and method with its code and debug tables, and the class attributes
    /// (`EnclosingMethod`, `InnerClasses`, `Signature`, `@Metadata`). The constant pool, the
    /// listing's file details and pool indices are left out: the two pools may be laid out
    /// differently.
    pub fn class_listing(&self, class: &str) -> (String, String) {
        let listing = |bytes: Option<&Vec<u8>>| {
            let bytes = bytes.unwrap_or_else(|| panic!("{class} was not written"));
            let disassembly = disassemble_with(class, bytes, &["-v", "-p"]);
            assert!(!disassembly.is_empty(), "javap disassembles {class}");
            let mut lines = Vec::new();
            let mut pool_indices = ListingPoolIndices::default();
            let mut in_pool = false;
            for line in disassembly.lines() {
                if line.starts_with("Constant pool:") {
                    in_pool = true;
                } else if line == "{" {
                    in_pool = false;
                }
                let file_detail = [
                    "Classfile ",
                    "Last modified",
                    "SHA-256",
                    "MD5",
                    "Compiled from",
                ]
                .iter()
                .any(|prefix| line.trim_start().starts_with(prefix));
                if in_pool || file_detail {
                    continue;
                }
                lines.push(pool_indices.erase(line));
            }
            lines.join("\n")
        };
        (
            listing(self.reference.get(class)),
            listing(self.krusty.get(class)),
        )
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
    classes_against_kotlinc_lib_request(name, lib, src, jvm_target, None)
}

/// [`classes_against_kotlinc_lib`] with the dependency and consumer compiled under the same
/// explicit public language/API configuration on both compilers.
pub fn classes_against_kotlinc_lib_language_settings(
    name: &str,
    lib: &[(&str, &str)],
    src: &str,
    language_settings: &krusty::language_settings::LanguageSettings,
) -> Option<ClassSets> {
    classes_against_kotlinc_lib_request(name, lib, src, None, Some(language_settings))
}

fn classes_against_kotlinc_lib_request(
    name: &str,
    lib: &[(&str, &str)],
    src: &str,
    jvm_target: Option<u16>,
    language_settings: Option<&krusty::language_settings::LanguageSettings>,
) -> Option<ClassSets> {
    let kotlinc_extra = language_settings
        .map(kotlinc_language_options)
        .unwrap_or_default();
    let library = super::common_core::kotlinc_lib::kotlinc_lib_out_with(lib, &kotlinc_extra)?;
    let target = jvm_target
        .map(super::common_core::kotlinc_jvm_target_argument)
        .unwrap_or_else(|| "default".to_string());
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
        inputs.legacy_fingerprint,
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
            if jvm_target.is_some() {
                arguments.push("-jvm-target".to_string());
                arguments.push(target.clone());
            }
            arguments.extend(kotlinc_extra.iter().cloned());
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
    let class_major = jvm_target.map(|target| target + 44);
    let krusty = match language_settings {
        Some(language_settings) => super::common_core::source_set_compile::compile(
            &[(name, src)],
            &classpath,
            None,
            class_major,
            Some(language_settings.language_version.metadata_version()),
            language_settings,
        ),
        None => super::common_core::compile_in_process_metadata_cp_module_target(
            src,
            name,
            &classpath,
            "main",
            class_major,
        ),
    }
    .unwrap_or_else(|| panic!("{name}: krusty failed to compile"))
    .into_iter()
    .filter(|(name, _)| !name.ends_with(".kotlin_module"))
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
    assert_class_code_comparison(class, comparison)
}

/// [`assert_class_code_matches_kotlinc`] for fixtures that only resolve with the JDK modules
/// beside the stdlib (see [`compare_with_kotlinc_plugin_jdk`]).
pub fn assert_class_code_matches_kotlinc_jdk(
    stem: &str,
    source: &str,
    class: &str,
) -> ReferenceComparison {
    let comparison = compare_with_kotlinc_plugin_jdk(
        stem,
        source,
        class,
        "17",
        &super::common_core::language_directives::kotlinc_args(source),
    )
    .expect("reference kotlinc and javap are provisioned");
    assert_class_code_comparison(class, comparison)
}

/// [`assert_class_code_matches_kotlinc_jdk`] with `extra_cp` on both compilers' classpaths (see
/// [`compare_with_kotlinc_plugin_jdk_cp`]).
pub fn assert_class_code_matches_kotlinc_jdk_cp(
    stem: &str,
    source: &str,
    class: &str,
    extra_cp: &[PathBuf],
) -> ReferenceComparison {
    let comparison = compare_with_kotlinc_plugin_jdk_cp(
        stem,
        source,
        class,
        extra_cp,
        "17",
        &super::common_core::language_directives::kotlinc_args(source),
    )
    .expect("reference kotlinc and javap are provisioned");
    assert_class_code_comparison(class, comparison)
}

fn assert_class_code_comparison(
    class: &str,
    comparison: ReferenceComparison,
) -> ReferenceComparison {
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

/// Compile `src` (file `<stem>.kt`, module `main`) with kotlinc and krusty, and assert each class in
/// `classes` is byte-identical to kotlinc's.
pub fn assert_classes_identical_to_kotlinc(stem: &str, src: &str, classes: &[&str]) {
    assert_classes_identical_to_kotlinc_against(stem, src, classes, &[]);
}

/// [`assert_classes_identical_to_kotlinc`] with `libraries` on both compilers' classpath.
pub fn assert_classes_identical_to_kotlinc_against(
    stem: &str,
    src: &str,
    classes: &[&str],
    libraries: &[std::path::PathBuf],
) {
    assert_classes_identical_to_kotlinc_full(stem, src, classes, libraries, false);
}

/// [`assert_classes_identical_to_kotlinc_against`] with the JDK modules on krusty's classpath.
/// Dependency fixtures that expose an inline body using a JDK classifier need both inputs: the
/// repository-built dependency and the platform declarations its copied body resolves against.
pub fn assert_classes_identical_to_kotlinc_against_jdk(
    stem: &str,
    src: &str,
    classes: &[&str],
    libraries: &[std::path::PathBuf],
) {
    assert_classes_identical_to_kotlinc_full(stem, src, classes, libraries, true);
}

/// [`assert_classes_identical_to_kotlinc`] with the JDK modules beside the stdlib on the KRUSTY
/// side only, as [`compare_with_kotlinc_plugin_jdk`] arranges: a fixture naming a JDK-hierarchy
/// type (the `StringBuilder` typealias) does not resolve for krusty without them, while the
/// reference kotlinc always has its own JDK.
pub fn assert_classes_identical_to_kotlinc_jdk(stem: &str, src: &str, classes: &[&str]) {
    assert_classes_identical_to_kotlinc_full(stem, src, classes, &[], true);
}

fn assert_classes_identical_to_kotlinc_full(
    stem: &str,
    src: &str,
    classes: &[&str],
    libraries: &[std::path::PathBuf],
    jdk_modules: bool,
) {
    let dir = super::common_core::scratch_dir().expect("scratch directory");
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    let source = dir.join(format!("{stem}.kt"));
    std::fs::write(&source, src).expect("write fixture");
    let mut arguments = vec![
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
    ];
    if !libraries.is_empty() {
        let classpath = std::env::join_paths(libraries).expect("classpath");
        arguments.push("-cp".to_string());
        arguments.push(classpath.to_string_lossy().into_owned());
    }
    arguments.push(source.to_string_lossy().into_owned());
    let (code, stderr) =
        super::common_core::kotlinc_compile(&arguments).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let mut classpath = vec![super::common_core::stdlib_jar()];
    classpath.extend_from_slice(libraries);
    if jdk_modules {
        classpath.push(super::common_core::jdk_modules());
    }
    let krusty = super::common_core::compile_in_process_metadata_cp_module_target(
        src, stem, &classpath, "main", None,
    )
    .expect("krusty compiles the fixture");
    let mut differences = Vec::new();
    for class in classes {
        let reference = std::fs::read(reference_dir.join(format!("{class}.class")))
            .expect("kotlinc emits the class");
        let ours = krusty
            .iter()
            .find(|(internal, _)| internal == class)
            .map(|(_, bytes)| bytes)
            .expect("krusty emits the class");
        if &reference != ours {
            differences.push(exact_class_difference(class, &reference, ours));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's build:\n\n{}",
        differences.join("\n\n")
    );
}

/// The semantic surfaces behind an exact class-byte mismatch. Constant-pool ordering remains part
/// of the enclosing assertion, but this report names the header, member, code/debug, and metadata
/// difference that must be corrected instead of reducing the failure to a class name.
fn exact_class_difference(class: &str, reference: &[u8], ours: &[u8]) -> String {
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
    let reference_disassembly = disassemble(class, reference);
    let ours_disassembly = disassemble(class, ours);
    format!(
        "{class} differs (kotlinc {} bytes, krusty {} bytes)\n\
         headers:\n  kotlinc: {:?}\n  krusty:  {:?}\n\
         member tables:\n  kotlinc: {:#?}\n  krusty:  {:#?}\n\
         members/code/debug:\n--- kotlinc ---\n{}\n--- krusty ---\n{}\n\
         metadata:\n  kotlinc: {:#?}\n  krusty:  {:#?}",
        reference.len(),
        ours.len(),
        header(reference),
        header(ours),
        member_table(reference),
        member_table(ours),
        member_blocks(&reference_disassembly).join("\n"),
        member_blocks(&ours_disassembly).join("\n"),
        super::common_core::raw_kotlin_metadata(reference),
        super::common_core::raw_kotlin_metadata(ours),
    )
}

#[cfg(test)]
mod pool_index_tests {
    use super::ListingPoolIndices;

    fn erase(listing: &[&str]) -> Vec<String> {
        let mut indices = ListingPoolIndices::default();
        listing.iter().map(|line| indices.erase(line)).collect()
    }

    #[test]
    fn literal_hash_digits_in_an_annotation_stay_exact() {
        assert_eq!(
            erase(&[
                "RuntimeVisibleAnnotations:",
                "  0: #72(#73=s#74,#75=[s#76])",
                r##"    Ticket("#123", "a #123 b")"##,
                "    Ticket(",
                r##"      value="ticket#123""##,
                r##"      ids=["#7"]"##,
                "    )",
                "  1: #154(#155=[I#156])",
                "    Ids(",
                "      ids=[123]",
                "    )",
                "Signature: #12                          // TT;",
            ]),
            [
                "RuntimeVisibleAnnotations:",
                "0: #(#=s#,#=[s#])",
                r##"Ticket("#123", "a #123 b")"##,
                "Ticket(",
                r##"value="ticket#123""##,
                r##"ids=["#7"]"##,
                ")",
                "1: #(#=[I#])",
                "Ids(",
                "ids=[123]",
                ")",
                "Signature: # // TT;",
            ]
        );
    }

    #[test]
    fn encoded_annotation_rows_erase_every_pool_index() {
        assert_eq!(
            erase(&[
                "  0: #72()",
                "  1: #154(#155=[I#156])",
                "  2: #9(#10=e#11.#12,#13=@#14(#15=s#16),#17=c#18,#19=[])",
                "  0: #24(): FIELD",
                "  default_value: I#12"
            ],),
            [
                "0: #()",
                "1: #(#=[I#])",
                "2: #(#=e#.#,#=@#(#=s#),#=c#,#=[])",
                "0: #(): FIELD",
                "default_value: I#",
            ]
        );
    }

    #[test]
    fn operand_and_attribute_indices_erase() {
        assert_eq!(
            erase(&[
                "   3: invokestatic  #12                 // Method f:()V",
                "   6: invokedynamic #43,  0",
                "  public static final #10= #2 of #4;",
            ]),
            [
                "3: invokestatic # // Method f:()V",
                "6: invokedynamic #, 0",
                "public static final #= # of #;",
            ]
        );
    }

    #[test]
    fn a_row_outside_the_encoded_grammar_is_left_alone() {
        assert_eq!(
            erase(&[r##"  0: #72(value="ticket#123")"##, r##"    "ticket#123""##]),
            [r##"0: #72(value="ticket#123")"##, r##""ticket#123""##]
        );
    }
}

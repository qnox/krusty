//! Byte equality for KLIBs: build the same module with krusty and with the reference compiler, then
//! compare the two artifacts entry by entry.
//!
//! A `.klib` is a distributed artifact, so "krusty writes a klib the reader accepts" is not the bar.
//! A dependent resolves against its `unique_name`, a linker walks its fragments, and a fingerprint
//! over its bytes decides whether a downstream module recompiles — every one of which reads bytes
//! kotlinc wrote. The bar is therefore the same one the JVM lane already holds itself to, and the
//! instrument is the same shape: [`KRUSTY_LIB_BYTEDIFF_REPORT`](super)'s `LIBDIFF` lines have a
//! `KLIBDIFF` counterpart here.
//!
//! The METADATA-only klib is the comparison target, and the DIRECTORY shape is what both sides
//! write. `K2MetadataCompiler -Xmetadata-klib` produces a directory with no `default/ir` at all, so
//! a difference is a difference in declarations rather than in zip container metadata that no
//! consumer reads for meaning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::common;

/// Per-entry verdicts comparing a krusty-written klib against a reference-written one.
#[derive(Debug, Default)]
pub struct KlibEntryDiff {
    /// Entries present in both, with identical bytes.
    pub identical: Vec<String>,
    /// Present in both and different, with each side's length.
    pub divergent: Vec<(String, usize, usize)>,
    /// Written by the reference compiler and not by krusty — the work not yet done.
    pub missing: Vec<String>,
    /// Written by krusty and not by the reference compiler — invented output, always a defect.
    pub extra: Vec<String>,
}

impl KlibEntryDiff {
    /// Every entry the reference compiler wrote, however krusty fared on it.
    pub fn reference_entries(&self) -> usize {
        self.identical.len() + self.divergent.len() + self.missing.len()
    }

    /// A single line per entry on stderr, under the same opt-in the JVM lane uses. The prefix is
    /// grep-able so a run's whole ledger can be read off a log.
    pub fn report(&self, tag: &str) {
        let mut lines = Vec::new();
        for entry in &self.identical {
            lines.push(format!("KLIBDIFF\tidentical\t{entry}\t{tag}"));
        }
        for (entry, krusty, reference) in &self.divergent {
            lines.push(format!(
                "KLIBDIFF\tdivergent\t{entry}\t{tag}\tkrusty={krusty}\tref={reference}"
            ));
        }
        for entry in &self.missing {
            lines.push(format!("KLIBDIFF\tmissing\t{entry}\t{tag}"));
        }
        for entry in &self.extra {
            lines.push(format!("KLIBDIFF\textra\t{entry}\t{tag}"));
        }
        lines.push(format!(
            "KLIBDIFF\tsummary\t{tag}\tidentical={}\tdivergent={}\tmissing={}\textra={}",
            self.identical.len(),
            self.divergent.len(),
            self.missing.len(),
            self.extra.len()
        ));
        emit_report("KRUSTY_KLIB_BYTEDIFF_REPORT", &lines);
    }
}

/// Compare two klib DIRECTORIES entry by entry.
pub fn diff_klib_directories(krusty: &Path, reference: &Path) -> KlibEntryDiff {
    let ours = read_tree(krusty);
    let theirs = read_tree(reference);
    let mut diff = KlibEntryDiff::default();
    for (entry, reference_bytes) in &theirs {
        match ours.get(entry) {
            None => diff.missing.push(entry.clone()),
            Some(our_bytes) if our_bytes == reference_bytes => diff.identical.push(entry.clone()),
            Some(our_bytes) => {
                diff.divergent
                    .push((entry.clone(), our_bytes.len(), reference_bytes.len()))
            }
        }
    }
    for entry in ours.keys() {
        if !theirs.contains_key(entry) {
            diff.extra.push(entry.clone());
        }
    }
    diff
}

fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

fn collect(root: &Path, current: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(directory) = std::fs::read_dir(current) else {
        return;
    };
    for child in directory.flatten() {
        let path = child.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if let (Ok(relative), Ok(bytes)) = (path.strip_prefix(root), std::fs::read(&path)) {
            out.insert(relative.to_string_lossy().replace('\\', "/"), bytes);
        }
    }
}

/// Compile `sources` into a METADATA-only klib with the reference compiler, as a directory.
///
/// This is `K2MetadataCompiler`, not the JVM or JS one: it is the compiler whose klib carries
/// declarations and nothing else, which makes it the target krusty's own writer can be held to
/// before any IR serialization exists.
///
/// `None` = the toolchain is unavailable. kotlinc REJECTING the sources panics — an invalid fixture
/// must never read as a skip.
pub fn kotlinc_metadata_klib(tag: &str, sources: &[(&str, &str)]) -> Option<PathBuf> {
    let stdlib = common::stdlib_jar();
    if !stdlib.is_file() {
        return None;
    }
    let work = common::scratch_dir()?.join(format!("metadata-klib-{tag}"));
    let out = work.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let mut args = vec![
        "-Xmetadata-klib".to_string(),
        "-classpath".to_string(),
        stdlib.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
    ];
    for (name, source) in sources {
        let path = work.join(name);
        std::fs::write(&path, source).ok()?;
        args.push(path.to_string_lossy().into_owned());
    }
    match common::kotlinc_compile_with(
        "org.jetbrains.kotlin.cli.metadata.K2MetadataCompiler",
        &args,
    ) {
        Some((0, _)) => Some(out),
        Some((code, err)) => panic!("kotlinc(metadata-klib) failed ({code}): {err}"),
        None => None,
    }
}

/// Whether the klib dependency lane is on: `KRUSTY_LIB_KLIB=1`.
///
/// Off by default, because it builds a second artifact per dependency fixture. On, every dependency
/// a box test consumes is ALSO produced as a klib and the two carriers are required to declare the
/// same API — see [`LibBuild::cross_check_klib_declarations`](super::LibBuild).
#[allow(dead_code)]
pub fn klib_dep_lane_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            std::env::var("KRUSTY_LIB_KLIB").as_deref(),
            Ok("1") | Ok("true") | Ok("on")
        )
    })
}

/// What a classifier declares, in the terms BOTH carriers state the same way.
///
/// Deliberately not a whole [`krusty::libraries::LibraryType`]: a JVM classfile's descriptors are
/// erased and a klib's are not, so comparing rendered member types would report noise rather than
/// disagreement. The kind and the type-parameter names are Kotlin facts both carriers record, and a
/// difference in either is a real difference in the declared API.
///
/// One fact the two carriers spell differently, reconciled here rather than skipped: an ENUM ENTRY.
/// Kotlin metadata records it as a `Class` message with `CLASS_KIND = ENUM_ENTRY`, while krusty's
/// [`krusty::libraries::TypeKind`] has no entry variant and the JVM provider reports the entry as
/// `importable_declaration` with NO classifier — which is that provider's documented way of saying
/// "importable, but not a classifier". Both sides are normalized to `EnumEntry` below, so an entry
/// still has to be present on both rather than being excluded from the comparison.
#[derive(Debug, PartialEq, Eq)]
pub struct ClassifierShape {
    pub kind: String,
    pub type_params: Vec<String>,
}

/// `Class.flags` CLASS_KIND (bits 6..9) for an enum entry, straight from the metadata schema. Read
/// from the raw flag word the reader preserves, rather than inferred from the enclosing declaration.
const CLASS_KIND_ENUM_ENTRY: u64 = 3;

/// The kind both carriers are compared on.
fn shape_kind(kind: krusty::libraries::TypeKind, flags: u64) -> String {
    if (flags >> 6) & 0x7 == CLASS_KIND_ENUM_ENTRY {
        return "EnumEntry".to_string();
    }
    format!("{kind:?}")
}

/// The declaration surface a dependency presents.
#[derive(Debug, Default)]
pub struct DeclarationSurface {
    /// Internal classifier name → its shape.
    pub classifiers: BTreeMap<String, ClassifierShape>,
    /// Top-level functions as (package, name, value arity).
    pub functions: std::collections::BTreeSet<(String, String, usize)>,
}

/// Per-declaration verdicts comparing the klib carrier against the JVM one.
#[derive(Debug, Default)]
pub struct SurfaceDiff {
    pub identical: Vec<String>,
    /// A declaration both carriers have, disagreeing — with each side's rendering.
    pub divergent: Vec<(String, String, String)>,
    /// Declared in the klib and not visible through the JVM provider.
    pub missing: Vec<String>,
}

impl SurfaceDiff {
    pub fn report(&self, tag: &str) {
        let mut lines = Vec::new();
        for declaration in &self.identical {
            lines.push(format!("KLIBDEP\tidentical\t{declaration}\t{tag}"));
        }
        for (declaration, klib, jvm) in &self.divergent {
            lines.push(format!(
                "KLIBDEP\tdivergent\t{declaration}\t{tag}\tklib={klib}\tjvm={jvm}"
            ));
        }
        for declaration in &self.missing {
            lines.push(format!("KLIBDEP\tmissing\t{declaration}\t{tag}"));
        }
        lines.push(format!(
            "KLIBDEP\tsummary\t{tag}\tidentical={}\tdivergent={}\tmissing={}",
            self.identical.len(),
            self.divergent.len(),
            self.missing.len()
        ));
        emit_report("KRUSTY_LIB_KLIB_REPORT", &lines);
    }
}

/// Write a ledger under `variable`: to the FILE it names, or to stderr for `1`.
///
/// A file is the useful default for a whole-suite run. libtest captures a passing test's stderr and
/// prints it only for failures, so a report on stderr silently measures the failures alone — which
/// reads as a plausible ledger and is not one. `--nocapture` would fix that and bury the run in
/// 5000 lines of test output; a file survives capture and aggregates across parallel tests.
pub fn emit_report(variable: &str, lines: &[String]) {
    let Ok(destination) = std::env::var(variable) else {
        return;
    };
    if matches!(destination.as_str(), "1" | "true" | "on") {
        for line in lines {
            eprintln!("{line}");
        }
        return;
    }
    use std::io::Write as _;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&destination)
    else {
        for line in lines {
            eprintln!("{line}");
        }
        return;
    };
    let mut body = lines.join("\n");
    body.push('\n');
    let _ = file.write_all(body.as_bytes());
}

/// The surface a klib declares, excluding the standard library it was compiled against.
pub fn surface_from_klib(klib: &Path) -> DeclarationSurface {
    let mut surface = DeclarationSurface::default();
    let Some(archive) = krusty::klib::KlibArchive::open(klib) else {
        return surface;
    };
    for fragment in archive.package_fragments() {
        let Some(bytes) = archive.read(&fragment.entry) else {
            continue;
        };
        let package = krusty::metadata::reader::parse_package_fragment(&bytes);
        for (internal, declaration) in package.classes {
            surface.classifiers.insert(
                internal,
                ClassifierShape {
                    kind: shape_kind(declaration.kind, declaration.flags),
                    type_params: declaration
                        .type_params
                        .iter()
                        .map(|parameter| parameter.name.clone())
                        .collect(),
                },
            );
        }
        for function in package.functions {
            // A member of a receiver is not a top-level declaration of the package.
            if function.receiver.is_some() {
                continue;
            }
            surface.functions.insert((
                fragment.package_fqname.replace('.', "/"),
                function.name,
                function.params.len(),
            ));
        }
    }
    surface
}

/// The same surface, as krusty's JVM provider reports it from a compiled classpath directory.
///
/// Only the declarations the klib named are queried: the point is whether the two carriers agree
/// about the dependency's API, not to enumerate a classpath that also holds the whole stdlib.
pub fn surface_from_classpath(
    classpath: &[PathBuf],
    wanted: &DeclarationSurface,
) -> DeclarationSurface {
    use krusty::symbol_source::{SymbolNamespace, SymbolSource};

    let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(classpath.to_vec()));
    let libraries = krusty::jvm::jvm_libraries::JvmLibraries::new(cp);
    let mut surface = DeclarationSurface::default();
    for internal in wanted.classifiers.keys() {
        let identity = krusty::types::type_name(internal);
        let (namespace, leaf) = SymbolNamespace::classifier_key(identity);
        let record = libraries.symbols(namespace, leaf);
        let Some(classifier) = record.classifier.as_ref() else {
            // No classifier but importable: the provider's way of declaring an enum entry.
            if record.importable_declaration {
                surface.classifiers.insert(
                    internal.clone(),
                    ClassifierShape {
                        kind: "EnumEntry".to_string(),
                        type_params: Vec::new(),
                    },
                );
            }
            continue;
        };
        surface.classifiers.insert(
            internal.clone(),
            ClassifierShape {
                kind: format!("{:?}", classifier.kind),
                type_params: classifier.type_parameters.type_params().clone(),
            },
        );
    }
    for (package, name, _) in &wanted.functions {
        let namespace = SymbolNamespace::Package(krusty::types::type_name(package));
        let record = libraries.symbols(namespace, name);
        if let krusty::libraries::Callables::Functions(functions)
        | krusty::libraries::Callables::Both { functions, .. } = &record.callables
        {
            for overload in functions.top_level() {
                surface.functions.insert((
                    package.clone(),
                    name.clone(),
                    overload.callable.params.len(),
                ));
            }
        }
    }
    surface
}

/// Compare the klib's surface against the JVM provider's.
pub fn diff_surfaces(klib: &DeclarationSurface, jvm: &DeclarationSurface) -> SurfaceDiff {
    let mut diff = SurfaceDiff::default();
    for (internal, shape) in &klib.classifiers {
        match jvm.classifiers.get(internal) {
            None => diff.missing.push(internal.clone()),
            Some(other) if other == shape => diff.identical.push(internal.clone()),
            Some(other) => {
                diff.divergent
                    .push((internal.clone(), format!("{shape:?}"), format!("{other:?}")))
            }
        }
    }
    for entry in &klib.functions {
        let (package, name, arity) = entry;
        let rendered = format!("{package}/{name}({arity})");
        if jvm.functions.contains(entry) {
            diff.identical.push(rendered);
        } else {
            diff.missing.push(rendered);
        }
    }
    diff
}

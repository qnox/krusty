//! Transactional per-entry indexes derived from authoritative Kotlin metadata.
//!
//! A malformed metadata-bearing declaration aborts the whole entry build. Callers may therefore
//! publish a successful result atomically without confusing an invalid declaration with an absent
//! alias or extension.

use super::archive_entries::ArchiveEntries;
use super::{
    descriptor_parts, enter_directory, push_id_dedup, Entry, EntryExt, JarPackages, TypeIndex,
};
use crate::fx_hash::{HashMap, HashSet};
use crate::jvm::classreader::{parse_class, ClassInfo, ReadError};
use crate::name_tree::{NameId, NameTree};
use crate::types::{type_name, TypeName};
use std::fs::File;
use std::path::{Path, PathBuf};

/// Why an entry's declarations could not be indexed or loaded.
#[derive(Debug)]
pub(super) enum ClassMetadataLoadError {
    /// A class whose bytes or Kotlin metadata do not parse.
    Class(TypeName, ReadError),
    /// An archive entry whose name does not decode, so it cannot be classified.
    EntryName {
        archive: PathBuf,
        error: zip::result::ZipError,
    },
}

impl ClassMetadataLoadError {
    /// The terminal frontend diagnostic for this failure.
    pub(super) fn diagnostic(&self) -> String {
        match self {
            ClassMetadataLoadError::Class(internal, error) => {
                let detail = match error {
                    ReadError::BadKotlinMetadata(error) => {
                        format!("invalid Kotlin metadata: {error:?}")
                    }
                    ReadError::NotAClass => "entry is not a JVM class".to_string(),
                    ReadError::Truncated => "truncated JVM class".to_string(),
                    ReadError::BadConstant(tag) => format!("invalid constant-pool tag {tag}"),
                };
                format!(
                    "cannot load classpath declaration {}: {detail}",
                    internal.render()
                )
            }
            ClassMetadataLoadError::EntryName { archive, error } => format!(
                "cannot read classpath entry {}: undecodable entry name: {error}",
                archive.display()
            ),
        }
    }
}

impl super::Classpath {
    /// Accept one entry's index build. A failure becomes the classpath's terminal load error and
    /// yields no index, so lookup stops at that entry instead of reading a partial one.
    pub(super) fn accept_entry_index<T>(
        &self,
        built: Result<T, ClassMetadataLoadError>,
    ) -> Option<T> {
        built
            .map_err(|failure| self.record_metadata_load_error(failure))
            .ok()
    }
}

/// The name of entry `entry_id` of the archive at `path`. A name that does not decode fails the
/// whole build: the entry cannot be classified, so skipping it would publish an index without its
/// declarations.
pub(super) fn archive_entry_name<'a>(
    archive: &'a impl ArchiveEntries,
    path: &Path,
    entry_id: usize,
) -> Result<std::borrow::Cow<'a, str>, ClassMetadataLoadError> {
    archive
        .entry_name(entry_id)
        .map_err(|error| ClassMetadataLoadError::EntryName {
            archive: path.to_path_buf(),
            error,
        })
}

fn open_jar(jar: &Path) -> Option<zip::ZipArchive<File>> {
    zip::ZipArchive::new(File::open(jar).ok()?).ok()
}

fn parse_metadata_class(
    bytes: &[u8],
    internal: &str,
) -> Result<Option<ClassInfo>, ClassMetadataLoadError> {
    match parse_class(bytes) {
        Ok(class) => Ok(Some(class)),
        Err(error @ ReadError::BadKotlinMetadata(_)) => {
            Err(ClassMetadataLoadError::Class(type_name(internal), error))
        }
        Err(_) => Ok(None),
    }
}

/// A lean per-class record for building the extension index — only what's needed to follow facade
/// superclass chains and index static methods (no fields, no instance methods).
struct ClassLite {
    is_public: bool,
    super_class: Option<NameId>,
    /// `(name, descriptor, generic-signature, is_public)` of each static method (excl `<init>`/`<clinit>`).
    /// Non-public ones (`@InlineOnly`) are kept for the inliner; the flag gates normal resolution.
    statics: Vec<(String, String, Option<String>, bool)>,
    /// JVM names of functions `@Metadata` marks as genuine TOP-LEVEL (NO extension receiver). A top-level
    /// generic whose first parameter erases to `Object` (`assertEquals<T>(T, T, String)`) is otherwise
    /// indistinguishable in bytecode from an extension, so a name that is ONLY ever top-level must NOT be
    /// keyed by its first parameter in `by_recv`. Name-keyed (not name+desc): `@Metadata` often omits the
    /// method descriptor (`jvm_desc=None`).
    toplevel_names: HashSet<String>,
    /// JVM names `@Metadata` marks as EXTENSIONS (receiver of any kind — class OR type parameter). A name
    /// that is an extension anywhere is NEVER excluded from `by_recv` (so `takeIf`/`uppercase` stay indexed).
    ext_names: HashSet<String>,
}

/// Copy an interned classifier into the local index by its stored `/` segments.
///
/// [`crate::types::insert_type_name_in`] also publishes `exact_nested_owner`. That inserts the
/// lexical owner (`Outer`) beside the physical classfile node (`Outer$Inner`) and shifts later
/// [`NameId`]s. The class index only follows the path the class file recorded.
fn insert_classifier(names: &NameTree, internal: TypeName) -> NameId {
    let mut segments = Vec::new();
    let mut current = internal;
    while current != TypeName::ROOT {
        segments.push(current.segment_ref());
        current = current.parent().unwrap_or(TypeName::ROOT);
    }
    let mut parent = NameTree::ROOT;
    for segment in segments.into_iter().rev() {
        parent = names.child_of(parent, segment);
    }
    parent
}

fn collect_class_bytes(
    bytes: &[u8],
    internal: &str,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
) -> Result<(), ClassMetadataLoadError> {
    let Some(class) = parse_metadata_class(bytes, internal)? else {
        return Ok(());
    };
    let this_class = insert_classifier(names, class.this_class);
    let super_class = class.super_class.map(|name| insert_classifier(names, name));
    let statics = class
        .methods
        .iter()
        .filter(|method| method.is_static() && !method.name.starts_with('<'))
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.signature.clone(),
                method.is_public(),
            )
        })
        .collect();
    let mut toplevel_names = HashSet::default();
    let mut ext_names = HashSet::default();
    for function in crate::jvm::metadata::package_functions(&class)
        .iter()
        .chain(crate::jvm::metadata::class_functions(&class).iter())
    {
        if function.is_extension() {
            ext_names.insert(function.jvm_name.clone());
        } else {
            toplevel_names.insert(function.jvm_name.clone());
        }
    }
    all.insert(
        this_class,
        ClassLite {
            is_public: class.is_public(),
            super_class,
            statics,
            toplevel_names,
            ext_names,
        },
    );
    Ok(())
}

fn collect_dir(
    directory: &Path,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
) -> Result<(), ClassMetadataLoadError> {
    let mut ancestors = HashSet::default();
    collect_dir_visited(directory, directory, names, all, &mut ancestors)
}

fn collect_dir_visited(
    root: &Path,
    directory: &Path,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
    ancestors: &mut HashSet<PathBuf>,
) -> Result<(), ClassMetadataLoadError> {
    let Ok(Some(canonical)) = enter_directory(directory, ancestors) else {
        return Ok(());
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        ancestors.remove(&canonical);
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dir_visited(root, &path, names, all, ancestors)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "class")
        {
            if let Ok(bytes) = std::fs::read(&path) {
                let Some(internal) = path
                    .strip_prefix(root)
                    .ok()
                    .and_then(Path::to_str)
                    .map(|path| path.replace('\\', "/"))
                    .and_then(|path| class_internal_from_entry(&path).map(str::to_owned))
                else {
                    continue;
                };
                collect_class_bytes(&bytes, &internal, names, all)?;
            }
        }
    }
    ancestors.remove(&canonical);
    Ok(())
}

fn collect_archive(
    archive: &mut impl ArchiveEntries,
    jar: &Path,
    packages: &JarPackages,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
) -> Result<(), ClassMetadataLoadError> {
    for entry_id in 0..archive.entry_count() {
        let internal = class_internal_from_entry(&archive_entry_name(archive, jar, entry_id)?)
            .filter(|internal| ext_scan_wanted(internal, packages))
            .map(str::to_owned);
        let Some(internal) = internal else {
            continue;
        };
        if let Some(bytes) = archive.entry_bytes(entry_id) {
            collect_class_bytes(&bytes, &internal, names, all)?;
        }
    }
    Ok(())
}

/// Scan one classpath entry into its extension contribution. The result is returned only after the
/// complete entry succeeds, so the caller can publish it atomically.
pub(super) fn build_entry_ext(
    entry: &Entry,
    packages: &JarPackages,
) -> Result<EntryExt, ClassMetadataLoadError> {
    let mut names = NameTree::default();
    let mut all: HashMap<NameId, ClassLite> = HashMap::default();
    match entry {
        Entry::Dir(directory) => collect_dir(directory, &mut names, &mut all)?,
        Entry::Jar(jar) => {
            if let Some(mut archive) = open_jar(jar) {
                collect_archive(&mut archive, jar, packages, &mut names, &mut all)?;
            }
        }
        Entry::Jimage(_) | Entry::CtSym { .. } => {}
    }
    Ok(extensions_of(&names, &all))
}

/// Index the collected classes' static methods by name and receiver, following facade superclass
/// chains.
fn extensions_of(names: &NameTree, all: &HashMap<NameId, ClassLite>) -> EntryExt {
    let mut extensions = EntryExt::default();
    for class in all.values() {
        extensions
            .toplevel_names
            .extend(class.toplevel_names.iter().cloned());
        extensions.ext_names.extend(class.ext_names.iter().cloned());
    }
    for (&root, root_class) in all {
        let mut root_id = None;
        let mut current = Some(root);
        let mut visited = HashSet::default();
        while let Some(class_name) = current {
            if !visited.insert(class_name) {
                break;
            }
            let Some(class) = all.get(&class_name) else {
                break;
            };
            for (method_name, descriptor, _signature, public) in &class.statics {
                if !root_class.is_public && *public {
                    continue;
                }
                let Some((first_parameter, _return_descriptor)) = descriptor_parts(descriptor)
                else {
                    continue;
                };
                let owner =
                    *root_id.get_or_insert_with(|| extensions.owner_names.insert_from(names, root));
                push_id_dedup(&mut extensions.by_name, method_name, owner);
                if let Some(receiver) = first_parameter {
                    extensions
                        .by_recv_raw
                        .entry(receiver)
                        .or_default()
                        .push((method_name.clone(), owner));
                }
            }
            current = class.super_class;
        }
    }
    extensions
}

/// `Xxx.class` entry name (jar/jimage path) → internal name, or `None` if not an indexable class.
fn class_internal_from_entry(name: &str) -> Option<&str> {
    name.strip_suffix(".class").filter(|name| !name.is_empty())
}

fn ext_scan_wanted(internal: &str, packages: &JarPackages) -> bool {
    packages.contains_facade(internal)
        || internal
            .rsplit('/')
            .next()
            .unwrap_or(internal)
            .contains("Kt")
}

fn type_alias_scan_wanted(internal: &str, packages: &JarPackages) -> bool {
    packages.contains_facade(internal) || is_type_aliases_kt(internal)
}

fn parse_aliases_from_bytes(
    bytes: &[u8],
    internal: &str,
    index: &mut TypeIndex,
) -> Result<(), ClassMetadataLoadError> {
    let Some(class) = parse_metadata_class(bytes, internal)? else {
        return Ok(());
    };
    for alias in crate::jvm::metadata::metadata_type_aliases(&class) {
        let name = alias.name;
        index.type_aliases.insert(name, type_name(&alias.target));
        index.alias_expansions.insert(
            name,
            (
                type_name(&alias.target),
                alias.formals.clone(),
                alias.expansion,
                alias.expansion_spelling.clone(),
            ),
        );
    }
    Ok(())
}

/// A Kotlin file facade (`*Kt`) is where a top-level type alias is recorded.
fn is_type_aliases_kt(internal: &str) -> bool {
    internal
        .rsplit('/')
        .next()
        .unwrap_or(internal)
        .ends_with("Kt")
}

pub(super) fn build_entry_types(
    entry: &Entry,
    packages: &JarPackages,
) -> Result<TypeIndex, ClassMetadataLoadError> {
    let mut index = TypeIndex::default();
    match entry {
        Entry::Dir(directory) => scan_types_dir(directory, &mut index)?,
        Entry::Jar(jar) => {
            if let Some(mut archive) = open_jar(jar) {
                scan_types_archive(&mut archive, jar, packages, &mut index)?;
            }
        }
        Entry::Jimage(_) | Entry::CtSym { .. } => {}
    }
    Ok(index)
}

pub(super) fn build_entry_package_types(
    entry: &Entry,
    packages: &JarPackages,
    package: TypeName,
) -> Result<TypeIndex, ClassMetadataLoadError> {
    let mut index = TypeIndex::default();
    match entry {
        Entry::Dir(directory) => {
            if let Some(package_entry) = packages.entry_name(package) {
                for &facade in &package_entry.facades {
                    let internal = packages.names.render(facade);
                    let path = directory.join(format!("{internal}.class"));
                    if let Ok(bytes) = std::fs::read(path) {
                        parse_aliases_from_bytes(&bytes, &internal, &mut index)?;
                    }
                }
            }
        }
        Entry::Jar(jar) => {
            if let Some(mut archive) = open_jar(jar) {
                // Archive entry names are an external classfile format and therefore require text.
                let package = package.render();
                scan_types_archive_package(&mut archive, jar, packages, &package, &mut index)?;
            }
        }
        Entry::Jimage(_) | Entry::CtSym { .. } => {}
    }
    Ok(index)
}

fn scan_types_dir(directory: &Path, index: &mut TypeIndex) -> Result<(), ClassMetadataLoadError> {
    let mut ancestors = HashSet::default();
    scan_types_dir_rooted(directory, directory, index, &mut ancestors)
}

fn scan_types_dir_rooted(
    root: &Path,
    directory: &Path,
    index: &mut TypeIndex,
    ancestors: &mut HashSet<PathBuf>,
) -> Result<(), ClassMetadataLoadError> {
    let Ok(Some(canonical)) = enter_directory(directory, ancestors) else {
        return Ok(());
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        ancestors.remove(&canonical);
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_types_dir_rooted(root, &path, index, ancestors)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "class")
        {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            let Some(internal) = class_internal_from_entry(&relative) else {
                continue;
            };
            if is_type_aliases_kt(internal) {
                if let Ok(bytes) = std::fs::read(&path) {
                    parse_aliases_from_bytes(&bytes, internal, index)?;
                }
            }
        }
    }
    ancestors.remove(&canonical);
    Ok(())
}

fn scan_types_archive(
    archive: &mut impl ArchiveEntries,
    jar: &Path,
    packages: &JarPackages,
    index: &mut TypeIndex,
) -> Result<(), ClassMetadataLoadError> {
    scan_archive_aliases(archive, jar, index, |internal| {
        type_alias_scan_wanted(internal, packages)
    })
}

fn scan_types_archive_package(
    archive: &mut impl ArchiveEntries,
    jar: &Path,
    packages: &JarPackages,
    package: &str,
    index: &mut TypeIndex,
) -> Result<(), ClassMetadataLoadError> {
    scan_archive_aliases(archive, jar, index, |internal| {
        packages.declares_facade(package, internal)
            || (internal.rsplit_once('/').map_or("", |(parent, _)| parent) == package
                && is_type_aliases_kt(internal))
    })
}

fn scan_archive_aliases(
    archive: &mut impl ArchiveEntries,
    jar: &Path,
    index: &mut TypeIndex,
    wanted: impl Fn(&str) -> bool,
) -> Result<(), ClassMetadataLoadError> {
    for entry_id in 0..archive.entry_count() {
        let internal = class_internal_from_entry(&archive_entry_name(archive, jar, entry_id)?)
            .filter(|internal| wanted(internal))
            .map(str::to_owned);
        let Some(internal) = internal else {
            continue;
        };
        if let Some(bytes) = archive.entry_bytes(entry_id) {
            parse_aliases_from_bytes(&bytes, &internal, index)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::archive_entries::test_archives::{archive, UndecodableName};
    use super::super::{EntryCache, EntryKey};
    use super::*;

    #[test]
    fn metadata_index_scans_cataloged_and_conventional_facades() {
        let module = crate::metadata::module::build_kotlin_module(
            &[(
                "p".to_string(),
                vec!["Utils".to_string(), "HelpersKt".to_string()],
            )],
            [2, 4, 0],
        );
        let mut packages = JarPackages::default();
        super::super::module_optional_annotations::record_kotlin_module(
            "META-INF/main.kotlin_module",
            &module,
            &mut packages,
        );

        assert!(ext_scan_wanted("p/Utils", &packages));
        assert!(type_alias_scan_wanted("p/Utils", &packages));
        assert!(ext_scan_wanted("p/HelpersKt", &packages));
        assert!(!ext_scan_wanted("p/Regular", &packages));

        let empty = JarPackages::default();
        assert!(ext_scan_wanted("p/HelpersKt", &empty));
        assert!(!ext_scan_wanted("p/Regular", &empty));
    }

    #[test]
    fn class_index_copies_classifier_segments_without_nested_owners() {
        let nested = type_name("krusty/index/Outer$Inner");
        assert!(nested.nested_owner().is_some());

        let copied = NameTree::default();
        let copied_id = insert_classifier(&copied, nested);
        assert_eq!(copied.render(copied_id), "krusty/index/Outer$Inner");
        assert_eq!(copied.get("krusty/index/Outer"), None);

        let with_owner = NameTree::default();
        crate::types::insert_type_name_in(&with_owner, nested);
        assert!(with_owner.get("krusty/index/Outer").is_some());
        assert!(with_owner.len() > copied.len());
    }

    fn undecodable_after_a_facade() -> UndecodableName<impl ArchiveEntries> {
        UndecodableName {
            inner: archive(&[("p/AKt.class", b""), ("p/BKt.class", b"")]),
            index: 1,
        }
    }

    fn assert_undecodable_name(failure: ClassMetadataLoadError) {
        assert_eq!(
            failure.diagnostic(),
            "cannot read classpath entry /undecodable/lib.jar: undecodable entry name: \
             i/o error: undecodable entry name"
        );
    }

    fn undecodable_key() -> EntryKey {
        EntryKey {
            path: PathBuf::from("/undecodable/lib.jar"),
            stamp: None,
            jdk_release: None,
        }
    }

    #[test]
    fn an_undecodable_name_after_a_facade_publishes_no_extension_index() {
        let jar = Path::new("/undecodable/lib.jar");
        let cache = EntryCache::<EntryExt>::new();
        let built = cache.get_or_try_build(&undecodable_key(), || {
            let mut names = NameTree::default();
            let mut all = HashMap::default();
            let packages = JarPackages::default();
            let mut archive = undecodable_after_a_facade();
            collect_archive(&mut archive, jar, &packages, &mut names, &mut all)?;
            Ok(extensions_of(&names, &all))
        });

        let Err(failure) = built else {
            panic!("a partial extension index was published");
        };
        assert_undecodable_name(failure);
        assert!(cache.map.read().unwrap().is_empty());
    }

    #[test]
    fn an_undecodable_name_after_a_facade_publishes_no_type_alias_index() {
        let jar = Path::new("/undecodable/lib.jar");
        let cache = EntryCache::<TypeIndex>::new();
        let built = cache.get_or_try_build(&undecodable_key(), || {
            let mut index = TypeIndex::default();
            let packages = JarPackages::default();
            scan_types_archive(
                &mut undecodable_after_a_facade(),
                jar,
                &packages,
                &mut index,
            )?;
            Ok(index)
        });

        let Err(failure) = built else {
            panic!("a partial type-alias index was published");
        };
        assert_undecodable_name(failure);
        assert!(cache.map.read().unwrap().is_empty());
    }

    #[test]
    fn an_undecodable_name_after_a_facade_fails_the_package_type_alias_scan() {
        let jar = Path::new("/undecodable/lib.jar");
        let mut index = TypeIndex::default();
        let packages = JarPackages::default();
        let scanned = scan_types_archive_package(
            &mut undecodable_after_a_facade(),
            jar,
            &packages,
            "p",
            &mut index,
        );

        let Err(failure) = scanned else {
            panic!("a partial package type-alias index was returned");
        };
        assert_undecodable_name(failure);
    }

    #[test]
    fn a_jar_classpath_accepts_an_undecodable_name_as_its_terminal_failure() {
        let jar = Path::new("/undecodable/lib.jar");
        let classpath = super::super::Classpath::new(vec![jar.to_path_buf()]);
        let cache = EntryCache::<TypeIndex>::new();
        let built = cache.get_or_try_build(&undecodable_key(), || {
            let mut index = TypeIndex::default();
            let packages = JarPackages::default();
            scan_types_archive(
                &mut undecodable_after_a_facade(),
                jar,
                &packages,
                &mut index,
            )?;
            Ok(index)
        });
        assert!(classpath.validate_lazy_class_loads().is_ok());

        assert!(classpath.accept_entry_index(built).is_none());
        let error = classpath
            .validate_lazy_class_loads()
            .expect_err("the failure is terminal");
        assert_eq!(
            error.message,
            "cannot read classpath entry /undecodable/lib.jar: undecodable entry name: \
             i/o error: undecodable entry name"
        );
    }
}

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::model::ProjectModel;

const MAX_INVENTORY_ENTRIES: usize = 32 * 1024;

pub type LoadedProjectSources<'a> = (&'a [(String, String)], usize, Vec<String>);

#[derive(Default)]
pub struct ProjectSources {
    cache: Option<Cache>,
}

struct Cache {
    roots: Vec<PathBuf>,
    inferred_roots: Vec<PathBuf>,
    excluded_paths: Vec<PathBuf>,
    documents: Vec<(String, String)>,
    java_documents: Vec<(String, String)>,
    inferred_count: usize,
    kotlin_bytes: usize,
    max_bytes: usize,
}

impl ProjectSources {
    pub fn invalidate(&mut self) {
        self.cache = None;
    }

    pub fn load(
        &mut self,
        model: &ProjectModel,
        documents: &[(&str, &str)],
        open_uris: &[&str],
        max_bytes: usize,
    ) -> Result<LoadedProjectSources<'_>, String> {
        let source_paths = documents
            .iter()
            .filter_map(|(uri, _)| url::Url::parse(uri).ok()?.to_file_path().ok())
            .collect::<HashSet<_>>();
        let excluded_paths = open_uris
            .iter()
            .filter_map(|uri| url::Url::parse(uri).ok()?.to_file_path().ok())
            .collect::<HashSet<_>>();
        let open_bytes = documents
            .iter()
            .try_fold(0usize, |total, (_, source)| total.checked_add(source.len()))
            .filter(|total| *total <= max_bytes)
            .ok_or_else(|| size_limit_message(max_bytes))?;
        let remaining = max_bytes - open_bytes;

        let mut root_policies = BTreeMap::new();
        let mut inferred_roots = Vec::new();
        let mut inferred_modules = HashSet::new();
        for source_path in &source_paths {
            if let Some(module) = model.module_for_source(source_path) {
                if let Some(id) = &module.id {
                    inferred_modules.insert(id.clone());
                }
                for root in &module.source_roots {
                    let broad_root = root.path == model.root || root.path == module.base_directory;
                    root_policies
                        .entry(root.path.clone())
                        .and_modify(|broad| *broad |= broad_root)
                        .or_insert(broad_root);
                    if !inferred_roots.contains(&root.path) {
                        inferred_roots.push(root.path.clone());
                    }
                }
                for dependency in module
                    .depends_on
                    .iter()
                    .filter_map(|dependency| model.module(dependency))
                {
                    for root in &dependency.source_roots {
                        let broad_root =
                            root.path == model.root || root.path == dependency.base_directory;
                        root_policies
                            .entry(root.path.clone())
                            .and_modify(|broad| *broad |= broad_root)
                            .or_insert(broad_root);
                    }
                }
            }
        }
        let roots = root_policies.keys().cloned().collect::<Vec<_>>();
        inferred_roots.sort();

        let mut excluded_paths = excluded_paths.into_iter().collect::<Vec<_>>();
        excluded_paths.sort();
        let cache_matches = self.cache.as_ref().is_some_and(|cache| {
            cache.roots == roots
                && cache.inferred_roots == inferred_roots
                && cache.excluded_paths == excluded_paths
                && cache.max_bytes == max_bytes
        });
        if cache_matches {
            let cache = self.cache.as_ref().unwrap();
            if cache.kotlin_bytes > remaining {
                return Err(size_limit_message(max_bytes));
            }
            let java_sources =
                sources_within_budget(&cache.java_documents, remaining - cache.kotlin_bytes);
            return Ok((&cache.documents, cache.inferred_count, java_sources));
        }

        let mut remaining_entries = MAX_INVENTORY_ENTRIES;
        let mut paths = Vec::new();
        for (root, ignore_workspace_directories) in &root_policies {
            paths.extend(find_sources(
                root,
                *ignore_workspace_directories,
                &mut remaining_entries,
            )?);
        }
        paths.retain(|path| excluded_paths.binary_search(path).is_err());
        paths.sort();
        paths.dedup();
        let (kotlin_paths, java_paths): (Vec<_>, Vec<_>) = paths
            .into_iter()
            .partition(|path| krusty::source::is_supported_path(path));
        let (mut inferred_paths, dependency_paths): (Vec<_>, Vec<_>) =
            kotlin_paths.into_iter().partition(|path| {
                model
                    .module_for_source(path)
                    .and_then(|module| module.id.as_ref())
                    .is_some_and(|id| inferred_modules.contains(id))
            });
        let inferred_count = inferred_paths.len();
        inferred_paths.extend(dependency_paths);

        let (documents, kotlin_bytes) = load_documents(inferred_paths, remaining, max_bytes)?;
        let java_documents = load_documents_best_effort(java_paths, max_bytes - kotlin_bytes);
        self.cache = Some(Cache {
            roots,
            inferred_roots,
            excluded_paths,
            documents,
            java_documents,
            inferred_count,
            kotlin_bytes,
            max_bytes,
        });
        let cache = self.cache.as_ref().unwrap();
        let java_sources =
            sources_within_budget(&cache.java_documents, remaining - cache.kotlin_bytes);
        Ok((&cache.documents, cache.inferred_count, java_sources))
    }
}

fn load_documents(
    paths: Vec<PathBuf>,
    budget: usize,
    max_bytes: usize,
) -> Result<(Vec<(String, String)>, usize), String> {
    let mut remaining = budget;
    let mut inventory = Vec::new();
    for path in paths {
        let metadata = fs::metadata(&path).map_err(|_| read_error_message())?;
        let Ok(bytes) = usize::try_from(metadata.len()) else {
            return Err(size_limit_message(max_bytes));
        };
        if bytes > remaining {
            return Err(size_limit_message(max_bytes));
        }
        remaining -= bytes;
        inventory.push(path);
    }

    let mut bytes = 0usize;
    let mut documents = Vec::with_capacity(inventory.len());
    for path in inventory {
        let source = fs::read_to_string(&path).map_err(|_| read_error_message())?;
        let Some(next_bytes) = bytes.checked_add(source.len()) else {
            return Err(size_limit_message(max_bytes));
        };
        if next_bytes > budget {
            return Err(size_limit_message(max_bytes));
        }
        bytes = next_bytes;
        let uri = url::Url::from_file_path(path).map_err(|_| read_error_message())?;
        documents.push((uri.into(), source));
    }
    Ok((documents, bytes))
}

fn load_documents_best_effort(paths: Vec<PathBuf>, mut remaining: usize) -> Vec<(String, String)> {
    let mut documents = Vec::new();
    for path in paths {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let Ok(len) = usize::try_from(metadata.len()) else {
            continue;
        };
        if len > remaining {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        if source.len() > remaining {
            continue;
        }
        let Ok(uri) = url::Url::from_file_path(&path) else {
            continue;
        };
        remaining -= source.len();
        documents.push((uri.into(), source));
    }
    documents
}

fn sources_within_budget(documents: &[(String, String)], mut remaining: usize) -> Vec<String> {
    let mut sources = Vec::new();
    for (_, source) in documents {
        if source.len() <= remaining {
            remaining -= source.len();
            sources.push(source.clone());
        }
    }
    sources
}

fn size_limit_message(max_bytes: usize) -> String {
    format!(
        "module source set exceeds analysis limit (maximum {} MiB); semantic diagnostics suppressed",
        max_bytes / (1024 * 1024)
    )
}

fn inventory_limit_message() -> String {
    format!(
        "module source inventory exceeds analysis limit (maximum {MAX_INVENTORY_ENTRIES} entries); semantic diagnostics suppressed"
    )
}

fn read_error_message() -> String {
    "module source set contains an unreadable source; semantic diagnostics suppressed".to_string()
}

fn find_sources(
    root: &Path,
    ignore_workspace_directories: bool,
    remaining_entries: &mut usize,
) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(read_error_message()),
        };
        for entry in entries {
            let entry = entry.map_err(|_| read_error_message())?;
            let Some(next_remaining) = remaining_entries.checked_sub(1) else {
                return Err(inventory_limit_message());
            };
            *remaining_entries = next_remaining;
            let path = entry.path();
            let kind = entry.file_type().map_err(|_| read_error_message())?;
            if kind.is_dir()
                && (!ignore_workspace_directories || !super::walk::is_ignored_directory(&path))
            {
                pending.push(path);
            } else if kind.is_file()
                && (krusty::source::is_supported_path(&path)
                    || path.extension().and_then(|extension| extension.to_str()) == Some("java"))
            {
                sources.push(path);
            }
        }
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::project::{Module, ModuleId, ProjectModel, ProviderKind, SourceRoot};

    const MAX_BYTES: usize = 32 * 1024 * 1024;
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "krusty-lsp-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    fn file_uri(path: &Path) -> String {
        url::Url::from_file_path(path).unwrap().into()
    }

    #[test]
    fn load_collects_java_sources_from_module_roots() {
        let directory = temp_path("java-collection");
        fs::create_dir_all(&directory).unwrap();
        let kt = directory.join("Use.kt");
        let java = directory.join("Widget.java");
        fs::write(&kt, "fun f() {}").unwrap();
        fs::write(&java, "package p; public class Widget {}").unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let kt_uri = file_uri(&kt);
        let documents = [(kt_uri.as_str(), "fun f() {}")];
        let open_uris = [kt_uri.as_str()];
        let mut sources = ProjectSources::default();

        let (_loaded, _inferred, java_docs) = sources
            .load(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(java_docs.len(), 1);
        assert!(java_docs[0].contains("class Widget"));
    }

    #[test]
    fn load_tolerates_java_budget_overrun_without_failing_kotlin_load() {
        let directory = temp_path("java-overrun");
        fs::create_dir_all(&directory).unwrap();
        let use_kt = directory.join("Use.kt");
        let support_kt = directory.join("Support.kt");
        let java = directory.join("Big.java");
        fs::write(&use_kt, "fun f() {}").unwrap();
        fs::write(&support_kt, "val s=1").unwrap();
        fs::write(&java, "package p; public class Widget {}").unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let use_uri = file_uri(&use_kt);
        let documents = [(use_uri.as_str(), "fun f() {}")];
        let open_uris = [use_uri.as_str()];
        let mut sources = ProjectSources::default();

        let max_bytes = 18;
        let (loaded, inferred_count, java_docs) = sources
            .load(&model, &documents, &open_uris, max_bytes)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(loaded, [(file_uri(&support_kt), "val s=1".to_string())]);
        assert_eq!(inferred_count, 1);
        assert!(java_docs.is_empty());
    }

    #[test]
    fn cached_java_sources_reappear_when_open_documents_shrink() {
        let directory = temp_path("java-cache-budget");
        fs::create_dir_all(&directory).unwrap();
        let use_kt = directory.join("Use.kt");
        let support_kt = directory.join("Support.kt");
        let java = directory.join("Widget.java");
        let small_java = directory.join("A.java");
        fs::write(&use_kt, "fun use() {}").unwrap();
        fs::write(&support_kt, "val s=1").unwrap();
        fs::write(&java, "package p; public class Widget {}").unwrap();
        fs::write(&small_java, "class A{}").unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let uri = file_uri(&use_kt);
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let long_source = "fun use() = \"123456789012345678\"";
        let documents = [(uri.as_str(), long_source)];
        let (_, _, java_docs) = sources.load(&model, &documents, &open_uris, 64).unwrap();
        assert_eq!(java_docs, ["class A{}"]);

        let documents = [(uri.as_str(), "fun u()=0")];
        let (_, _, java_docs) = sources.load(&model, &documents, &open_uris, 64).unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(java_docs.len(), 2);
    }

    #[test]
    fn oversized_java_file_does_not_hide_other_java_sources() {
        let directory = temp_path("java-partial-budget");
        fs::create_dir_all(&directory).unwrap();
        let use_kt = directory.join("Use.kt");
        fs::write(&use_kt, "fun use() {}").unwrap();
        fs::write(directory.join("Big.java"), "x".repeat(256)).unwrap();
        fs::write(directory.join("Small.java"), "package p; class Small {}").unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), "fun use() {}")];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let (_, _, java_docs) = sources.load(&model, &documents, &open_uris, 128).unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(java_docs.len(), 1);
        assert!(java_docs[0].contains("class Small"));
    }

    #[test]
    fn load_excludes_every_open_document_from_disk_sources() {
        let directory = temp_path("open-source-exclusion");
        fs::create_dir_all(&directory).unwrap();
        let active = directory.join("Active.kt");
        let blocked = directory.join("Blocked.kt");
        let support = directory.join("Support.kt");
        fs::write(&active, "val staleActive = 1").unwrap();
        fs::write(&blocked, "val staleBlocked = 1").unwrap();
        fs::write(&support, "val support = 1").unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let active_uri = file_uri(&active);
        let blocked_uri = file_uri(&blocked);
        let documents = [(active_uri.as_str(), "val active = 1")];
        let open_uris = [active_uri.as_str(), blocked_uri.as_str()];
        let mut sources = ProjectSources::default();

        let (loaded, inferred_count, java_docs) = sources
            .load(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(
            loaded,
            [(file_uri(&support), "val support = 1".to_string())]
        );
        assert_eq!(inferred_count, 1);
        assert!(java_docs.is_empty());
    }

    #[test]
    fn load_partitions_own_and_direct_project_dependency_sources() {
        let directory = temp_path("dependency-source");
        let base_root = directory.join("base/src");
        let consumer_root = directory.join("feature/src");
        let dependency_root = consumer_root.join("foundation");
        fs::create_dir_all(&base_root).unwrap();
        fs::create_dir_all(&dependency_root).unwrap();
        fs::create_dir_all(&consumer_root).unwrap();
        let base = base_root.join("Marker.kt");
        let dependency = dependency_root.join("Bridge.kt");
        let consumer = consumer_root.join("Use.kt");
        let local = consumer_root.join("Local.kt");
        fs::write(&base, "package fixture\ninterface Marker\n").unwrap();
        fs::write(
            &dependency,
            "package fixture\ninterface Bridge { companion object { fun current(): Bridge? = null } }\n",
        )
        .unwrap();
        fs::write(&consumer, "package fixture\nfun use() = Bridge.current()\n").unwrap();
        fs::write(&local, "package fixture\nfun local() = 1\n").unwrap();

        let base_id = ModuleId::new(":base", "main");
        let mut base_module = Module::new(base_id.clone(), directory.join("base"));
        base_module.source_roots = vec![SourceRoot::source(base_root)];
        let dependency_id = ModuleId::new(":foundation", "main");
        let mut dependency_module =
            Module::new(dependency_id.clone(), directory.join("foundation"));
        dependency_module.source_roots = vec![SourceRoot::source(dependency_root)];
        dependency_module.depends_on = vec![base_id];
        let mut consumer_module =
            Module::new(ModuleId::new(":feature", "main"), directory.join("feature"));
        consumer_module.source_roots = vec![SourceRoot::source(consumer_root)];
        consumer_module.depends_on = vec![dependency_id];
        let model = ProjectModel::new(&directory, ProviderKind::Gradle).with_modules(vec![
            base_module,
            dependency_module,
            consumer_module,
        ]);
        let consumer_uri = file_uri(&consumer);
        let documents = [(
            consumer_uri.as_str(),
            "package fixture\nfun use() = Bridge.current()\n",
        )];
        let open_uris = [consumer_uri.as_str()];
        let mut sources = ProjectSources::default();

        let (loaded, inferred_count, java_docs) = sources
            .load(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(
            loaded,
            [
                (
                    file_uri(&local),
                    "package fixture\nfun local() = 1\n".to_string()
                ),
                (
                    file_uri(&dependency),
                    "package fixture\ninterface Bridge { companion object { fun current(): Bridge? = null } }\n"
                        .to_string()
                )
            ]
        );
        assert_eq!(inferred_count, 1);
        assert!(java_docs.is_empty());
    }

    #[test]
    fn oversized_sources_are_rejected_before_they_are_read() {
        let path = temp_path("oversized-support").with_extension("kt");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_BYTES as u64 + 1).unwrap();

        let result = load_documents(vec![path.clone()], MAX_BYTES, MAX_BYTES);

        fs::remove_file(path).ok();
        assert_eq!(result.unwrap_err(), size_limit_message(MAX_BYTES));
    }

    #[test]
    fn inventory_is_entry_bounded() {
        let directory = temp_path("support-inventory");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("First.kt"), "").unwrap();
        fs::write(directory.join("Second.kt"), "").unwrap();
        let mut remaining_entries = 1;

        let result = find_sources(&directory, false, &mut remaining_entries);

        fs::remove_dir_all(directory).ok();
        assert_eq!(result.unwrap_err(), inventory_limit_message());
    }

    #[test]
    fn broad_roots_skip_output_directories() {
        let directory = temp_path("support-output");
        fs::create_dir_all(directory.join("target/cache")).unwrap();
        fs::create_dir_all(directory.join("src")).unwrap();
        for index in 0..8 {
            fs::write(directory.join(format!("target/cache/{index}.kt")), "").unwrap();
        }
        fs::write(directory.join("src/Feature.kt"), "").unwrap();
        let mut remaining_entries = 3;

        let result = find_sources(&directory, true, &mut remaining_entries);

        fs::remove_dir_all(&directory).ok();
        assert_eq!(result.unwrap(), vec![directory.join("src/Feature.kt")]);
    }

    #[test]
    fn explicit_source_roots_keep_output_named_packages() {
        let directory = temp_path("source-package");
        fs::create_dir_all(directory.join("build")).unwrap();
        fs::write(directory.join("build/Feature.kt"), "").unwrap();
        let mut remaining_entries = 2;

        let result = find_sources(&directory, false, &mut remaining_entries);

        fs::remove_dir_all(&directory).ok();
        assert_eq!(result.unwrap(), vec![directory.join("build/Feature.kt")]);
    }

    #[test]
    fn unreadable_source_suppresses_semantic_diagnostics() {
        let path = temp_path("invalid-support").with_extension("kt");
        fs::write(&path, [0xff]).unwrap();

        let result = load_documents(vec![path.clone()], MAX_BYTES, MAX_BYTES);

        fs::remove_file(path).ok();
        assert_eq!(result.unwrap_err(), read_error_message());
    }
}

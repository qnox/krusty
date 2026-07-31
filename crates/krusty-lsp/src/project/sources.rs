use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::model::{SourceModuleGraph, SourceModuleGraphKey};

const MAX_INVENTORY_ENTRIES: usize = 32 * 1024;
const MAX_CACHED_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_CACHED_MODULE_KEYS: usize = 32 * 1024;

pub type LoadedProjectSources<'a> = (&'a [(String, String)], usize, Vec<String>);

#[derive(Default)]
pub struct ProjectSources {
    excluded_paths: Vec<PathBuf>,
    model_key: Option<SourceModuleGraphKey>,
    caches: Vec<Cache>,
}

struct Cache {
    key: CacheKey,
    documents: Vec<(String, String)>,
    java_documents: Vec<(String, String)>,
    inferred_count: usize,
    kotlin_bytes: usize,
}

#[derive(PartialEq, Eq)]
struct CacheKey {
    module_indices: Vec<usize>,
    import_seed: Vec<String>,
    max_bytes: usize,
}

impl Cache {
    fn retained_bytes(&self) -> usize {
        self.kotlin_bytes.saturating_add(
            self.java_documents
                .iter()
                .map(|(_, source)| source.len())
                .sum::<usize>(),
        )
    }

    fn retained_entries(&self) -> usize {
        self.documents
            .len()
            .saturating_add(self.java_documents.len())
    }

    fn retained_module_keys(&self) -> usize {
        self.key.retained_module_keys()
    }
}

impl CacheKey {
    fn retained_module_keys(&self) -> usize {
        1usize
            .saturating_add(self.module_indices.len())
            .saturating_add(self.import_seed.len())
    }
}

impl ProjectSources {
    pub fn invalidate(&mut self) {
        self.excluded_paths.clear();
        self.model_key = None;
        self.caches.clear();
    }

    pub fn load(
        &mut self,
        module_relations: &SourceModuleGraph,
        documents: &[(&str, &str)],
        open_uris: &[&str],
        max_bytes: usize,
    ) -> Result<LoadedProjectSources<'_>, String> {
        let model = module_relations.model();
        if self.model_key.as_ref() != Some(module_relations.cache_key()) {
            self.model_key = Some(module_relations.cache_key().clone());
            self.caches.clear();
        }
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
        let mut inferred_module_indices = HashSet::new();
        let mut visible_module_indices = HashSet::new();
        let mut module_indices = HashSet::new();
        for source_path in &source_paths {
            if let Some(module_index) = model.module_index_for_source(source_path) {
                module_indices.insert(module_index);
                inferred_module_indices.insert(module_index);
                visible_module_indices.insert(module_index);
                let module = &model.modules[module_index];
                for root in &module.source_roots {
                    let broad_root = root.path == model.root || root.path == module.base_directory;
                    root_policies
                        .entry(root.path.clone())
                        .and_modify(|broad| *broad |= broad_root)
                        .or_insert(broad_root);
                }
                let Some(relations) = module_relations.get(module_index) else {
                    continue;
                };
                inferred_module_indices.extend(relations.friends.iter().copied());
                let visible = relations.visible();
                visible_module_indices.extend(visible.iter().copied());
                for dependency in visible.iter().filter_map(|&index| model.modules.get(index)) {
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
        let mut module_indices = module_indices.into_iter().collect::<Vec<_>>();
        module_indices.sort_unstable();
        let mut excluded_source_roots = model
            .modules
            .iter()
            .enumerate()
            .filter(|(index, _)| !visible_module_indices.contains(index))
            .flat_map(|(_, module)| module.source_roots.iter().map(|root| root.path.clone()))
            .filter(|root| !root_policies.contains_key(root))
            .collect::<Vec<_>>();
        excluded_source_roots.sort();
        excluded_source_roots.dedup();
        let import_seed = open_dependency_seed(documents);
        let cache_key = CacheKey {
            module_indices,
            import_seed,
            max_bytes,
        };
        if cache_key.retained_module_keys() > MAX_CACHED_MODULE_KEYS {
            return Err(cache_limit_message());
        }

        let mut excluded_paths = excluded_paths.into_iter().collect::<Vec<_>>();
        excluded_paths.sort();
        if self.excluded_paths != excluded_paths {
            self.excluded_paths = excluded_paths;
            self.caches.clear();
        }
        let cached = self.caches.iter().position(|cache| cache.key == cache_key);
        if let Some(index) = cached {
            let cache = self.caches.remove(index);
            self.caches.push(cache);
            let cache = self.caches.last().unwrap();
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
                &excluded_source_roots,
                &mut remaining_entries,
            )?);
        }
        paths.retain(|path| self.excluded_paths.binary_search(path).is_err());
        paths.retain(|path| {
            model
                .module_index_for_source(path)
                .is_some_and(|index| visible_module_indices.contains(&index))
        });
        paths.sort();
        paths.dedup();
        let mut newest_dependency_source: BTreeMap<usize, std::time::SystemTime> = BTreeMap::new();
        for path in &paths {
            let Some(index) = model
                .module_index_for_source(path)
                .filter(|index| !inferred_module_indices.contains(index))
            else {
                continue;
            };
            if let Ok(modified) = fs::metadata(path).and_then(|meta| meta.modified()) {
                let newest = newest_dependency_source.entry(index).or_insert(modified);
                *newest = (*newest).max(modified);
            }
        }
        for (&index, newest) in &mut newest_dependency_source {
            let Some(module) = model.modules.get(index) else {
                continue;
            };
            for root in &module.source_roots {
                if let Some(modified) = newest_directory_mtime(&root.path) {
                    *newest = (*newest).max(modified);
                }
            }
        }
        let covered_modules = newest_dependency_source
            .iter()
            .filter(|(&index, &newest_source)| {
                model
                    .modules
                    .get(index)
                    .is_some_and(|module| module_build_is_current(module, newest_source))
            })
            .map(|(&index, _)| index)
            .collect::<HashSet<_>>();
        paths.retain(|path| {
            !model
                .module_index_for_source(path)
                .is_some_and(|index| covered_modules.contains(&index))
        });
        let (kotlin_paths, java_paths): (Vec<_>, Vec<_>) = paths
            .into_iter()
            .partition(|path| krusty::source::is_supported_path(path));
        let java_paths = java_paths
            .into_iter()
            .map(|path| {
                let (_, root) = model
                    .module_source_root_for_source(&path)
                    .expect("inventoried source must have an owning source root");
                let relative = path
                    .strip_prefix(&root.path)
                    .expect("owning source root must contain the source");
                let mut logical = root
                    .package_prefix
                    .split('.')
                    .filter(|segment| !segment.is_empty())
                    .collect::<PathBuf>();
                logical.push(relative);
                (path, logical)
            })
            .collect::<Vec<_>>();
        let (mut inferred_paths, dependency_paths): (Vec<_>, Vec<_>) =
            kotlin_paths.into_iter().partition(|path| {
                model
                    .module_index_for_source(path)
                    .is_some_and(|index| inferred_module_indices.contains(&index))
            });
        let inferred_count = inferred_paths.len();
        inferred_paths.extend(dependency_paths);

        let (documents, kotlin_bytes) = load_documents(inferred_paths, remaining, max_bytes)?;
        let java_documents = load_java_documents_by_import_closure(
            java_paths,
            &cache_key.import_seed,
            max_bytes - kotlin_bytes,
        );
        let cache = Cache {
            key: cache_key,
            documents,
            java_documents,
            inferred_count,
            kotlin_bytes,
        };
        if !retain_cache_budget(
            &mut self.caches,
            cache.retained_bytes(),
            cache.retained_entries(),
            cache.retained_module_keys(),
            MAX_CACHED_SOURCE_BYTES,
            MAX_INVENTORY_ENTRIES,
            MAX_CACHED_MODULE_KEYS,
        ) {
            return Err(cache_limit_message());
        }
        self.caches.push(cache);
        let cache = self.caches.last().unwrap();
        let java_sources =
            sources_within_budget(&cache.java_documents, remaining - cache.kotlin_bytes);
        Ok((&cache.documents, cache.inferred_count, java_sources))
    }

    #[cfg(test)]
    fn load_model(
        &mut self,
        model: &super::model::ProjectModel,
        documents: &[(&str, &str)],
        open_uris: &[&str],
        max_bytes: usize,
    ) -> Result<LoadedProjectSources<'_>, String> {
        let snapshot = model.clone().into_source_module_graph();
        self.load(&snapshot, documents, open_uris, max_bytes)
    }
}

fn module_build_is_current(
    module: &crate::project::Module,
    newest_source: std::time::SystemTime,
) -> bool {
    let mut class_outputs = 0usize;
    for output in &module.outputs {
        let crate::project::model::ModuleOutput::Classes(path) = output else {
            continue;
        };
        class_outputs += 1;
        match oldest_class_mtime(path) {
            Some(oldest_output) if oldest_output >= newest_source => {}
            _ => return false,
        }
    }
    class_outputs > 0
}

fn oldest_class_mtime(path: &Path) -> Option<std::time::SystemTime> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.is_file() {
        return if path
            .extension()
            .is_some_and(|extension| extension == "class")
        {
            metadata.modified().ok()
        } else {
            None
        };
    }
    let mut oldest = None;
    for entry in fs::read_dir(path).ok()?.flatten() {
        if let Some(candidate) = oldest_class_mtime(&entry.path()) {
            oldest = Some(oldest.map_or(candidate, |current: std::time::SystemTime| {
                current.min(candidate)
            }));
        }
    }
    oldest
}

fn newest_directory_mtime(path: &Path) -> Option<std::time::SystemTime> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    let mut newest = metadata.modified().ok();
    for entry in fs::read_dir(path).ok()?.flatten() {
        if let Some(candidate) = newest_directory_mtime(&entry.path()) {
            newest = Some(newest.map_or(candidate, |current| current.max(candidate)));
        }
    }
    newest
}

fn retain_cache_budget(
    caches: &mut Vec<Cache>,
    incoming_bytes: usize,
    incoming_entries: usize,
    incoming_module_keys: usize,
    max_bytes: usize,
    max_entries: usize,
    max_module_keys: usize,
) -> bool {
    if incoming_bytes > max_bytes
        || incoming_entries > max_entries
        || incoming_module_keys > max_module_keys
    {
        return false;
    }
    let mut retained_bytes = caches.iter().map(Cache::retained_bytes).sum::<usize>();
    let mut retained_entries = caches.iter().map(Cache::retained_entries).sum::<usize>();
    let mut retained_module_keys = caches
        .iter()
        .map(Cache::retained_module_keys)
        .sum::<usize>();
    while !caches.is_empty()
        && (incoming_bytes > max_bytes.saturating_sub(retained_bytes)
            || incoming_entries > max_entries.saturating_sub(retained_entries)
            || incoming_module_keys > max_module_keys.saturating_sub(retained_module_keys))
    {
        let evicted = caches.remove(0);
        retained_bytes = retained_bytes.saturating_sub(evicted.retained_bytes());
        retained_entries = retained_entries.saturating_sub(evicted.retained_entries());
        retained_module_keys = retained_module_keys.saturating_sub(evicted.retained_module_keys());
    }
    true
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

#[derive(Default)]
struct ImportTargets {
    files: Vec<Vec<String>>,
    directories: Vec<Vec<String>>,
}

fn open_dependency_seed(documents: &[(&str, &str)]) -> Vec<String> {
    let mut seed = Vec::new();
    for (uri, source) in documents {
        let kind = url::Url::parse(uri)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .and_then(|path| {
                if path
                    .extension()
                    .is_some_and(|extension| extension == "java")
                {
                    Some(krusty::source::SourceKind::Java)
                } else {
                    krusty::source::kind(&path)
                }
            })
            .unwrap_or(krusty::source::SourceKind::Kotlin);
        seed.extend(krusty::source::dependency_candidates(kind, source));
    }
    seed.sort();
    seed.dedup();
    seed
}

impl ImportTargets {
    fn new() -> Self {
        Self::default()
    }

    fn add_import(&mut self, dotted: &str) {
        let segments: Vec<&str> = dotted.split('.').collect();
        let Some((last, package)) = segments.split_last() else {
            return;
        };
        if *last == "*" {
            self.directories
                .push(package.iter().map(|s| s.to_string()).collect());
        } else {
            self.add_type(dotted);
        }
    }

    fn add_type(&mut self, dotted: &str) {
        let segments: Vec<&str> = dotted.split('.').collect();
        for end in (2..=segments.len()).rev() {
            let mut file = segments[..end - 1]
                .iter()
                .map(|segment| segment.to_string())
                .collect::<Vec<_>>();
            file.push(format!("{}.java", segments[end - 1]));
            self.files.push(file);
        }
    }

    fn is_empty(&self) -> bool {
        self.files.is_empty() && self.directories.is_empty()
    }
}

fn add_java_dependencies(source: &str, targets: &mut ImportTargets) {
    for dependency in
        krusty::source::dependency_candidates(krusty::source::SourceKind::Java, source)
    {
        targets.add_import(&dependency);
    }
}

fn path_ends_with(path: &Path, suffix: &[String]) -> bool {
    !suffix.is_empty()
        && path
            .components()
            .rev()
            .zip(suffix.iter().rev())
            .take(suffix.len())
            .all(|(component, expected)| component.as_os_str() == expected.as_str())
        && path.components().count() >= suffix.len()
}

fn take_targeted_paths(
    pending: &mut BTreeSet<PathBuf>,
    by_name: &HashMap<std::ffi::OsString, Vec<PathBuf>>,
    logical: &HashMap<PathBuf, PathBuf>,
    targets: &ImportTargets,
) -> Vec<PathBuf> {
    let mut wave = BTreeSet::new();
    for suffix in &targets.files {
        let Some(name) = suffix.last() else {
            continue;
        };
        if let Some(candidates) = by_name.get(std::ffi::OsStr::new(name)) {
            for path in candidates {
                if pending.contains(path)
                    && logical
                        .get(path)
                        .is_some_and(|path| path_ends_with(path, suffix))
                {
                    wave.insert(path.clone());
                }
            }
        }
    }
    if !targets.directories.is_empty() {
        wave.extend(
            pending
                .iter()
                .filter(|path| {
                    logical.get(*path).is_some_and(|path| {
                        path.parent().is_some_and(|directory| {
                            targets
                                .directories
                                .iter()
                                .any(|suffix| path_ends_with(directory, suffix))
                        })
                    })
                })
                .cloned(),
        );
    }
    for path in &wave {
        pending.remove(path);
    }
    wave.into_iter().collect()
}

fn load_java_documents_by_import_closure(
    paths: Vec<(PathBuf, PathBuf)>,
    import_seed: &[String],
    budget: usize,
) -> Vec<(String, String)> {
    let mut remaining = budget;
    let mut loaded: Vec<(String, String)> = Vec::new();
    let mut by_name: HashMap<std::ffi::OsString, Vec<PathBuf>> = HashMap::new();
    for (path, _) in &paths {
        if let Some(name) = path.file_name() {
            by_name
                .entry(name.to_owned())
                .or_default()
                .push(path.clone());
        }
    }
    let logical = paths.iter().cloned().collect::<HashMap<_, _>>();
    let mut pending = paths
        .into_iter()
        .map(|(path, _)| path)
        .collect::<BTreeSet<_>>();
    let mut seen_imports: HashSet<String> = import_seed.iter().cloned().collect();
    let mut targets = ImportTargets::new();
    for dotted in import_seed {
        targets.add_import(dotted);
    }
    loop {
        if targets.is_empty() {
            break;
        }
        let wave = take_targeted_paths(&mut pending, &by_name, &logical, &targets);
        if wave.is_empty() {
            break;
        }
        targets = ImportTargets::new();
        let mut progressed = false;
        for path in wave {
            let Some((uri, source)) = read_document_within(&path, &mut remaining) else {
                continue;
            };
            let mut dependencies = ImportTargets::new();
            add_java_dependencies(&source, &mut dependencies);
            for file in dependencies.files {
                let key = file.join(".");
                if seen_imports.insert(key) {
                    targets.files.push(file);
                }
            }
            for directory in dependencies.directories {
                let key = directory.join(".");
                if seen_imports.insert(format!("{key}.*")) {
                    targets.directories.push(directory);
                }
            }
            loaded.push((uri, source));
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
    loaded.extend(load_documents_best_effort(
        pending.into_iter().collect(),
        remaining,
    ));
    loaded
}

fn read_document_within(path: &Path, remaining: &mut usize) -> Option<(String, String)> {
    let metadata = fs::metadata(path).ok()?;
    let len = usize::try_from(metadata.len()).ok()?;
    if len > *remaining {
        return None;
    }
    let source = fs::read_to_string(path).ok()?;
    if source.len() > *remaining {
        return None;
    }
    let uri = url::Url::from_file_path(path).ok()?;
    *remaining -= source.len();
    Some((uri.into(), source))
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

fn cache_limit_message() -> String {
    "module source cache metadata exceeds analysis limit; semantic diagnostics suppressed"
        .to_string()
}

/// Every Kotlin source the project model knows about, for background indexing.
///
/// Reuses `find_sources`, so the ignore rules and entry budget that govern open-document source
/// discovery govern the sweep too; a second walk here would be a second, divergent definition of
/// what counts as a workspace source.
pub fn workspace_sources(model: &super::model::ProjectModel) -> (Vec<PathBuf>, bool) {
    let mut roots: Vec<PathBuf> = model
        .modules
        .iter()
        .flat_map(|module| module.source_roots.iter().map(|root| root.path.clone()))
        .collect();
    roots.sort();
    roots.dedup();
    let all_roots = roots.clone();
    let mut sources = Vec::new();
    let mut truncated = false;
    'roots: for root in &roots {
        // A root nested inside another would otherwise be walked twice and charged twice.
        let excluded: Vec<PathBuf> = all_roots
            .iter()
            .filter(|other| *other != root && other.starts_with(root))
            .cloned()
            .collect();
        // Budget per root rather than shared across the workspace: one large module must not
        // starve every module after it in iteration order, silently and with no report.
        let mut remaining = MAX_INVENTORY_ENTRIES;
        let mut found = Vec::new();
        if find_sources_into(root, true, &excluded, &mut remaining, &mut found).is_err() {
            // Keep the bounded prefix. Dropping it would make a large root contribute no files at
            // all, even though the caller explicitly asked for best-effort workspace coverage.
            truncated = true;
        }
        for path in found
            .into_iter()
            .filter(|path| krusty::source::is_supported_path(path))
        {
            if sources.len() >= crate::MAX_WORKSPACE_INDEX_FILES {
                truncated = true;
                break 'roots;
            }
            sources.push(path);
        }
    }
    sources.sort();
    sources.dedup();
    (sources, truncated)
}

fn find_sources(
    root: &Path,
    ignore_workspace_directories: bool,
    excluded_source_roots: &[PathBuf],
    remaining_entries: &mut usize,
) -> Result<Vec<PathBuf>, String> {
    let mut sources = Vec::new();
    find_sources_into(
        root,
        ignore_workspace_directories,
        excluded_source_roots,
        remaining_entries,
        &mut sources,
    )?;
    Ok(sources)
}

/// Shared walker for strict open-document discovery and best-effort workspace inventory.
///
/// The strict wrapper above discards this buffer on error. Workspace indexing deliberately keeps
/// it, so both paths share traversal and ignore rules without sharing incompatible truncation
/// semantics.
fn find_sources_into(
    root: &Path,
    ignore_workspace_directories: bool,
    excluded_source_roots: &[PathBuf],
    remaining_entries: &mut usize,
    sources: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(read_error_message()),
        };
        for entry in entries {
            let entry = entry.map_err(|_| read_error_message())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|_| read_error_message())?;
            if kind.is_dir() && excluded_source_roots.binary_search(&path).is_ok() {
                continue;
            }
            let Some(next_remaining) = remaining_entries.checked_sub(1) else {
                return Err(inventory_limit_message());
            };
            *remaining_entries = next_remaining;
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::project::model::ModuleOutput;
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

    fn dependency_fixture(label: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, ProjectModel) {
        let directory = temp_path(label);
        let app = directory.join("app");
        let lib = directory.join("lib");
        let lib_classes = directory.join("lib-build").join("classes");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let use_kt = app.join("Use.kt");
        let lib_kt = lib.join("Lib.kt");
        fs::write(&use_kt, "fun use() {}").unwrap();
        fs::write(&lib_kt, "fun libFun() {}").unwrap();
        let mut app_module = Module::new(ModuleId::new(":app", "main"), app.clone());
        app_module.source_roots = vec![SourceRoot::source(app.clone())];
        app_module.depends_on = vec![ModuleId::new(":lib", "main")];
        let mut lib_module = Module::new(ModuleId::new(":lib", "main"), lib.clone());
        lib_module.source_roots = vec![SourceRoot::source(lib.clone())];
        lib_module.outputs = vec![ModuleOutput::classes(lib_classes.clone())];
        let model = ProjectModel::new(directory.clone(), ProviderKind::None)
            .with_modules(vec![app_module, lib_module]);
        (directory, use_kt, lib_kt, lib_classes, model)
    }

    fn set_mtime(path: &Path, when: std::time::SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    #[test]
    fn built_dependency_modules_are_not_inlined_as_source() {
        let (directory, use_kt, _lib_kt, lib_classes, model) =
            dependency_fixture("dep-built-classpath");
        fs::create_dir_all(&lib_classes).unwrap();
        fs::write(lib_classes.join("LibKt.class"), b"\xca\xfe\xba\xbe").unwrap();
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), "fun use() {}")];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let (loaded, inferred_count, _java) = sources
            .load_model(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();
        let loaded = loaded.to_vec();

        fs::remove_dir_all(directory).ok();
        assert_eq!(inferred_count, 0);
        assert!(
            loaded.is_empty(),
            "a built dependency resolves from its compiled output, not inlined source: {loaded:?}"
        );
    }

    #[test]
    fn stale_dependency_output_falls_back_to_source_inlining() {
        let (directory, use_kt, lib_kt, lib_classes, model) =
            dependency_fixture("dep-stale-fallback");
        fs::create_dir_all(&lib_classes).unwrap();
        let class = lib_classes.join("LibKt.class");
        fs::write(&class, b"\xca\xfe\xba\xbe").unwrap();
        set_mtime(
            &class,
            std::time::SystemTime::now() - std::time::Duration::from_secs(3600),
        );
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), "fun use() {}")];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let (loaded, _inferred, _java) = sources
            .load_model(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();
        let loaded = loaded.to_vec();

        fs::remove_dir_all(directory).ok();
        assert_eq!(
            loaded,
            [(file_uri(&lib_kt), "fun libFun() {}".to_string())],
            "a stale build must not shadow newer dependency source"
        );
    }

    #[test]
    fn one_stale_class_keeps_dependency_source_inlining() {
        let (directory, use_kt, lib_kt, lib_classes, model) =
            dependency_fixture("dep-partially-stale");
        fs::create_dir_all(&lib_classes).unwrap();
        let stale = lib_classes.join("Old.class");
        let current = lib_classes.join("Current.class");
        fs::write(&stale, b"\xca\xfe\xba\xbe").unwrap();
        fs::write(&current, b"\xca\xfe\xba\xbe").unwrap();
        set_mtime(
            &stale,
            std::time::SystemTime::now() - std::time::Duration::from_secs(3600),
        );
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), "fun use() {}")];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let (loaded, _, _) = sources
            .load_model(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();
        let loaded = loaded.to_vec();

        fs::remove_dir_all(directory).ok();
        assert_eq!(loaded, [(file_uri(&lib_kt), "fun libFun() {}".to_string())]);
    }

    #[test]
    fn unbuilt_dependency_modules_keep_source_inlining() {
        let (directory, use_kt, lib_kt, _lib_classes, model) =
            dependency_fixture("dep-unbuilt-fallback");
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), "fun use() {}")];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let (loaded, _inferred, _java) = sources
            .load_model(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();
        let loaded = loaded.to_vec();

        fs::remove_dir_all(directory).ok();
        assert_eq!(loaded, [(file_uri(&lib_kt), "fun libFun() {}".to_string())]);
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
            .load_model(&model, &documents, &open_uris, MAX_BYTES)
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
            .load_model(&model, &documents, &open_uris, max_bytes)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(loaded, [(file_uri(&support_kt), "val s=1".to_string())]);
        assert_eq!(inferred_count, 1);
        assert!(java_docs.is_empty());
    }

    #[test]
    fn imported_java_sources_win_the_budget_over_alphabetical_order() {
        let directory = temp_path("java-import-priority");
        let package_dir = directory.join("p").join("q");
        fs::create_dir_all(&package_dir).unwrap();
        let use_kt = directory.join("Use.kt");
        let open_text = "import p.q.Widget\nimport p.q.other.*\nfun f() {}";
        fs::write(&use_kt, open_text).unwrap();
        fs::write(
            package_dir.join("Aaa.java"),
            "package p.q; public class Aaa {}",
        )
        .unwrap();
        fs::write(
            package_dir.join("Widget.java"),
            "package p.q; public class Widget {}",
        )
        .unwrap();
        let star_dir = package_dir.join("other");
        fs::create_dir_all(&star_dir).unwrap();
        fs::write(
            star_dir.join("Star.java"),
            "package p.q.other; public class Star {}",
        )
        .unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), open_text)];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let imported_bytes = "package p.q; public class Widget {}".len()
            + "package p.q.other; public class Star {}".len();
        let max_bytes = open_text.len() + imported_bytes;
        let (_, _, java_docs) = sources
            .load_model(&model, &documents, &open_uris, max_bytes)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert!(java_docs.iter().any(|s| s.contains("class Widget")));
        assert!(java_docs.iter().any(|s| s.contains("class Star")));
        assert!(!java_docs.iter().any(|s| s.contains("class Aaa")));
    }

    #[test]
    fn java_budget_prioritizes_open_source_type_references() {
        let directory = temp_path("java-reference-priority");
        let package_dir = directory.join("p");
        let qualified_dir = directory.join("q");
        fs::create_dir_all(&package_dir).unwrap();
        fs::create_dir_all(&qualified_dir).unwrap();
        let use_kt = package_dir.join("Use.kt");
        let open_text =
            "package p\n// q.Ignored is not code\nfun use(widget: Widget): q.Base = q.Base()";
        fs::write(&use_kt, open_text).unwrap();
        fs::write(
            package_dir.join("Aaa.java"),
            "package p; public class Aaa {}",
        )
        .unwrap();
        let widget = "package p; public class Widget {}";
        let base = "package q; public class Base {}";
        fs::write(package_dir.join("Widget.java"), widget).unwrap();
        fs::write(qualified_dir.join("Base.java"), base).unwrap();

        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), open_text)];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();
        let (_, _, java_docs) = sources
            .load_model(
                &model,
                &documents,
                &open_uris,
                open_text.len() + widget.len() + base.len(),
            )
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert!(java_docs.iter().any(|s| s.contains("class Widget")));
        assert!(java_docs.iter().any(|s| s.contains("class Base")));
        assert!(!java_docs.iter().any(|s| s.contains("class Aaa")));
    }

    #[test]
    fn imported_java_sources_use_their_owning_root_package_prefix() {
        let directory = temp_path("java-package-prefix");
        let prefixed_root = directory.join("src");
        let nested_root = prefixed_root.join("plain");
        let package_dir = prefixed_root.join("p");
        fs::create_dir_all(&package_dir).unwrap();
        fs::create_dir_all(nested_root.join("p")).unwrap();
        let use_kt = directory.join("Use.kt");
        let open_text = "import com.acme.p.Widget\nimport p.Plain\nfun f() {}";
        fs::write(&use_kt, open_text).unwrap();
        fs::write(prefixed_root.join("Aaa.java"), "public class Aaa {}").unwrap();
        let widget = "package com.acme.p; public class Widget {}";
        let plain = "package p; public class Plain {}";
        fs::write(package_dir.join("Widget.java"), widget).unwrap();
        fs::write(nested_root.join("p").join("Plain.java"), plain).unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![
            SourceRoot::source(directory.clone()),
            SourceRoot::source(prefixed_root.clone()).with_package_prefix("com.acme"),
            SourceRoot::source(nested_root),
        ];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), open_text)];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let max_bytes = open_text.len() + widget.len() + plain.len();
        let (_, _, java_docs) = sources
            .load_model(&model, &documents, &open_uris, max_bytes)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert!(java_docs.iter().any(|s| s.contains("class Widget")));
        assert!(java_docs.iter().any(|s| s.contains("class Plain")));
        assert!(!java_docs.iter().any(|s| s.contains("class Aaa")));
    }

    #[test]
    fn java_budget_follows_transitive_imports_and_supertypes() {
        let directory = temp_path("java-transitive");
        let package_dir = directory.join("p").join("q");
        let base_dir = directory.join("r");
        fs::create_dir_all(&package_dir).unwrap();
        fs::create_dir_all(&base_dir).unwrap();
        let use_kt = directory.join("Use.kt");
        let open_text = "import p.q.Widget\nfun f() {}";
        fs::write(&use_kt, open_text).unwrap();
        let widget = "package p.q;\nimport r.Base;\npublic class Widget extends WidgetBase {}";
        let widget_base = "package p.q; public class WidgetBase {}";
        let base = "package r; public class Base {}";
        fs::write(directory.join("Aaa.java"), "package s; public class Aaa {}").unwrap();
        fs::write(package_dir.join("Widget.java"), widget).unwrap();
        fs::write(package_dir.join("WidgetBase.java"), widget_base).unwrap();
        fs::write(base_dir.join("Base.java"), base).unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let uri = file_uri(&use_kt);
        let documents = [(uri.as_str(), open_text)];
        let open_uris = [uri.as_str()];
        let mut sources = ProjectSources::default();

        let max_bytes = open_text.len() + widget.len() + widget_base.len() + base.len();
        let (_, _, java_docs) = sources
            .load_model(&model, &documents, &open_uris, max_bytes)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert!(java_docs.iter().any(|s| s.contains("class Widget ")));
        assert!(java_docs.iter().any(|s| s.contains("class WidgetBase")));
        assert!(java_docs.iter().any(|s| s.contains("class Base")));
        assert!(!java_docs.iter().any(|s| s.contains("class Aaa")));
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
        let (_, _, java_docs) = sources
            .load_model(&model, &documents, &open_uris, 64)
            .unwrap();
        assert_eq!(java_docs, ["class A{}"]);

        let documents = [(uri.as_str(), "fun u()=0")];
        let (_, _, java_docs) = sources
            .load_model(&model, &documents, &open_uris, 64)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(java_docs.len(), 2);
    }

    #[test]
    fn caches_support_sources_for_each_open_module() {
        let directory = temp_path("module-caches");
        let first_root = directory.join("first");
        let second_root = directory.join("second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let first_open = first_root.join("Open.kt");
        let first_support = first_root.join("Support.kt");
        let second_open = second_root.join("Open.kt");
        let second_support = second_root.join("Support.kt");
        fs::write(&first_open, "fun open() {}").unwrap();
        fs::write(&first_support, "fun first() {}").unwrap();
        fs::write(&second_open, "fun open() {}").unwrap();
        fs::write(&second_support, "fun second() {}").unwrap();
        let mut first = Module::new(ModuleId::new(":first", "main"), &first_root);
        first.source_roots = vec![SourceRoot::source(&first_root)];
        let mut second = Module::new(ModuleId::new(":second", "main"), &second_root);
        second.source_roots = vec![SourceRoot::source(&second_root)];
        let model =
            ProjectModel::new(&directory, ProviderKind::Gradle).with_modules(vec![first, second]);
        let first_uri = file_uri(&first_open);
        let second_uri = file_uri(&second_open);
        let open_uris = [first_uri.as_str(), second_uri.as_str()];
        let first_documents = [(first_uri.as_str(), "fun open() {}")];
        let second_documents = [(second_uri.as_str(), "fun open() {}")];
        let mut sources = ProjectSources::default();

        let first_loaded = sources
            .load_model(&model, &first_documents, &open_uris, MAX_BYTES)
            .unwrap()
            .0
            .to_vec();
        let second_loaded = sources
            .load_model(&model, &second_documents, &open_uris, MAX_BYTES)
            .unwrap()
            .0
            .to_vec();
        assert_eq!(sources.caches.len(), 2);
        let first_only = [first_uri.as_str()];
        sources
            .load_model(&model, &first_documents, &first_only, MAX_BYTES)
            .unwrap();
        assert_eq!(sources.caches.len(), 1);
        fs::remove_file(&first_support).unwrap();
        let first_cached = sources
            .load_model(&model, &first_documents, &first_only, MAX_BYTES)
            .unwrap()
            .0
            .to_vec();

        fs::remove_dir_all(directory).ok();
        assert_eq!(
            first_loaded,
            [(file_uri(&first_support), "fun first() {}".to_string())]
        );
        assert_eq!(
            second_loaded,
            [(file_uri(&second_support), "fun second() {}".to_string())]
        );
        assert_eq!(first_cached, first_loaded);
    }

    #[test]
    fn cache_key_tracks_model_relation_changes() {
        let directory = temp_path("relation-cache");
        let consumer_root = directory.join("consumer");
        let dependency_root = directory.join("dependency");
        fs::create_dir_all(&consumer_root).unwrap();
        fs::create_dir_all(&dependency_root).unwrap();
        let open = consumer_root.join("Open.kt");
        let support = dependency_root.join("Support.kt");
        fs::write(&open, "fun open() {}").unwrap();
        fs::write(&support, "fun support() {}").unwrap();

        let dependency_id = ModuleId::new(":dependency", "main");
        let model = |has_dependency| {
            let mut consumer = Module::new(ModuleId::new(":consumer", "main"), &consumer_root);
            consumer.source_roots = vec![SourceRoot::source(&consumer_root)];
            if has_dependency {
                consumer.depends_on = vec![dependency_id.clone()];
            }
            let mut dependency = Module::new(dependency_id.clone(), &dependency_root);
            dependency.source_roots = vec![SourceRoot::source(&dependency_root)];
            ProjectModel::new(&directory, ProviderKind::Gradle)
                .with_modules(vec![consumer, dependency])
        };
        let open_uri = file_uri(&open);
        let documents = [(open_uri.as_str(), "fun open() {}")];
        let open_uris = [open_uri.as_str()];
        let mut sources = ProjectSources::default();

        let with_dependency = sources
            .load_model(&model(true), &documents, &open_uris, MAX_BYTES)
            .unwrap()
            .0
            .to_vec();
        let without_dependency = sources
            .load_model(&model(false), &documents, &open_uris, MAX_BYTES)
            .unwrap()
            .0
            .to_vec();

        fs::remove_dir_all(directory).ok();
        assert_eq!(
            with_dependency,
            [(file_uri(&support), "fun support() {}".to_string())]
        );
        assert!(without_dependency.is_empty());
    }

    #[test]
    fn cache_key_tracks_source_root_ownership_changes() {
        let directory = temp_path("root-ownership-cache");
        let consumer_root = directory.join("consumer");
        let first_root = directory.join("first");
        let second_root = directory.join("second");
        fs::create_dir_all(&consumer_root).unwrap();
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let open = consumer_root.join("Open.kt");
        let first = first_root.join("Support.kt");
        let second = second_root.join("Support.kt");
        fs::write(&open, "fun open() {}").unwrap();
        fs::write(&first, "fun first() {}").unwrap();
        fs::write(&second, "fun second() {}").unwrap();

        let friend_output = directory.join("friend-classes");
        let dependency_id = ModuleId::new(":dependency", "main");
        let model = |swap_roots| {
            let mut consumer = Module::new(ModuleId::new(":consumer", "main"), &consumer_root);
            consumer.source_roots = vec![SourceRoot::source(&consumer_root)];
            consumer.depends_on = vec![dependency_id.clone()];
            consumer.friend_paths = vec![friend_output.clone()];

            let mut friend = Module::new(ModuleId::new(":friend", "main"), &directory);
            friend.source_roots = vec![SourceRoot::source(if swap_roots {
                &second_root
            } else {
                &first_root
            })];
            friend.outputs = vec![ModuleOutput::classes(&friend_output)];

            let mut dependency = Module::new(dependency_id.clone(), &directory);
            dependency.source_roots = vec![SourceRoot::source(if swap_roots {
                &first_root
            } else {
                &second_root
            })];
            ProjectModel::new(&directory, ProviderKind::Gradle)
                .with_modules(vec![consumer, friend, dependency])
        };
        let open_uri = file_uri(&open);
        let documents = [(open_uri.as_str(), "fun open() {}")];
        let open_uris = [open_uri.as_str()];
        let mut sources = ProjectSources::default();

        let before = sources
            .load_model(&model(false), &documents, &open_uris, MAX_BYTES)
            .unwrap();
        assert_eq!(before.1, 1);
        assert_eq!(before.0[0].0, file_uri(&first));
        let after = sources
            .load_model(&model(true), &documents, &open_uris, MAX_BYTES)
            .unwrap();
        assert_eq!(after.1, 1);
        assert_eq!(after.0[0].0, file_uri(&second));

        fs::remove_dir_all(directory).ok();
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

        let (_, _, java_docs) = sources
            .load_model(&model, &documents, &open_uris, 128)
            .unwrap();

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
            .load_model(&model, &documents, &open_uris, MAX_BYTES)
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
        let unrelated_root = consumer_root.join("unrelated");
        fs::create_dir_all(&base_root).unwrap();
        fs::create_dir_all(&dependency_root).unwrap();
        fs::create_dir_all(&unrelated_root).unwrap();
        fs::create_dir_all(&consumer_root).unwrap();
        let base = base_root.join("Marker.kt");
        let dependency = dependency_root.join("Bridge.kt");
        let consumer = consumer_root.join("Use.kt");
        let local = consumer_root.join("Local.kt");
        let unrelated = unrelated_root.join("Hidden.kt");
        let unrelated_java = unrelated_root.join("HiddenJava.java");
        fs::write(&base, "package fixture\ninterface Marker\n").unwrap();
        fs::write(
            &dependency,
            "package fixture\ninterface Bridge { companion object { fun current(): Bridge? = null } }\n",
        )
        .unwrap();
        fs::write(&consumer, "package fixture\nfun use() = Bridge.current()\n").unwrap();
        fs::write(&local, "package fixture\nfun local() = 1\n").unwrap();
        fs::write(&unrelated, "package fixture\ninternal class Hidden\n").unwrap();
        fs::write(&unrelated_java, "package fixture; class HiddenJava {}").unwrap();

        let base_id = ModuleId::new(":base", "main");
        let mut base_module = Module::new(base_id.clone(), directory.join("base"));
        base_module.source_roots = vec![SourceRoot::source(base_root)];
        let dependency_id = ModuleId::new(":foundation", "main");
        let mut dependency_module =
            Module::new(dependency_id.clone(), directory.join("foundation"));
        dependency_module.source_roots = vec![SourceRoot::source(dependency_root)];
        dependency_module.depends_on = vec![base_id];
        let mut unrelated_module = Module::new(
            ModuleId::new(":unrelated", "main"),
            directory.join("unrelated"),
        );
        unrelated_module.source_roots = vec![SourceRoot::source(unrelated_root)];
        let mut consumer_module =
            Module::new(ModuleId::new(":feature", "main"), directory.join("feature"));
        consumer_module.source_roots = vec![SourceRoot::source(consumer_root)];
        consumer_module.depends_on = vec![dependency_id];
        let model = ProjectModel::new(&directory, ProviderKind::Gradle).with_modules(vec![
            base_module,
            dependency_module,
            unrelated_module,
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
            .load_model(&model, &documents, &open_uris, MAX_BYTES)
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
    fn load_includes_sources_exposed_by_friend_outputs() {
        let directory = temp_path("friend-source");
        let associated_root = directory.join("associated/src");
        let consumer_root = directory.join("consumer/src");
        fs::create_dir_all(&associated_root).unwrap();
        fs::create_dir_all(&consumer_root).unwrap();
        let associated_source = associated_root.join("Available.kt");
        let consumer_source = consumer_root.join("Open.kt");
        fs::write(&associated_source, "package sample\nfun available() = 1\n").unwrap();
        fs::write(
            &consumer_source,
            "package sample\nfun use() = available()\n",
        )
        .unwrap();
        let output = directory.join("associated/out");
        let mut associated = Module::new(ModuleId::new(":associated", "main"), &associated_root);
        associated.source_roots = vec![SourceRoot::source(&associated_root)];
        associated.outputs = vec![ModuleOutput::classes(&output)];
        let mut consumer = Module::new(ModuleId::new(":consumer", "main"), &consumer_root);
        consumer.source_roots = vec![SourceRoot::source(&consumer_root)];
        consumer.friend_paths = vec![output];
        let model = ProjectModel::new(&directory, ProviderKind::Gradle)
            .with_modules(vec![associated, consumer]);
        let consumer_uri = file_uri(&consumer_source);
        let documents = [(
            consumer_uri.as_str(),
            "package sample\nfun use() = available()\n",
        )];
        let open_uris = [consumer_uri.as_str()];
        let mut sources = ProjectSources::default();

        let (loaded, inferred_count, java_docs) = sources
            .load_model(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(
            loaded,
            [(
                file_uri(&associated_source),
                "package sample\nfun available() = 1\n".to_string()
            )]
        );
        assert_eq!(inferred_count, 1);
        assert!(java_docs.is_empty());
    }

    #[test]
    fn source_cache_eviction_is_byte_entry_and_metadata_bounded() {
        let cache = |module_index: usize, bytes: usize, entries: usize| Cache {
            key: CacheKey {
                module_indices: vec![module_index],
                import_seed: Vec::new(),
                max_bytes: bytes,
            },
            documents: (0..entries)
                .map(|index| {
                    (
                        format!("{module_index}/{index}.kt"),
                        "x".repeat(bytes / entries),
                    )
                })
                .collect(),
            java_documents: Vec::new(),
            inferred_count: entries,
            kotlin_bytes: bytes,
        };
        let mut caches = vec![cache(0, 4, 1), cache(1, 4, 1)];

        assert!(!retain_cache_budget(&mut caches, 4, 1, 5, 8, 2, 4));
        assert_eq!(caches.len(), 2);
        assert!(retain_cache_budget(&mut caches, 4, 1, 2, 8, 2, 4));

        assert_eq!(caches.len(), 1);
        assert_eq!(caches[0].key.module_indices, [1]);
        assert_eq!(caches[0].retained_module_keys(), 2);
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

        let result = find_sources(&directory, false, &[], &mut remaining_entries);

        fs::remove_dir_all(directory).ok();
        assert_eq!(result.unwrap_err(), inventory_limit_message());
    }

    #[test]
    fn workspace_inventory_keeps_the_bounded_prefix_of_a_large_root() {
        let directory = temp_path("workspace-inventory-prefix");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("First.kt"), "").unwrap();
        fs::write(directory.join("Second.kt"), "").unwrap();
        let mut remaining_entries = 1;
        let mut found = Vec::new();

        let result = find_sources_into(&directory, false, &[], &mut remaining_entries, &mut found);

        fs::remove_dir_all(directory).ok();
        assert_eq!(result.unwrap_err(), inventory_limit_message());
        assert_eq!(
            found.len(),
            1,
            "best-effort indexing must retain work discovered before the inventory ceiling"
        );
    }

    #[test]
    fn inventory_prunes_unrelated_nested_source_roots_before_counting() {
        let directory = temp_path("support-pruned-module");
        let unrelated = directory.join("nested");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(directory.join("Active.kt"), "").unwrap();
        for index in 0..8 {
            fs::write(unrelated.join(format!("{index}.kt")), "").unwrap();
        }
        let mut remaining_entries = 1;

        let result = find_sources(
            &directory,
            false,
            std::slice::from_ref(&unrelated),
            &mut remaining_entries,
        );

        fs::remove_dir_all(&directory).ok();
        assert_eq!(result.unwrap(), vec![directory.join("Active.kt")]);
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

        let result = find_sources(&directory, true, &[], &mut remaining_entries);

        fs::remove_dir_all(&directory).ok();
        assert_eq!(result.unwrap(), vec![directory.join("src/Feature.kt")]);
    }

    #[test]
    fn explicit_source_roots_keep_output_named_packages() {
        let directory = temp_path("source-package");
        fs::create_dir_all(directory.join("build")).unwrap();
        fs::write(directory.join("build/Feature.kt"), "").unwrap();
        let mut remaining_entries = 2;

        let result = find_sources(&directory, false, &[], &mut remaining_entries);

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

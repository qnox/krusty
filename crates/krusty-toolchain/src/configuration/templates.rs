//! Reading a module's file and the templates it applies, transitively and breadth first
//! (`readWithTemplates`), and how those files rank (`PathInheritance`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use super::ModuleFiles;
use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::module::ModuleHeader;
use crate::reading::read_document;
use crate::schema::{ObjectType, MODULE, TEMPLATE};
use crate::tree::{self, Contexts, FileId, FileOrder, Files, Node, Paths, Trace, Value};

/// The template paths `tree` applies, with where each is written.
fn applied(tree: &Node) -> Vec<(PathBuf, Trace)> {
    match tree.get("apply").map(|apply| &apply.value) {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|item| match &item.value {
                Value::Path(path) => Some((path.clone(), item.trace.clone())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Read a module's file and the templates it applies. Problems in the module file the header
/// reader already reported are not reported again.
pub(super) fn read(
    root: &Path,
    header: &ModuleHeader,
    diagnostics: &mut Diagnostics,
) -> ModuleFiles {
    let mut files = Files::default();
    let module = files.add(header.file.clone());
    let mut fresh = Diagnostics::default();
    let module_tree = read_tree(root, &header.file, module, &MODULE, &mut fresh);
    for diagnostic in fresh.iter() {
        if !diagnostics.iter().any(|known| known == diagnostic) {
            diagnostics.push(diagnostic.clone());
        }
    }
    let mut trees = vec![module_tree];
    let mut reaches: Vec<(FileId, Vec<FileId>)> = Vec::new();
    let mut edges: Vec<(FileId, FileId)> = Vec::new();
    let mut queue = VecDeque::from([(module, 0)]);
    while let Some((file, index)) = queue.pop_front() {
        let base = files.path(file).parent().unwrap_or(root).to_path_buf();
        for (path, trace) in applied(&trees[index]) {
            let path = if path.is_absolute() {
                path
            } else {
                base.join(path)
            };
            if !path.is_file() {
                let shown = path.strip_prefix(&base).unwrap_or(&path).to_path_buf();
                let position = match trace {
                    Trace::File { position, .. } => Some(position),
                    _ => None,
                };
                diagnostics.push(Diagnostic::error(
                    files.path(file),
                    position,
                    format!("Cannot find template file `{}`", shown.display()),
                ));
                continue;
            }
            let known = files.contains(&path);
            let template = files.add(path.clone());
            edges.push((file, template));
            if !known {
                let tree = read_tree(root, &path, template, &TEMPLATE, diagnostics);
                trees.push(tree);
                queue.push_back((template, trees.len() - 1));
            }
        }
    }
    for from in files.ids() {
        let mut reached = Vec::new();
        let mut stack = vec![from];
        while let Some(file) = stack.pop() {
            for &(source, target) in &edges {
                if source == file && !reached.contains(&target) {
                    reached.push(target);
                    stack.push(target);
                }
            }
        }
        if from != module && !reached.is_empty() {
            reaches.push((from, reached));
        }
    }
    // The module itself reaches every template, which makes each a template.
    let templates: Vec<FileId> = files.ids().filter(|&file| file != module).collect();
    if !templates.is_empty() {
        reaches.push((module, templates));
    }
    ModuleFiles {
        files,
        module,
        trees,
        order: FileOrder {
            root: Some(module),
            reaches,
        },
    }
}

fn read_tree(
    root: &Path,
    path: &Path,
    file: FileId,
    object: &'static ObjectType,
    diagnostics: &mut Diagnostics,
) -> Node {
    let base = path.parent().unwrap_or(root);
    match read_document(path, diagnostics) {
        Some(document) => tree::read(
            path,
            file,
            &document,
            object,
            Paths { root, base },
            diagnostics,
        ),
        None => Node::new(Value::Error, Trace::Default, Contexts::default()),
    }
}

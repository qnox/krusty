//! A module's configuration: its file and the templates it applies, refined into the values the
//! module as a whole and each of its fragments see (`build.kt`, `buildFragments.kt`).

mod conflicts;
mod sections;
mod templates;

use std::path::Path;

use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::module::{ModuleHeader, ProductType};
use crate::schema::{ObjectType, FRAGMENT, MODULE};
use crate::tree::{self, Conflict, Contexts, FileId, FileOrder, Files, Node, Refiner, Value};

/// A module's files, read.
pub struct ModuleFiles {
    pub files: Files,
    module: FileId,
    /// The module's tree, then each template's, in breadth-first `apply` order.
    trees: Vec<Node>,
    order: FileOrder,
}

/// Problems already reported for a module: refinement repeats, and its problems are reported once.
/// A conflict is the same conflict whichever contexts it is seen from.
#[derive(Default)]
struct Reported {
    diagnostics: Vec<Diagnostic>,
    conflicts: Vec<Conflict>,
}

impl Reported {
    fn push(&mut self, diagnostic: Diagnostic, diagnostics: &mut Diagnostics) {
        if !self.diagnostics.contains(&diagnostic) {
            self.diagnostics.push(diagnostic.clone());
            diagnostics.push(diagnostic);
        }
    }

    /// Whether `conflict` is new, remembering it.
    fn conflict(&mut self, conflict: &Conflict) -> bool {
        let new = !self.conflicts.contains(conflict);
        if new {
            self.conflicts.push(conflict.clone());
        }
        new
    }
}

/// A module's configuration: its files, and the settings and dependencies of its JVM fragments.
pub struct Configuration {
    pub files: ModuleFiles,
    /// The module as a whole (its repositories, among others); `None` when it is incomplete.
    pub module: Option<Node>,
    /// The main fragment; `None` when the module or one of its fragments is incomplete (reported).
    pub main: Option<Node>,
    /// The test fragment, whose dependencies follow the main fragment's; `None` like `main`.
    pub test: Option<Node>,
}

/// Configure every module of a project, in its order, as the toolchain builds its model: each
/// module is read and checked as a whole, then each module's fragments are refined.
pub fn configure(
    root: &Path,
    modules: &[ModuleHeader],
    diagnostics: &mut Diagnostics,
) -> Vec<Configuration> {
    let read: Vec<(ModuleFiles, Reported, Option<Node>)> = modules
        .iter()
        .map(|header| {
            let files = templates::read(root, header, diagnostics);
            let mut reported = Reported::default();
            let module = files.check(header.product, &mut reported, diagnostics);
            (files, reported, module)
        })
        .collect();
    read.into_iter()
        .map(|(files, mut reported, module)| {
            let complete = module.is_some();
            let main = complete
                .then(|| files.fragment(false, &mut reported, diagnostics))
                .flatten();
            let test = complete
                .then(|| files.fragment(true, &mut reported, diagnostics))
                .flatten();
            let complete = main.is_some() && test.is_some();
            Configuration {
                module,
                main: main.filter(|_| complete),
                test: test.filter(|_| complete),
                files,
            }
        })
        .collect()
}

impl ModuleFiles {
    /// Refine `trees` (each of type `object`) as seen from `selected`, resolve its references and
    /// complete it.
    fn refine(
        &self,
        trees: &[&Node],
        object: &'static ObjectType,
        selected: Contexts,
        reported: &mut Reported,
        diagnostics: &mut Diagnostics,
    ) -> Option<Node> {
        let refiner = Refiner {
            order: &self.order,
            selected,
        };
        let mut found = Vec::new();
        let mut refined = refiner.refine_roots(trees, object, &mut found);
        for conflict in &found {
            if !reported.conflict(conflict) {
                continue;
            }
            let message = conflicts::message(conflict, selected, &self.files);
            reported.push(Diagnostic::project_error(message), diagnostics);
        }
        tree::resolve(&mut refined);
        let mut missing = Diagnostics::default();
        let complete = tree::complete(&refined, self.files.path(self.module), &mut missing);
        for diagnostic in missing.iter() {
            reported.push(diagnostic.clone(), diagnostics);
        }
        complete
    }

    /// The module as a whole (`readModuleMergedTree`): its values seen from its own file, then
    /// where its settings are written. `None` when it is incomplete, which leaves it without
    /// fragments.
    fn check(
        &self,
        product: ProductType,
        reported: &mut Reported,
        diagnostics: &mut Diagnostics,
    ) -> Option<Node> {
        let trees: Vec<&Node> = self.trees.iter().collect();
        let selected = Contexts {
            file: Some(self.module),
            ..Contexts::default()
        };
        let module = self.refine(&trees, &MODULE, selected, reported, diagnostics)?;
        for tree in &self.trees {
            sections::check(tree, product, &self.files, reported, diagnostics);
        }
        Some(module)
    }

    /// The dependencies and settings the JVM fragment (`test`: the test fragment) sees.
    fn fragment(
        &self,
        test: bool,
        reported: &mut Reported,
        diagnostics: &mut Diagnostics,
    ) -> Option<Node> {
        let roots: Vec<Node> = self.trees.iter().map(fragment_part).collect();
        let roots: Vec<&Node> = roots.iter().collect();
        let selected = Contexts {
            file: Some(self.module),
            test,
            jvm: true,
        };
        self.refine(&roots, &FRAGMENT, selected, reported, diagnostics)
    }
}

/// The part of a module or template tree a fragment is made of: its dependencies and settings.
fn fragment_part(tree: &Node) -> Node {
    match &tree.value {
        Value::Mapping { entries, .. } => Node::new(
            Value::Mapping {
                object: Some(&FRAGMENT),
                entries: entries
                    .iter()
                    .filter(|entry| {
                        entry
                            .property
                            .is_some_and(|property| FRAGMENT.property(property.name).is_some())
                    })
                    .cloned()
                    .collect(),
            },
            tree.trace.clone(),
            tree.contexts,
        ),
        _ => Node::new(
            Value::Mapping {
                object: Some(&FRAGMENT),
                entries: Vec::new(),
            },
            tree.trace.clone(),
            tree.contexts,
        ),
    }
}

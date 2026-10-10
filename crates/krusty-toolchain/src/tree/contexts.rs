//! Contexts: what a value is specific to, and which of two values is more specific
//! (`ContextsInheritance`). A value is specific to the file it is written in (a module beats the
//! templates it applies), to tests (`test-settings`), and to a platform (`@jvm`).

use super::node::FileId;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contexts {
    pub file: Option<FileId>,
    pub test: bool,
    /// Qualified `@jvm`, the only platform qualifier krusty-toolchain reads.
    pub jvm: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Specificity {
    More,
    Less,
    Same,
    Indeterminate,
}

impl Specificity {
    pub fn same_or_more(self) -> bool {
        matches!(self, Specificity::More | Specificity::Same)
    }

    /// Combine two dimensions as the toolchain's `ContextsInheritance.plus` does.
    fn and(self, other: Specificity) -> Specificity {
        match (self, other) {
            (Specificity::Same, other) | (other, Specificity::Same) => other,
            (first, second) if first == second => first,
            _ => Specificity::Indeterminate,
        }
    }
}

fn flag(this: bool, other: bool) -> Specificity {
    match (this, other) {
        (a, b) if a == b => Specificity::Same,
        (true, false) => Specificity::More,
        _ => Specificity::Less,
    }
}

/// How the files of one module relate: which templates each file applies, transitively.
#[derive(Debug, Default)]
pub struct FileOrder {
    /// The module (or template) file being refined, more specific than any template.
    pub root: Option<FileId>,
    /// For each file, every template it reaches through `apply`.
    pub reaches: Vec<(FileId, Vec<FileId>)>,
}

impl FileOrder {
    fn reaches(&self, from: FileId, to: FileId) -> bool {
        self.reaches
            .iter()
            .any(|(file, reached)| *file == from && reached.contains(&to))
    }

    fn is_template(&self, file: FileId) -> bool {
        self.reaches
            .iter()
            .any(|(known, reached)| *known == file || reached.contains(&file))
    }

    /// `PathInheritance`.
    fn compare(&self, this: Option<FileId>, other: Option<FileId>) -> Specificity {
        match (this, other) {
            (a, b) if a == b => Specificity::Same,
            (None, _) => Specificity::Less,
            (_, None) => Specificity::More,
            (Some(this), Some(other)) => {
                if Some(this) == self.root && self.is_template(other) {
                    return Specificity::More;
                }
                if Some(other) == self.root && self.is_template(this) {
                    return Specificity::Less;
                }
                match (self.reaches(this, other), self.reaches(other, this)) {
                    (true, false) => Specificity::More,
                    (false, true) => Specificity::Less,
                    _ => Specificity::Indeterminate,
                }
            }
        }
    }

    /// How `this` compares to `other`: platforms, then files, then tests, as the toolchain chains
    /// its inheritance relations.
    pub fn compare_contexts(&self, this: &Contexts, other: &Contexts) -> Specificity {
        flag(this.jvm, other.jvm)
            .and(self.compare(this.file, other.file))
            .and(flag(this.test, other.test))
    }
}

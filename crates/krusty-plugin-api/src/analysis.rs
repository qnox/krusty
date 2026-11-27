//! What krusty hands an Analysis API host for one session: the source files of its modules and the
//! declarations each one contains, as positions in the file's text.
//!
//! Offsets are kotlinc's: UTF-16 code units into the text kotlinc reads, in which every line break
//! is a single `\n`. A host that maps a `PsiElement` offset to a declaration needs no other
//! conversion.

/// Every source file of the session, in the order the compilation reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisSession {
    pub files: Vec<AnalysisFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisFile {
    /// The path the compilation was given.
    pub path: String,
    /// The file's named declarations in source order. A declaration's container precedes it.
    pub declarations: Vec<AnalysisDeclaration>,
}

/// What a declaration is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisDeclarationKind {
    Class,
    Interface,
    Enum,
    Annotation,
    Object,
    EnumEntry,
    Function,
    Property,
    TypeAlias,
}

/// A half-open range of offsets, `start <= end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisDeclaration {
    pub kind: AnalysisDeclarationKind,
    pub name: String,
    /// Where the declaration's name is written.
    pub name_range: AnalysisRange,
    /// The whole declaration.
    pub range: AnalysisRange,
    /// The index in [`AnalysisFile::declarations`] of the declaration this one is declared in.
    pub parent: Option<u32>,
}

impl AnalysisFile {
    /// The innermost declaration whose range contains `offset`, as the index in
    /// [`Self::declarations`].
    pub fn declaration_at(&self, offset: u32) -> Option<u32> {
        self.declarations
            .iter()
            .enumerate()
            .filter(|(_, declaration)| {
                declaration.range.start <= offset && offset < declaration.range.end
            })
            .min_by_key(|(_, declaration)| declaration.range.end - declaration.range.start)
            .map(|(index, _)| index as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration(start: u32, end: u32, parent: Option<u32>) -> AnalysisDeclaration {
        AnalysisDeclaration {
            kind: AnalysisDeclarationKind::Class,
            name: String::new(),
            name_range: AnalysisRange { start, end: start },
            range: AnalysisRange { start, end },
            parent,
        }
    }

    #[test]
    fn the_innermost_declaration_answers_an_offset() {
        let file = AnalysisFile {
            path: "a.kt".to_string(),
            declarations: vec![declaration(0, 20, None), declaration(5, 10, Some(0))],
        };
        assert_eq!(
            [0, 5, 9, 10, 19, 20].map(|offset| file.declaration_at(offset)),
            [Some(0), Some(1), Some(1), Some(0), Some(0), None]
        );
    }
}

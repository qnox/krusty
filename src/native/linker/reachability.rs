//! Which global definitions a program can reach from its entry point.
//!
//! The runtime is linked whole, so a definition being present says nothing about whether the
//! program can call it. Reachability is followed section by section from the entry symbol's
//! section through each section's relocations; the runtime is compiled with one section per
//! function, so a section stands for one function. A reference by address (a function pointer
//! stored in data) counts as a use, so the answer errs towards reachable.

use std::collections::{HashMap, HashSet};

use object::read::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
use object::SectionIndex;

use super::elf::{symbol_name, Elf};
use super::ProgramLinkError;

/// The names of the global definitions reachable from `entry`, which `files` must define.
pub(super) fn reachable_globals(
    files: &[Elf],
    entry: &str,
) -> Result<HashSet<String>, ProgramLinkError> {
    // Every section defining each global name; a weak and a strong definition are both kept,
    // which can only make more reachable.
    let mut definitions: HashMap<&str, Vec<(usize, SectionIndex)>> = HashMap::new();
    for (file_index, file) in files.iter().enumerate() {
        for symbol in file.symbols() {
            if symbol.is_local() || symbol.is_undefined() {
                continue;
            }
            if let Some(section) = symbol.section_index() {
                definitions
                    .entry(symbol_name(&symbol)?)
                    .or_default()
                    .push((file_index, section));
            }
        }
    }
    let mut queue: Vec<(usize, SectionIndex)> = definitions
        .get(entry)
        .cloned()
        .ok_or_else(|| ProgramLinkError::UndefinedSymbol(entry.to_string()))?;
    let mut seen: HashSet<(usize, SectionIndex)> = queue.iter().copied().collect();
    while let Some((file_index, section_index)) = queue.pop() {
        let file = &files[file_index];
        let section = file
            .section_by_index(section_index)
            .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
        for (_, relocation) in section.relocations() {
            let RelocationTarget::Symbol(target) = relocation.target() else {
                continue;
            };
            let symbol = file
                .symbol_by_index(target)
                .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
            let targets = if symbol.is_local() || !symbol.is_undefined() {
                symbol
                    .section_index()
                    .map(|section| vec![(file_index, section)])
                    .unwrap_or_default()
            } else {
                definitions
                    .get(symbol_name(&symbol)?)
                    .cloned()
                    .unwrap_or_default()
            };
            for target in targets {
                if seen.insert(target) {
                    queue.push(target);
                }
            }
        }
    }
    Ok(definitions
        .into_iter()
        .filter(|(_, sections)| sections.iter().any(|section| seen.contains(section)))
        .map(|(name, _)| name.to_string())
        .collect())
}

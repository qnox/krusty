//! The interface sections every object carries for the link, read into one answer: for each
//! interface stub, every `(type, function)` pair of the program that implements its member (see
//! `native::interface_calls`).
//!
//! A module's own types record a member by its module-wide number, which names the stub directly.
//! A type of a dependent module records a dependency's member by the member's KLIB identity, which
//! the module declaring the member maps to its number; an identity no module maps is one no stub
//! is called through, and is dropped.
//!
//! A property accessor's number has no identity yet. A type implementing a property of another
//! module's interface is recorded only by that module's namespace, and a call through such an
//! accessor's stub is refused (see [`Records::refuse_unidentified_calls`]).

use crate::fx_hash::{HashMap, HashSet};

use object::read::{Object, ObjectSection, ObjectSymbol, RelocationTarget};

use super::super::interface_calls::{
    identified_stub, identities_namespace, padded, recorded_stub,
    DEPENDENCY_IMPLEMENTATIONS_SECTION, DEPENDENCY_PROPERTIES_SECTION, RECORD_SIZE,
};
use super::elf::{symbol_name, Elf};
use super::ProgramLinkError;

/// A symbol a record names: a global by name, a local by its object and index. Two references to
/// one function are one implementation exactly when they are the same `Target`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Target {
    Global(String),
    Local(usize, object::SymbolIndex),
}

/// Each stub's implementations, `(type, function)`, keyed by stub symbol.
pub(super) type Implementations = HashMap<String, Vec<(Target, Target)>>;

/// What the objects record for the stubs.
#[derive(Debug, Default)]
pub(super) struct Records {
    pub(super) implementations: Implementations,
    /// The stubs of property accessors, which have no identity, by the namespace of the module
    /// whose interface declares them.
    unidentified: HashMap<String, String>,
    /// The namespaces of the modules whose interface properties a type of another module
    /// implements.
    implemented_properties: HashSet<String>,
}

impl Records {
    /// Refuse a call through stub `stub` that could miss an implementation: one of a property
    /// accessor that another module's type implements, recorded by no identity.
    pub(super) fn refuse_unidentified_calls(&self, stub: &str) -> Result<(), ProgramLinkError> {
        match self.unidentified.get(stub) {
            Some(namespace) if self.implemented_properties.contains(namespace) => {
                Err(ProgramLinkError::Unsupported(format!(
                    "a call through `{stub}`, a property of an interface another module's type \
                     implements"
                )))
            }
            _ => Ok(()),
        }
    }
}

/// Read every implementation the objects `files` record.
pub(super) fn read(files: &[Elf]) -> Result<Records, ProgramLinkError> {
    let mut records = Records::default();
    let mut identities: HashMap<Vec<u8>, String> = HashMap::default();
    let mut by_identity: Vec<(Vec<u8>, Target, Target)> = Vec::new();
    for (file_index, file) in files.iter().enumerate() {
        for section in file.sections() {
            let Some(name) = section.name().ok() else {
                continue;
            };
            if recorded_stub(name, 0).is_some() {
                read_numbered(
                    file_index,
                    file,
                    &section,
                    name,
                    &mut records.implementations,
                )?;
            } else if identified_stub(name, 0).is_some() {
                read_identities(
                    file_index,
                    &section,
                    name,
                    &mut identities,
                    &mut records.unidentified,
                )?;
            } else if name == DEPENDENCY_IMPLEMENTATIONS_SECTION {
                read_by_identity(file_index, file, &section, &mut by_identity)?;
            } else if name == DEPENDENCY_PROPERTIES_SECTION {
                read_namespaces(file_index, &section, &mut records.implemented_properties)?;
            }
        }
    }
    for (identity, ty, function) in by_identity {
        if let Some(stub) = identities.get(&identity) {
            records
                .implementations
                .entry(stub.clone())
                .or_default()
                .push((ty, function));
        }
    }
    Ok(records)
}

/// The symbol each relocation of `section` names, by the offset it patches. Every one fills a
/// whole word.
fn relocated_words(
    file_index: usize,
    file: &Elf,
    section: &object::read::elf::ElfSection64<'_, '_, object::LittleEndian>,
) -> Result<HashMap<u64, Target>, ProgramLinkError> {
    let size = section.size();
    let mut fields = HashMap::default();
    for (offset, relocation) in section.relocations() {
        let RelocationTarget::Symbol(index) = relocation.target() else {
            return Err(record_error(file_index, offset, "names no symbol"));
        };
        if relocation.addend() != 0 || offset % 8 != 0 || offset >= size {
            return Err(record_error(file_index, offset, "is not a whole field"));
        }
        let symbol = file
            .symbol_by_index(index)
            .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
        let target = if symbol.is_local() {
            Target::Local(file_index, index)
        } else {
            Target::Global(symbol_name(&symbol)?.to_string())
        };
        fields.insert(offset, target);
    }
    Ok(fields)
}

fn section_data<'d>(
    section: &object::read::elf::ElfSection64<'d, '_, object::LittleEndian>,
) -> Result<&'d [u8], ProgramLinkError> {
    section
        .data()
        .map_err(|error| ProgramLinkError::Parse(error.to_string()))
}

fn word(data: &[u8], at: u64) -> Option<u64> {
    let at = usize::try_from(at).ok()?;
    let bytes = data.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(bytes.try_into().expect("a word")))
}

/// Records of a module's own types, by member number.
fn read_numbered(
    file_index: usize,
    file: &Elf,
    section: &object::read::elf::ElfSection64<'_, '_, object::LittleEndian>,
    name: &str,
    implementations: &mut Implementations,
) -> Result<(), ProgramLinkError> {
    let size = section.size();
    if size % RECORD_SIZE != 0 {
        return Err(ProgramLinkError::Parse(format!(
            "input {file_index} has an interface implementation record section of {size} bytes, \
             which is not a whole number of records"
        )));
    }
    let data = section_data(section)?;
    let fields = relocated_words(file_index, file, section)?;
    if let Some(&offset) = fields.keys().find(|offset| *offset % RECORD_SIZE == 0) {
        return Err(record_error(file_index, offset, "is not a whole field"));
    }
    for record in 0..size / RECORD_SIZE {
        let at = record * RECORD_SIZE;
        let field = |offset: u64| {
            fields
                .get(&(at + offset))
                .cloned()
                .ok_or_else(|| record_error(file_index, at + offset, "is empty"))
        };
        let stub = word(data, at)
            .and_then(|number| u32::try_from(number).ok())
            .and_then(|number| recorded_stub(name, number))
            .ok_or_else(|| record_error(file_index, at, "names no member number"))?;
        implementations
            .entry(stub)
            .or_default()
            .push((field(8)?, field(16)?));
    }
    Ok(())
}

/// The identity each member number of a module's interfaces stands for.
fn read_identities(
    file_index: usize,
    section: &object::read::elf::ElfSection64<'_, '_, object::LittleEndian>,
    name: &str,
    identities: &mut HashMap<Vec<u8>, String>,
    unidentified: &mut HashMap<String, String>,
) -> Result<(), ProgramLinkError> {
    let namespace = identities_namespace(name).expect("an identities section");
    let data = section_data(section)?;
    let mut at = 0;
    while at < data.len() as u64 {
        let malformed = || record_error(file_index, at, "is not a whole member identity");
        let number = word(data, at).ok_or_else(malformed)?;
        let length = word(data, at + 8).ok_or_else(malformed)?;
        let start = at + 16;
        let identity = usize::try_from(start)
            .ok()
            .zip(usize::try_from(length).ok())
            .and_then(|(start, length)| data.get(start..start.checked_add(length)?))
            .ok_or_else(malformed)?;
        let stub = u32::try_from(number)
            .ok()
            .and_then(|number| identified_stub(name, number))
            .ok_or_else(malformed)?;
        if identity.is_empty() {
            unidentified.insert(stub, namespace.to_string());
            at = start;
            continue;
        }
        match identities.get(identity) {
            Some(known) if *known != stub => {
                return Err(ProgramLinkError::Parse(format!(
                    "input {file_index}: one interface member identity names both `{known}` and \
                     `{stub}`"
                )))
            }
            Some(_) => {}
            None => {
                identities.insert(identity.to_vec(), stub);
            }
        }
        at = start + padded(length);
    }
    Ok(())
}

/// Implementations a dependent module's types supply for a dependency's members, by identity.
fn read_by_identity(
    file_index: usize,
    file: &Elf,
    section: &object::read::elf::ElfSection64<'_, '_, object::LittleEndian>,
    by_identity: &mut Vec<(Vec<u8>, Target, Target)>,
) -> Result<(), ProgramLinkError> {
    let data = section_data(section)?;
    let fields = relocated_words(file_index, file, section)?;
    let mut at = 0;
    while at < data.len() as u64 {
        let malformed = || {
            record_error(
                file_index,
                at,
                "is not a whole dependency implementation record",
            )
        };
        let length = word(data, at).ok_or_else(malformed)?;
        let field = |offset: u64| {
            fields
                .get(&(at + offset))
                .cloned()
                .ok_or_else(|| record_error(file_index, at + offset, "is empty"))
        };
        let (ty, function) = (field(8)?, field(16)?);
        let start = at + 24;
        let identity = usize::try_from(start)
            .ok()
            .zip(usize::try_from(length).ok())
            .and_then(|(start, length)| data.get(start..start.checked_add(length)?))
            .ok_or_else(malformed)?;
        by_identity.push((identity.to_vec(), ty, function));
        at = start + padded(length);
    }
    Ok(())
}

/// The namespaces a dependency properties section names.
fn read_namespaces(
    file_index: usize,
    section: &object::read::elf::ElfSection64<'_, '_, object::LittleEndian>,
    namespaces: &mut HashSet<String>,
) -> Result<(), ProgramLinkError> {
    let data = section_data(section)?;
    let mut at = 0;
    while at < data.len() as u64 {
        let malformed = || record_error(file_index, at, "is not a whole module namespace");
        let length = word(data, at).ok_or_else(malformed)?;
        let start = at + 8;
        let namespace = usize::try_from(start)
            .ok()
            .zip(usize::try_from(length).ok())
            .and_then(|(start, length)| data.get(start..start.checked_add(length)?))
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .ok_or_else(malformed)?;
        namespaces.insert(namespace.to_string());
        at = start + padded(length);
    }
    Ok(())
}

fn record_error(file_index: usize, offset: u64, why: &str) -> ProgramLinkError {
    ProgramLinkError::Parse(format!(
        "input {file_index}: the interface implementation record field at {offset:#x} {why}"
    ))
}

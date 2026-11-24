//! An optional `.symtab` for a linked executable, so a debugger can name the function a crashing
//! program stopped in.
//!
//! The image has no section headers, because no loader reads them. When the environment variable
//! [`ENVIRONMENT_VARIABLE`] is set, the linker appends a symbol table, its string table and the
//! section headers after the loaded segments and points the ELF header at them. Nothing in the
//! loaded segments moves, so the program behaves identically. Two sections describe the loaded
//! segments (`.text` for the read+execute one, `.data` for the read+write one), and each symbol
//! belongs to the one containing its address, which is what lets a debugger treat a function as
//! an executable location. A symbol in neither is absolute (`SHN_ABS`).

use object::read::{Object, ObjectSymbol};
use object::SymbolKind;
use object::SymbolSection;

use super::{symbol_name, Elf, Layout, ProgramHeader, ProgramLinkError, PT_LOAD};

/// Set to any value to have the linker append a symbol table to the executable.
pub(super) const ENVIRONMENT_VARIABLE: &str = "KRUSTY_NATIVE_SYMBOL_TABLE";

const SHN_ABS: u16 = 0xfff1;
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_PROGBITS: u32 = 1;
const SHF_WRITE: u64 = 1;
const SHF_ALLOC: u64 = 2;
const SHF_EXECINSTR: u64 = 4;
const SYMBOL_SIZE: u64 = 24;
const SECTION_HEADER_SIZE: u64 = 64;

pub(super) fn requested() -> bool {
    std::env::var_os(ENVIRONMENT_VARIABLE).is_some()
}

struct Entry {
    name: u32,
    info: u8,
    value: u64,
    size: u64,
    section: u16,
}

/// What a section header says besides its name and type.
struct Section {
    name: u32,
    kind: u32,
    flags: u64,
    address: u64,
    offset: u64,
    size: u64,
    link: u32,
    info: u32,
}

fn section_header(image: &mut Vec<u8>, section: &Section) {
    let Section {
        name,
        kind,
        flags,
        address,
        offset,
        size,
        link,
        info,
    } = *section;
    let (align, entry_size) = if kind == SHT_SYMTAB {
        (8u64, SYMBOL_SIZE)
    } else {
        (1, 0)
    };
    image.extend_from_slice(&name.to_le_bytes());
    image.extend_from_slice(&kind.to_le_bytes());
    image.extend_from_slice(&flags.to_le_bytes());
    image.extend_from_slice(&address.to_le_bytes());
    image.extend_from_slice(&offset.to_le_bytes());
    image.extend_from_slice(&size.to_le_bytes());
    image.extend_from_slice(&link.to_le_bytes());
    image.extend_from_slice(&info.to_le_bytes());
    image.extend_from_slice(&align.to_le_bytes());
    image.extend_from_slice(&entry_size.to_le_bytes());
}

/// Append the table for every function and data symbol the layout placed.
pub(super) fn append(
    image: &mut Vec<u8>,
    files: &[Elf],
    layout: &Layout,
    program_headers: &[ProgramHeader],
) -> Result<(), ProgramLinkError> {
    // The loaded segments, in order: the executable one, then the writable one.
    let loaded: Vec<&ProgramHeader> = program_headers
        .iter()
        .filter(|header| header.p_type == PT_LOAD)
        .collect();
    let section_of = |address: u64| {
        loaded
            .iter()
            .position(|segment| {
                address >= segment.vaddr && address < segment.vaddr + segment.memsz.max(1)
            })
            .map_or(SHN_ABS, |index| index as u16 + 1)
    };
    let mut strings = vec![0u8];
    let mut locals = Vec::new();
    let mut globals = Vec::new();
    for (file_index, file) in files.iter().enumerate() {
        for symbol in file.symbols() {
            let SymbolSection::Section(section) = symbol.section() else {
                continue;
            };
            let Some(place) = layout.placed.get(&(file_index, section)) else {
                continue;
            };
            let kind = match symbol.kind() {
                SymbolKind::Text => 2,
                SymbolKind::Data => 1,
                _ => continue,
            };
            let name = symbol_name(&symbol)?;
            if name.is_empty() {
                continue;
            }
            let offset = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            let binding = if symbol.is_local() { 0 } else { 1 };
            let value = place.vaddr + symbol.address();
            let entry = Entry {
                name: offset,
                info: (binding << 4) | kind,
                value,
                size: symbol.size(),
                section: section_of(value),
            };
            if symbol.is_local() {
                locals.push(entry);
            } else {
                globals.push(entry);
            }
        }
    }

    let mut symbols = vec![0u8; SYMBOL_SIZE as usize];
    for entry in locals.iter().chain(&globals) {
        symbols.extend_from_slice(&entry.name.to_le_bytes());
        symbols.push(entry.info);
        symbols.push(0);
        symbols.extend_from_slice(&entry.section.to_le_bytes());
        symbols.extend_from_slice(&entry.value.to_le_bytes());
        symbols.extend_from_slice(&entry.size.to_le_bytes());
    }
    let first_global = locals.len() as u32 + 1;

    let section_names = b"\0.text\0.data\0.symtab\0.strtab\0.shstrtab\0";
    let (text_name, data_name, symtab_name, strtab_name, shstrtab_name) =
        (1u32, 7u32, 13u32, 21u32, 29u32);

    let mut place = |bytes: &[u8]| {
        while !image.len().is_multiple_of(8) {
            image.push(0);
        }
        let offset = image.len() as u64;
        image.extend_from_slice(bytes);
        offset
    };
    let symtab_at = place(&symbols);
    let strtab_at = place(&strings);
    let shstrtab_at = place(section_names);
    while !image.len().is_multiple_of(8) {
        image.push(0);
    }
    let headers_at = image.len() as u64;

    image.extend_from_slice(&[0; SECTION_HEADER_SIZE as usize]);
    // `.text` and `.data` describe the loaded segments; a program with one segment has no `.data`.
    let loaded_names = [
        (text_name, SHF_ALLOC | SHF_EXECINSTR),
        (data_name, SHF_ALLOC | SHF_WRITE),
    ];
    for (segment, (name, flags)) in loaded.iter().zip(loaded_names) {
        section_header(
            image,
            &Section {
                name,
                kind: SHT_PROGBITS,
                flags,
                address: segment.vaddr,
                offset: segment.offset,
                size: segment.filesz,
                link: 0,
                info: 0,
            },
        );
    }
    let first_table = loaded.len() as u32 + 1;
    section_header(
        image,
        &Section {
            name: symtab_name,
            kind: SHT_SYMTAB,
            flags: 0,
            address: 0,
            offset: symtab_at,
            size: symbols.len() as u64,
            link: first_table + 1,
            info: first_global,
        },
    );
    for (name, offset, size) in [
        (strtab_name, strtab_at, strings.len() as u64),
        (shstrtab_name, shstrtab_at, section_names.len() as u64),
    ] {
        section_header(
            image,
            &Section {
                name,
                kind: SHT_STRTAB,
                flags: 0,
                address: 0,
                offset,
                size,
                link: 0,
                info: 0,
            },
        );
    }

    image[0x28..0x30].copy_from_slice(&headers_at.to_le_bytes()); // e_shoff
    let count = first_table as u16 + 3; // null, the loaded sections, symtab, strtab, shstrtab
    image[0x3c..0x3e].copy_from_slice(&count.to_le_bytes()); // e_shnum
    image[0x3e..0x40].copy_from_slice(&(count - 1).to_le_bytes()); // e_shstrndx
    Ok(())
}

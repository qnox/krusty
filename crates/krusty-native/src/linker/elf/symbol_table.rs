//! An optional `.symtab` for a linked executable, so a debugger can name the function a crashing
//! program stopped in.
//!
//! The image has no section headers, because no loader reads them. When the environment variable
//! [`ENVIRONMENT_VARIABLE`] is set, the linker appends a symbol table, its string table and the
//! section headers after the loaded segments and points the ELF header at them. Nothing in the
//! loaded segments moves, so the program behaves identically. Symbols are absolute (`SHN_ABS`)
//! with their final virtual addresses: there are no output sections for them to belong to.

use object::read::{Object, ObjectSymbol};
use object::SymbolKind;
use object::SymbolSection;

use super::{symbol_name, Elf, Layout, ProgramLinkError};

/// Set to any value to have the linker append a symbol table to the executable.
pub(super) const ENVIRONMENT_VARIABLE: &str = "KRUSTY_NATIVE_SYMBOL_TABLE";

const SHN_ABS: u16 = 0xfff1;
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;
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
}

fn section_header(
    image: &mut Vec<u8>,
    name: u32,
    kind: u32,
    (offset, size): (u64, u64),
    link: u32,
    info: u32,
) {
    let (align, entry_size) = if kind == SHT_SYMTAB {
        (8u64, SYMBOL_SIZE)
    } else {
        (1, 0)
    };
    image.extend_from_slice(&name.to_le_bytes());
    image.extend_from_slice(&kind.to_le_bytes());
    image.extend_from_slice(&0u64.to_le_bytes()); // sh_flags
    image.extend_from_slice(&0u64.to_le_bytes()); // sh_addr: not loaded
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
) -> Result<(), ProgramLinkError> {
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
            let entry = Entry {
                name: offset,
                info: (binding << 4) | kind,
                value: place.vaddr + symbol.address(),
                size: symbol.size(),
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
        symbols.extend_from_slice(&SHN_ABS.to_le_bytes());
        symbols.extend_from_slice(&entry.value.to_le_bytes());
        symbols.extend_from_slice(&entry.size.to_le_bytes());
    }
    let first_global = locals.len() as u32 + 1;

    let section_names = b"\0.symtab\0.strtab\0.shstrtab\0";
    let (symtab_name, strtab_name, shstrtab_name) = (1u32, 9u32, 17u32);

    let mut place = |bytes: &[u8]| {
        while image.len() % 8 != 0 {
            image.push(0);
        }
        let offset = image.len() as u64;
        image.extend_from_slice(bytes);
        offset
    };
    let symtab_at = place(&symbols);
    let strtab_at = place(&strings);
    let shstrtab_at = place(section_names);
    while image.len() % 8 != 0 {
        image.push(0);
    }
    let headers_at = image.len() as u64;

    image.extend_from_slice(&[0; SECTION_HEADER_SIZE as usize]);
    section_header(
        image,
        symtab_name,
        SHT_SYMTAB,
        (symtab_at, symbols.len() as u64),
        2,
        first_global,
    );
    section_header(
        image,
        strtab_name,
        SHT_STRTAB,
        (strtab_at, strings.len() as u64),
        0,
        0,
    );
    section_header(
        image,
        shstrtab_name,
        SHT_STRTAB,
        (shstrtab_at, section_names.len() as u64),
        0,
        0,
    );

    image[0x28..0x30].copy_from_slice(&headers_at.to_le_bytes()); // e_shoff
    image[0x3c..0x3e].copy_from_slice(&4u16.to_le_bytes()); // e_shnum
    image[0x3e..0x40].copy_from_slice(&3u16.to_le_bytes()); // e_shstrndx
    Ok(())
}

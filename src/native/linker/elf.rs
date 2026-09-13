//! Static ELF64 linking.
//!
//! Layout is the simplest one that is correct: two `PT_LOAD` segments. The first is read+execute
//! and holds the ELF headers, every `.text`, then every read-only data section; the second is
//! read+write and holds every `.data`, then every `.bss` as memory beyond the file. The image is
//! linked at a fixed base, which is what lets the C runtime be compiled non-PIC with a small code
//! model and lets Cranelift emit absolute references — both then need only the handful of
//! relocation kinds implemented here.
//!
//! Read-only data sharing the executable segment is a simplification, not an oversight: a third
//! read-only segment is cheap to add when the runtime grows something worth protecting.

use std::collections::HashMap;

use object::read::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
use object::{RelocationFlags, SectionKind, SymbolKind};

use super::super::target::{Arch, NativeTarget};
use super::ProgramLinkError;

/// Where the executable segment begins, in memory and therefore where the file is mapped.
const BASE: u64 = 0x400000;
const PAGE: u64 = 0x1000;
const EHDR_SIZE: u64 = 64;
const PHDR_SIZE: u64 = 56;
const PHDR_COUNT: u64 = 2;

/// Which output segment an input section lands in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Placement {
    Text,
    ReadOnly,
    Data,
    Bss,
}

fn placement(kind: SectionKind) -> Option<Placement> {
    Some(match kind {
        SectionKind::Text => Placement::Text,
        SectionKind::ReadOnlyData
        | SectionKind::ReadOnlyString
        | SectionKind::ReadOnlyDataWithRel => Placement::ReadOnly,
        SectionKind::Data => Placement::Data,
        SectionKind::UninitializedData => Placement::Bss,
        // Relocation tables, symbol tables, notes, `.comment`, debug info: consumed or dropped.
        _ => return None,
    })
}

/// One input section's place in the output.
#[derive(Clone, Copy)]
struct Placed {
    vaddr: u64,
    /// Offset in the output file; meaningless for `.bss`.
    file_offset: u64,
}

fn align_up(value: u64, align: u64) -> u64 {
    if align <= 1 {
        value
    } else {
        (value + align - 1) & !(align - 1)
    }
}

pub(super) fn link_static(
    inputs: &[&[u8]],
    target: NativeTarget,
) -> Result<Vec<u8>, ProgramLinkError> {
    let files = inputs
        .iter()
        .map(|bytes| {
            object::File::parse(*bytes).map_err(|error| ProgramLinkError::Parse(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    // ---- lay sections into segments ----------------------------------------------------------
    // Key: (file index, section index) → placement in the output.
    let mut placed: HashMap<(usize, object::SectionIndex), Placed> = HashMap::new();

    // Executable segment: headers, then text, then read-only data. Offsets equal virtual addresses
    // minus BASE, since the segment is mapped from file offset 0.
    let mut cursor = EHDR_SIZE + PHDR_SIZE * PHDR_COUNT;
    for wanted in [Placement::Text, Placement::ReadOnly] {
        for (file_index, file) in files.iter().enumerate() {
            for section in file.sections() {
                if placement(section.kind()) != Some(wanted) || section.size() == 0 {
                    continue;
                }
                cursor = align_up(cursor, section.align().max(1));
                placed.insert(
                    (file_index, section.index()),
                    Placed {
                        vaddr: BASE + cursor,
                        file_offset: cursor,
                    },
                );
                cursor += section.size();
            }
        }
    }
    let text_file_end = cursor;

    // Writable segment: data, then bss. File offset and address stay congruent modulo the page.
    let data_file_start = align_up(text_file_end, PAGE);
    let data_vaddr_start = align_up(BASE + text_file_end, PAGE);
    let mut cursor = 0u64; // relative to the writable segment
    for (file_index, file) in files.iter().enumerate() {
        for section in file.sections() {
            if placement(section.kind()) != Some(Placement::Data) || section.size() == 0 {
                continue;
            }
            cursor = align_up(cursor, section.align().max(1));
            placed.insert(
                (file_index, section.index()),
                Placed {
                    vaddr: data_vaddr_start + cursor,
                    file_offset: data_file_start + cursor,
                },
            );
            cursor += section.size();
        }
    }
    let data_file_size = cursor;
    for (file_index, file) in files.iter().enumerate() {
        for section in file.sections() {
            if placement(section.kind()) != Some(Placement::Bss) || section.size() == 0 {
                continue;
            }
            cursor = align_up(cursor, section.align().max(1));
            placed.insert(
                (file_index, section.index()),
                Placed {
                    vaddr: data_vaddr_start + cursor,
                    file_offset: 0,
                },
            );
            cursor += section.size();
        }
    }
    let data_mem_size = cursor;

    // ---- resolve symbols ---------------------------------------------------------------------
    // Globals by name across every object; locals by (file, symbol index).
    let mut globals: HashMap<String, u64> = HashMap::new();
    let mut weak: HashMap<String, u64> = HashMap::new();
    let mut locals: HashMap<(usize, object::SymbolIndex), u64> = HashMap::new();
    for (file_index, file) in files.iter().enumerate() {
        for symbol in file.symbols() {
            let Some(section_index) = symbol.section_index() else {
                continue; // undefined, absolute or common: nothing to define here
            };
            let Some(place) = placed.get(&(file_index, section_index)) else {
                continue; // lives in a section that was dropped (debug, notes)
            };
            let address = place.vaddr + symbol.address();
            locals.insert((file_index, symbol.index()), address);
            if matches!(symbol.kind(), SymbolKind::Section | SymbolKind::File) {
                continue;
            }
            if symbol.is_global() {
                let name = symbol
                    .name()
                    .map_err(|error| ProgramLinkError::Parse(error.to_string()))?
                    .to_string();
                if symbol.is_weak() {
                    weak.entry(name).or_insert(address);
                } else if globals.insert(name.clone(), address).is_some() {
                    return Err(ProgramLinkError::DuplicateSymbol(name));
                }
            }
        }
    }
    let lookup_global = |name: &str| globals.get(name).or_else(|| weak.get(name)).copied();
    let entry = lookup_global("_start")
        .ok_or_else(|| ProgramLinkError::UndefinedSymbol("_start".into()))?;

    // ---- build the image ---------------------------------------------------------------------
    let file_size = data_file_start + data_file_size;
    let mut image = vec![0u8; file_size as usize];
    for (file_index, file) in files.iter().enumerate() {
        for section in file.sections() {
            let Some(place) = placed.get(&(file_index, section.index())) else {
                continue;
            };
            if placement(section.kind()) == Some(Placement::Bss) {
                continue;
            }
            let data = section
                .data()
                .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
            let start = place.file_offset as usize;
            image[start..start + data.len()].copy_from_slice(data);
        }
    }

    // ---- apply relocations -------------------------------------------------------------------
    for (file_index, file) in files.iter().enumerate() {
        for section in file.sections() {
            let Some(place) = placed.get(&(file_index, section.index())).copied() else {
                continue;
            };
            for (offset, relocation) in section.relocations() {
                let symbol_value = match relocation.target() {
                    RelocationTarget::Symbol(index) => {
                        if let Some(address) = locals.get(&(file_index, index)) {
                            *address
                        } else {
                            let symbol = file
                                .symbol_by_index(index)
                                .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
                            let name = symbol
                                .name()
                                .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
                            lookup_global(name).ok_or_else(|| {
                                ProgramLinkError::UndefinedSymbol(name.to_string())
                            })?
                        }
                    }
                    RelocationTarget::Section(index) => placed
                        .get(&(file_index, index))
                        .map(|p| p.vaddr)
                        .ok_or_else(|| {
                            ProgramLinkError::Unsupported(
                                "relocation against a dropped section".into(),
                            )
                        })?,
                    RelocationTarget::Absolute => 0,
                    _ => {
                        return Err(ProgramLinkError::Unsupported(
                            "relocation target kind".into(),
                        ))
                    }
                };
                let RelocationFlags::Elf { r_type } = relocation.flags() else {
                    return Err(ProgramLinkError::Unsupported("non-ELF relocation".into()));
                };
                let addend = relocation.addend();
                let site_vaddr = place.vaddr + offset;
                let site_file = (place.file_offset + offset) as usize;
                apply(
                    target.arch,
                    r_type.0,
                    &mut image,
                    site_file,
                    site_vaddr,
                    symbol_value,
                    addend,
                )?;
            }
        }
    }

    // ---- headers -----------------------------------------------------------------------------
    write_headers(
        &mut image,
        target.arch,
        entry,
        text_file_end,
        data_file_start,
        data_vaddr_start,
        data_file_size,
        data_mem_size,
    );
    Ok(image)
}

/// Apply one relocation. `S` is the symbol value, `A` the addend, `P` the site's address.
fn apply(
    arch: Arch,
    r_type: u32,
    image: &mut [u8],
    site: usize,
    p: u64,
    s: u64,
    a: i64,
) -> Result<(), ProgramLinkError> {
    let s_plus_a = (s as i64).wrapping_add(a);
    let pc_relative = s_plus_a.wrapping_sub(p as i64);
    let write32 = |image: &mut [u8], value: i64, signed: bool, what: &str| {
        let fits = if signed {
            i32::try_from(value).is_ok()
        } else {
            u32::try_from(value).is_ok()
        };
        if !fits {
            return Err(ProgramLinkError::RelocationOutOfRange(format!(
                "{what}: value {value:#x} at {p:#x}"
            )));
        }
        image[site..site + 4].copy_from_slice(&(value as u32).to_le_bytes());
        Ok(())
    };
    match arch {
        Arch::X86_64 => match r_type {
            // R_X86_64_64
            1 => {
                image[site..site + 8].copy_from_slice(&(s_plus_a as u64).to_le_bytes());
                Ok(())
            }
            // R_X86_64_PC32, R_X86_64_PLT32: a static link has no PLT, so both are PC-relative to S.
            2 | 4 => write32(image, pc_relative, true, "PC32"),
            // R_X86_64_32 (zero-extended) and R_X86_64_32S (sign-extended) absolute.
            10 => write32(image, s_plus_a, false, "32"),
            11 => write32(image, s_plus_a, true, "32S"),
            other => Err(ProgramLinkError::UnsupportedRelocation {
                arch,
                r_type: other,
            }),
        },
        other => Err(ProgramLinkError::UnsupportedRelocation {
            arch: other,
            r_type,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_headers(
    image: &mut [u8],
    arch: Arch,
    entry: u64,
    text_file_end: u64,
    data_file_start: u64,
    data_vaddr_start: u64,
    data_file_size: u64,
    data_mem_size: u64,
) {
    let (machine, flags): (u16, u32) = match arch {
        Arch::X86_64 => (62, 0),
        Arch::Aarch64 => (183, 0),
        // EF_RISCV_RVC | EF_RISCV_FLOAT_ABI_DOUBLE: what `-march=rv64gc -mabi=lp64d` objects carry.
        Arch::Riscv64 => (243, 0x0005),
    };
    let mut h = Vec::with_capacity(64);
    h.extend_from_slice(b"\x7fELF");
    h.extend_from_slice(&[2, 1, 1, 0]); // 64-bit, little-endian, ELF version 1, System V ABI
    h.extend_from_slice(&[0; 8]);
    h.extend_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    h.extend_from_slice(&machine.to_le_bytes());
    h.extend_from_slice(&1u32.to_le_bytes()); // e_version
    h.extend_from_slice(&entry.to_le_bytes());
    h.extend_from_slice(&EHDR_SIZE.to_le_bytes()); // e_phoff
    h.extend_from_slice(&0u64.to_le_bytes()); // e_shoff: no section headers
    h.extend_from_slice(&flags.to_le_bytes());
    h.extend_from_slice(&(EHDR_SIZE as u16).to_le_bytes());
    h.extend_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
    h.extend_from_slice(&(PHDR_COUNT as u16).to_le_bytes());
    h.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
    h.extend_from_slice(&0u16.to_le_bytes()); // e_shnum
    h.extend_from_slice(&0u16.to_le_bytes()); // e_shstrndx
    debug_assert_eq!(h.len(), EHDR_SIZE as usize);

    let mut phdr = |p_flags: u32, offset: u64, vaddr: u64, filesz: u64, memsz: u64| {
        let mut p = Vec::with_capacity(56);
        p.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        p.extend_from_slice(&p_flags.to_le_bytes());
        p.extend_from_slice(&offset.to_le_bytes());
        p.extend_from_slice(&vaddr.to_le_bytes());
        p.extend_from_slice(&vaddr.to_le_bytes()); // p_paddr
        p.extend_from_slice(&filesz.to_le_bytes());
        p.extend_from_slice(&memsz.to_le_bytes());
        p.extend_from_slice(&PAGE.to_le_bytes());
        h.extend_from_slice(&p);
    };
    phdr(0x5, 0, BASE, text_file_end, text_file_end); // R+X
    phdr(
        0x6,
        data_file_start,
        data_vaddr_start,
        data_file_size,
        data_mem_size,
    ); // R+W
    image[..h.len()].copy_from_slice(&h);
}

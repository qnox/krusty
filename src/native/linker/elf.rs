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

/// Every global symbol the prebuilt runtime for `arch` DEFINES.
///
/// The code generator asks for this so it never names a function of its own after one of them: a
/// Kotlin `fun cast(...)` becomes `kt_cast`, which is also the runtime's cast helper, and two
/// definitions of one symbol fail the link with nothing to say about where the Kotlin name was.
/// Reading the answer out of the runtime objects keeps it true as the runtime grows, which a list
/// written down beside them would not.
pub(crate) fn runtime_symbols(arch: Arch) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let Some(objects) = super::prebuilt::runtime_objects(arch) else {
        return names;
    };
    for (_, bytes) in objects {
        let Ok(file) = object::File::parse(*bytes) else {
            continue;
        };
        for symbol in file.symbols() {
            if symbol.section_index().is_none() || !symbol.is_global() {
                continue;
            }
            if let Ok(name) = symbol.name() {
                names.insert(name.to_string());
            }
        }
    }
    names
}
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
    // Gathered first, then applied per architecture: RISC-V's `PCREL_LO12` needs the result of the
    // `PCREL_HI20` it is paired with, which may sit anywhere in the same object.
    for (file_index, file) in files.iter().enumerate() {
        let mut relocations = Vec::new();
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
                relocations.push(Reloc {
                    r_type: r_type.0,
                    site_file: (place.file_offset + offset) as usize,
                    p: place.vaddr + offset,
                    s: symbol_value,
                    a: relocation.addend(),
                });
            }
        }
        relocate(target.arch, &mut image, &relocations)?;
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

/// One relocation, resolved: where it is, what it points at.
struct Reloc {
    r_type: u32,
    /// Offset of the site in the output file.
    site_file: usize,
    /// Address of the site: `P` in the ABI documents.
    p: u64,
    /// Value of the symbol: `S`.
    s: u64,
    /// Addend: `A`.
    a: i64,
}

fn out_of_range(what: &str, value: i64, p: u64) -> ProgramLinkError {
    ProgramLinkError::RelocationOutOfRange(format!("{what}: value {value:#x} at {p:#x}"))
}

fn read32(image: &[u8], site: usize) -> u32 {
    u32::from_le_bytes(image[site..site + 4].try_into().expect("4 bytes"))
}

fn write32(image: &mut [u8], site: usize, value: u32) {
    image[site..site + 4].copy_from_slice(&value.to_le_bytes());
}

fn read16(image: &[u8], site: usize) -> u16 {
    u16::from_le_bytes(image[site..site + 2].try_into().expect("2 bytes"))
}

fn write16(image: &mut [u8], site: usize, value: u16) {
    image[site..site + 2].copy_from_slice(&value.to_le_bytes());
}

/// Does `value` fit in a signed field of `bits` bits?
fn fits_signed(value: i64, bits: u32) -> bool {
    let min = -(1i64 << (bits - 1));
    let max = (1i64 << (bits - 1)) - 1;
    (min..=max).contains(&value)
}

/// Apply every relocation of one object for `arch`.
fn relocate(arch: Arch, image: &mut [u8], relocations: &[Reloc]) -> Result<(), ProgramLinkError> {
    match arch {
        Arch::X86_64 => relocations
            .iter()
            .try_for_each(|r| relocate_x86_64(image, r)),
        Arch::Aarch64 => relocations
            .iter()
            .try_for_each(|r| relocate_aarch64(image, r)),
        Arch::Riscv64 => relocate_riscv64(image, relocations),
    }
}

fn relocate_x86_64(image: &mut [u8], r: &Reloc) -> Result<(), ProgramLinkError> {
    let s_plus_a = (r.s as i64).wrapping_add(r.a);
    let pc_relative = s_plus_a.wrapping_sub(r.p as i64);
    match r.r_type {
        // R_X86_64_64
        1 => image[r.site_file..r.site_file + 8].copy_from_slice(&(s_plus_a as u64).to_le_bytes()),
        // R_X86_64_PC32, R_X86_64_PLT32: a static link has no PLT, so both are PC-relative to S.
        2 | 4 => {
            if !fits_signed(pc_relative, 32) {
                return Err(out_of_range("PC32", pc_relative, r.p));
            }
            write32(image, r.site_file, pc_relative as u32);
        }
        // R_X86_64_32 (zero-extended) and R_X86_64_32S (sign-extended) absolute.
        10 => {
            if u32::try_from(s_plus_a).is_err() {
                return Err(out_of_range("32", s_plus_a, r.p));
            }
            write32(image, r.site_file, s_plus_a as u32);
        }
        11 => {
            if !fits_signed(s_plus_a, 32) {
                return Err(out_of_range("32S", s_plus_a, r.p));
            }
            write32(image, r.site_file, s_plus_a as u32);
        }
        other => {
            return Err(ProgramLinkError::UnsupportedRelocation {
                arch: Arch::X86_64,
                r_type: other,
            })
        }
    }
    Ok(())
}

/// AArch64: every kind clang's freestanding objects and Cranelift's non-PIC output use. Fields are
/// patched into fixed 32-bit instructions per the ELF-for-AArch64 supplement.
fn relocate_aarch64(image: &mut [u8], r: &Reloc) -> Result<(), ProgramLinkError> {
    let x = (r.s as i64).wrapping_add(r.a);
    let rel = x.wrapping_sub(r.p as i64);
    let site = r.site_file;
    // Insert the low 12 bits of `x`, scaled by `shift`, into an LDST/ADD imm12 (bits 21:10).
    let imm12 = |image: &mut [u8], shift: u32| {
        let field = ((x as u64 & 0xfff) >> shift) as u32;
        let instruction = (read32(image, site) & !(0xfff << 10)) | (field << 10);
        write32(image, site, instruction);
    };
    match r.r_type {
        // R_AARCH64_ABS64
        257 => image[site..site + 8].copy_from_slice(&(x as u64).to_le_bytes()),
        // R_AARCH64_PREL32 (`.eh_frame` and friends)
        261 => {
            if !fits_signed(rel, 32) {
                return Err(out_of_range("PREL32", rel, r.p));
            }
            write32(image, site, rel as u32);
        }
        // R_AARCH64_ADR_PREL_PG_HI21: page delta into ADRP's immhi:immlo.
        275 => {
            let page_delta = ((x as u64 & !0xfff) as i64).wrapping_sub((r.p & !0xfff) as i64);
            if !fits_signed(page_delta, 33) {
                return Err(out_of_range("ADR_PREL_PG_HI21", page_delta, r.p));
            }
            let pages = (page_delta >> 12) as u64;
            let immlo = ((pages & 0x3) as u32) << 29;
            let immhi = (((pages >> 2) & 0x7ffff) as u32) << 5;
            let instruction =
                (read32(image, site) & !((0x3 << 29) | (0x7ffff << 5))) | immlo | immhi;
            write32(image, site, instruction);
        }
        // R_AARCH64_ADD_ABS_LO12_NC and the LDST*_ABS_LO12_NC family: low 12 bits, scaled by access size.
        277 | 278 => imm12(image, 0),
        284 => imm12(image, 1),
        285 => imm12(image, 2),
        286 => imm12(image, 3),
        299 => imm12(image, 4),
        // R_AARCH64_JUMP26 / R_AARCH64_CALL26: imm26 = (S+A-P) >> 2.
        282 | 283 => {
            if rel & 3 != 0 || !fits_signed(rel, 28) {
                return Err(out_of_range("CALL26", rel, r.p));
            }
            let imm26 = ((rel >> 2) as u32) & 0x03ff_ffff;
            let instruction = (read32(image, site) & !0x03ff_ffff) | imm26;
            write32(image, site, instruction);
        }
        other => {
            return Err(ProgramLinkError::UnsupportedRelocation {
                arch: Arch::Aarch64,
                r_type: other,
            })
        }
    }
    Ok(())
}

/// RISC-V: two passes, because a `PCREL_LO12_*` relocation names the `auipc` it pairs with rather
/// than the final symbol, and takes the low half of THAT relocation's value. `RELAX` is ignored:
/// this linker performs no relaxation, so every instruction stays where the assembler put it.
fn relocate_riscv64(image: &mut [u8], relocations: &[Reloc]) -> Result<(), ProgramLinkError> {
    // Value `X = S + A - P` of every PCREL_HI20, keyed by the address of its `auipc`.
    let mut hi20_at: HashMap<u64, i64> = HashMap::new();
    let mut deferred = Vec::new();
    for r in relocations {
        let x = (r.s as i64).wrapping_add(r.a);
        let rel = x.wrapping_sub(r.p as i64);
        let site = r.site_file;
        match r.r_type {
            // R_RISCV_64 / R_RISCV_32
            2 => image[site..site + 8].copy_from_slice(&(x as u64).to_le_bytes()),
            1 => {
                if u32::try_from(x).is_err() && !fits_signed(x, 32) {
                    return Err(out_of_range("32", x, r.p));
                }
                write32(image, site, x as u32);
            }
            // R_RISCV_BRANCH: B-type immediate.
            16 => {
                if rel & 1 != 0 || !fits_signed(rel, 13) {
                    return Err(out_of_range("BRANCH", rel, r.p));
                }
                write32(
                    image,
                    site,
                    (read32(image, site) & 0x01ff_f07f) | b_type(rel as u32),
                );
            }
            // R_RISCV_JAL: J-type immediate.
            17 => {
                if rel & 1 != 0 || !fits_signed(rel, 21) {
                    return Err(out_of_range("JAL", rel, r.p));
                }
                write32(
                    image,
                    site,
                    (read32(image, site) & 0x0000_0fff) | j_type(rel as u32),
                );
            }
            // R_RISCV_CALL / R_RISCV_CALL_PLT: `auipc` at P, `jalr` at P+4; no PLT in a static link.
            18 | 19 => {
                if !fits_signed(rel, 32) {
                    return Err(out_of_range("CALL", rel, r.p));
                }
                let (hi, lo) = split_hi_lo(rel);
                write32(image, site, (read32(image, site) & 0xfff) | (hi << 12));
                write32(
                    image,
                    site + 4,
                    (read32(image, site + 4) & 0x000f_ffff) | (lo << 20),
                );
            }
            // R_RISCV_PCREL_HI20: remember X for the paired LO12.
            23 => {
                if !fits_signed(rel, 32) {
                    return Err(out_of_range("PCREL_HI20", rel, r.p));
                }
                let (hi, _) = split_hi_lo(rel);
                write32(image, site, (read32(image, site) & 0xfff) | (hi << 12));
                hi20_at.insert(r.p, rel);
            }
            // R_RISCV_PCREL_LO12_I / _S: resolved after every HI20 is known.
            24 | 25 => deferred.push(r),
            // R_RISCV_HI20 / LO12_I / LO12_S: absolute, split the same way.
            26 => {
                if !fits_signed(x, 32) {
                    return Err(out_of_range("HI20", x, r.p));
                }
                let (hi, _) = split_hi_lo(x);
                write32(image, site, (read32(image, site) & 0xfff) | (hi << 12));
            }
            27 => {
                let (_, lo) = split_hi_lo(x);
                write32(
                    image,
                    site,
                    (read32(image, site) & 0x000f_ffff) | (lo << 20),
                );
            }
            28 => {
                let (_, lo) = split_hi_lo(x);
                write32(
                    image,
                    site,
                    (read32(image, site) & 0x01ff_f07f) | s_type(lo),
                );
            }
            // R_RISCV_RVC_BRANCH / R_RISCV_RVC_JUMP: 16-bit compressed forms.
            44 => {
                if rel & 1 != 0 || !fits_signed(rel, 9) {
                    return Err(out_of_range("RVC_BRANCH", rel, r.p));
                }
                write16(
                    image,
                    site,
                    (read16(image, site) & 0xe383) | cb_type(rel as u32),
                );
            }
            45 => {
                if rel & 1 != 0 || !fits_signed(rel, 12) {
                    return Err(out_of_range("RVC_JUMP", rel, r.p));
                }
                write16(
                    image,
                    site,
                    (read16(image, site) & 0xe003) | cj_type(rel as u32),
                );
            }
            // R_RISCV_RELAX, R_RISCV_ALIGN: hints for a relaxing linker; this one does not relax.
            43 | 51 => {}
            other => {
                return Err(ProgramLinkError::UnsupportedRelocation {
                    arch: Arch::Riscv64,
                    r_type: other,
                })
            }
        }
    }
    for r in deferred {
        // The symbol names the `auipc`; its recorded value is what we take the low half of.
        let Some(&x) = hi20_at.get(&r.s) else {
            return Err(ProgramLinkError::Unsupported(format!(
                "PCREL_LO12 at {:#x} has no PCREL_HI20 at {:#x}",
                r.p, r.s
            )));
        };
        let (_, lo) = split_hi_lo(x);
        let site = r.site_file;
        match r.r_type {
            24 => write32(
                image,
                site,
                (read32(image, site) & 0x000f_ffff) | (lo << 20),
            ),
            25 => write32(
                image,
                site,
                (read32(image, site) & 0x01ff_f07f) | s_type(lo),
            ),
            _ => unreachable!("only LO12 relocations are deferred"),
        }
    }
    Ok(())
}

/// Split a 32-bit value into RISC-V's `hi20`/`lo12` pair, where `lo12` is sign-extended and `hi20`
/// is adjusted so that `(hi20 << 12) + sext(lo12) == value`.
fn split_hi_lo(value: i64) -> (u32, u32) {
    let hi = ((value + 0x800) >> 12) as u32 & 0xf_ffff;
    let lo = (value as u32).wrapping_sub(hi << 12) & 0xfff;
    (hi, lo)
}

fn b_type(imm: u32) -> u32 {
    ((imm >> 12) & 1) << 31
        | ((imm >> 5) & 0x3f) << 25
        | ((imm >> 1) & 0xf) << 8
        | ((imm >> 11) & 1) << 7
}

fn j_type(imm: u32) -> u32 {
    ((imm >> 20) & 1) << 31
        | ((imm >> 1) & 0x3ff) << 21
        | ((imm >> 11) & 1) << 20
        | ((imm >> 12) & 0xff) << 12
}

fn s_type(imm: u32) -> u32 {
    ((imm >> 5) & 0x7f) << 25 | (imm & 0x1f) << 7
}

fn cb_type(imm: u32) -> u16 {
    (((imm >> 8) & 1) << 12
        | ((imm >> 3) & 3) << 10
        | ((imm >> 6) & 3) << 5
        | ((imm >> 1) & 3) << 3
        | ((imm >> 5) & 1) << 2) as u16
}

fn cj_type(imm: u32) -> u16 {
    (((imm >> 11) & 1) << 12
        | ((imm >> 4) & 1) << 11
        | ((imm >> 8) & 3) << 9
        | ((imm >> 10) & 1) << 8
        | ((imm >> 6) & 1) << 7
        | ((imm >> 7) & 1) << 6
        | ((imm >> 1) & 7) << 3
        | ((imm >> 5) & 1) << 2) as u16
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

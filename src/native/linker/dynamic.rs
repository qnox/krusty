//! Importing functions from shared libraries: what turns the static image into a dynamically
//! linked one.
//!
//! The image stays a non-PIE executable at the fixed base, so nothing in the program or the
//! runtime is relocated at load time. The only addresses the loader fills in are one global offset
//! table slot per imported function, and every call to an import goes through a stub in the
//! executable segment that jumps through that slot. A reference to the import (a call, or an
//! absolute address in a literal) therefore resolves at link time, to its stub, exactly as a
//! reference to a function the program defines would.
//!
//! The executable asks the loader to bind every slot before the program starts (`DF_BIND_NOW`),
//! so no lazy-binding trampoline is needed and a missing function fails at startup rather than at
//! its first call. It exports nothing: the symbol table lists only the imports, and the hash table
//! the loader searches the executable with is empty.
//!
//! Each import is bound at the version the library makes default for it (`name@@VERSION`). A
//! reference without a version would bind to the library's oldest definition of a name, which for
//! a few C library functions (`pthread_cond_wait`, `realpath`) is a compatibility shim with
//! different behaviour; recording the version is what makes the binding the one a C toolchain
//! would produce.

use std::collections::{HashMap, HashSet};

use object::read::{Object, ObjectSymbol};
use object::{SymbolKind, SymbolSection};

use super::super::target::{Arch, NativeTarget};
use super::elf::{symbol_name, Elf};
use super::libraries::{ExportKind, ImportLibrary};
use super::reachability::reachable_globals;
use super::relocate::split_hi_lo;
use super::ProgramLinkError;

/// Bytes per stub, on every architecture: four instructions, or one jump and padding.
const STUB_SIZE: u64 = 16;
const SYMBOL_SIZE: u64 = 24;
const RELA_SIZE: u64 = 24;
const DYNAMIC_ENTRY_SIZE: u64 = 16;

/// One function the program calls in a shared library.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Import {
    pub(super) name: String,
    /// Index into [`Imports::needed`].
    library: usize,
    version: Option<String>,
}

/// Everything the program takes from shared libraries.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Imports {
    /// The libraries at least one import comes from, by `DT_SONAME`, in the order given. A library
    /// nothing is imported from is not recorded, so the loader does not load it.
    pub(super) needed: Vec<String>,
    pub(super) functions: Vec<Import>,
}

/// The runtime's entry for starting a Kotlin thread. It makes the thread with a raw `clone`,
/// which gives the C library no per-thread state for that thread, so C code called from it is
/// unsafe. Starting threads through `pthread_create` when the program imports C is the runtime's
/// switch still to come; until it lands, a program that imports and can reach this entry is
/// refused rather than linked.
const RUNTIME_THREAD_START: &str = "kt_thread_start";

/// Refuse a program that imports from a shared library and can start a runtime thread. The
/// runtime is linked whole, so what counts is whether the thread start is reachable from the
/// entry point, not whether it is defined.
pub(super) fn refuse_clone_threads(
    files: &[Elf],
    imports: &Imports,
    entry: &str,
) -> Result<(), ProgramLinkError> {
    if imports.is_empty() || !reachable_globals(files, entry)?.contains(RUNTIME_THREAD_START) {
        return Ok(());
    }
    Err(ProgramLinkError::Unsupported(format!(
        "the program imports from {} and can start threads with the runtime's `clone`, which \
         gives the C library no state for them; starting threads through `pthread_create` for \
         such a program is not implemented yet",
        imports.needed.join(", ")
    )))
}

impl Imports {
    /// Which symbols the inputs reference but do not define, and which library supplies each.
    ///
    /// The first library (in the order given) that exports a name supplies it, as with a C
    /// toolchain's `-l` order. A name no library exports is left out: it is resolved later like
    /// any other reference, so a weak one becomes null and a strong one is named as undefined.
    pub(super) fn collect(
        files: &[Elf],
        libraries: &[ImportLibrary],
    ) -> Result<Self, ProgramLinkError> {
        let mut defined = HashSet::new();
        for file in files {
            for symbol in file.symbols() {
                if !symbol.is_local() && !symbol.is_undefined() {
                    defined.insert(symbol_name(&symbol)?);
                }
            }
        }
        let mut imports = Self::default();
        let mut seen = HashSet::new();
        let mut needed: HashMap<usize, usize> = HashMap::new();
        for file in files {
            for symbol in file.symbols() {
                if symbol.is_local()
                    || symbol.section() != SymbolSection::Undefined
                    || symbol.kind() == SymbolKind::Tls
                {
                    continue;
                }
                let name = symbol_name(&symbol)?;
                if name.is_empty() || defined.contains(name) || !seen.insert(name) {
                    continue;
                }
                let Some((index, export)) = libraries
                    .iter()
                    .enumerate()
                    .find_map(|(index, library)| Some((index, library.export(name)?)))
                else {
                    continue;
                };
                if export.kind == ExportKind::Data {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "`{name}` is a variable of {}; krusty's linker imports only functions \
                         from shared libraries",
                        libraries[index].soname()
                    )));
                }
                let library = *needed.entry(index).or_insert_with(|| {
                    imports.needed.push(libraries[index].soname().to_string());
                    imports.needed.len() - 1
                });
                imports.functions.push(Import {
                    name: name.to_string(),
                    library,
                    version: export.version.clone(),
                });
            }
        }
        Ok(imports)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }
}

/// Where the dynamic tables of the image are, once laid out.
#[derive(Clone, Copy, Debug)]
pub(super) struct Placement {
    /// The read-only tables' start, in memory and in the file.
    pub(super) tables_vaddr: u64,
    pub(super) tables_offset: u64,
    /// The stubs' start.
    pub(super) stubs_vaddr: u64,
    pub(super) stubs_offset: u64,
    /// `.dynamic` then the offset table, at the start of the writable segment.
    pub(super) writable_vaddr: u64,
    pub(super) writable_offset: u64,
}

/// The dynamic tables for a set of imports, before they have addresses.
///
/// Read-only, in the executable segment after the program headers: the interpreter path, the
/// hash table, the symbol and string tables, the version tables, and the relocations that fill the
/// offset table. The stubs follow the program's code. `.dynamic` and the offset table open the
/// writable segment, which the loader writes into (`DT_DEBUG`, and each slot).
pub(super) struct Tables {
    arch: Arch,
    interpreter: &'static str,
    strings: Vec<u8>,
    needed: Vec<u32>,
    /// Each import's name in `strings`, and its `.gnu.version` index.
    symbols: Vec<(u32, u16)>,
    /// `.gnu.version_r`, whose offsets are relative to itself.
    version_needs: Vec<u8>,
    version_need_count: u64,
    // Offsets of each read-only table from the start of the block.
    hash_at: u64,
    symbols_at: u64,
    strings_at: u64,
    versions_at: u64,
    version_needs_at: u64,
    relocations_at: u64,
    read_only_size: u64,
}

fn align_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}

/// The System V ABI's ELF hash, which `vna_hash` carries for each needed version.
fn elf_hash(name: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    for byte in name {
        hash = (hash << 4).wrapping_add(u32::from(*byte));
        let high = hash & 0xf000_0000;
        if high != 0 {
            hash ^= high >> 24;
        }
        hash &= !high;
    }
    hash
}

impl Tables {
    pub(super) fn new(imports: &Imports, target: NativeTarget) -> Self {
        let mut strings = vec![0u8];
        let mut intern = |text: &str| {
            let at = strings.len() as u32;
            strings.extend_from_slice(text.as_bytes());
            strings.push(0);
            at
        };
        let needed: Vec<u32> = imports.needed.iter().map(|name| intern(name)).collect();
        // Version indices 0 and 1 are reserved (local, unversioned global); needed versions are
        // numbered from 2 in the order they are first used.
        let mut versions: Vec<(usize, String, u16)> = Vec::new();
        let mut symbols = Vec::with_capacity(imports.functions.len());
        for import in &imports.functions {
            let name = intern(&import.name);
            let index = match &import.version {
                None => 1,
                Some(version) => match versions
                    .iter()
                    .find(|(library, name, _)| *library == import.library && name == version)
                {
                    Some((_, _, index)) => *index,
                    None => {
                        let index = versions.len() as u16 + 2;
                        versions.push((import.library, version.clone(), index));
                        index
                    }
                },
            };
            symbols.push((name, index));
        }
        let mut version_needs = Vec::new();
        let libraries: Vec<usize> = {
            let mut libraries: Vec<usize> = versions.iter().map(|(library, ..)| *library).collect();
            libraries.dedup();
            libraries.sort_unstable();
            libraries.dedup();
            libraries
        };
        for (position, library) in libraries.iter().enumerate() {
            let entries: Vec<&(usize, String, u16)> = versions
                .iter()
                .filter(|(owner, ..)| owner == library)
                .collect();
            let last = position + 1 == libraries.len();
            // Verneed: vn_version, vn_cnt, vn_file, vn_aux, vn_next.
            version_needs.extend_from_slice(&1u16.to_le_bytes());
            version_needs.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            version_needs.extend_from_slice(&needed[*library].to_le_bytes());
            version_needs.extend_from_slice(&16u32.to_le_bytes());
            let next = if last {
                0
            } else {
                16 + 16 * entries.len() as u32
            };
            version_needs.extend_from_slice(&next.to_le_bytes());
            for (entry, (_, version, index)) in entries.iter().enumerate() {
                // Vernaux: vna_hash, vna_flags, vna_other, vna_name, vna_next.
                version_needs.extend_from_slice(&elf_hash(version.as_bytes()).to_le_bytes());
                version_needs.extend_from_slice(&0u16.to_le_bytes());
                version_needs.extend_from_slice(&index.to_le_bytes());
                version_needs.extend_from_slice(&intern(version).to_le_bytes());
                let next: u32 = if entry + 1 == entries.len() { 0 } else { 16 };
                version_needs.extend_from_slice(&next.to_le_bytes());
            }
        }
        let interpreter = target.dynamic_loader();
        let count = symbols.len() as u64 + 1;
        let hash_at = align_up(interpreter.len() as u64 + 1, 8);
        let symbols_at = align_up(hash_at + 4 * (2 + 1 + count), 8);
        let strings_at = symbols_at + SYMBOL_SIZE * count;
        let versions_at = align_up(strings_at + strings.len() as u64, 8);
        let version_needs_at = align_up(versions_at + 2 * count, 8);
        let relocations_at = align_up(version_needs_at + version_needs.len() as u64, 8);
        let read_only_size = relocations_at + RELA_SIZE * (count - 1);
        Self {
            arch: target.arch,
            interpreter,
            strings,
            needed,
            symbols,
            version_needs,
            version_need_count: libraries.len() as u64,
            hash_at,
            symbols_at,
            strings_at,
            versions_at,
            version_needs_at,
            relocations_at,
            read_only_size,
        }
    }

    /// The read-only block's size; it is 8-aligned at its start.
    pub(super) fn read_only_size(&self) -> u64 {
        self.read_only_size
    }

    pub(super) fn interpreter_len(&self) -> u64 {
        self.interpreter.len() as u64 + 1
    }

    pub(super) fn stubs_size(&self) -> u64 {
        STUB_SIZE * self.symbols.len() as u64
    }

    fn dynamic_entries(&self) -> u64 {
        // NEEDED..., HASH, STRTAB, SYMTAB, STRSZ, SYMENT, RELA, RELASZ, RELAENT, FLAGS, FLAGS_1,
        // DEBUG, NULL, and VERSYM, VERNEED, VERNEEDNUM when anything is versioned.
        let versioned = if self.version_need_count > 0 { 3 } else { 0 };
        self.needed.len() as u64 + 12 + versioned
    }

    pub(super) fn dynamic_size(&self) -> u64 {
        DYNAMIC_ENTRY_SIZE * self.dynamic_entries()
    }

    /// `.dynamic` and the offset table; 8-aligned at its start.
    pub(super) fn writable_size(&self) -> u64 {
        self.dynamic_size() + 8 * self.symbols.len() as u64
    }

    /// Where the stub for import `index` is.
    pub(super) fn stub(&self, at: &Placement, index: usize) -> u64 {
        at.stubs_vaddr + STUB_SIZE * index as u64
    }

    fn slot(&self, at: &Placement, index: usize) -> u64 {
        at.writable_vaddr + self.dynamic_size() + 8 * index as u64
    }

    /// Write every table and stub into `image` at `at`.
    pub(super) fn write(&self, image: &mut [u8], at: &Placement) -> Result<(), ProgramLinkError> {
        let mut block = vec![0u8; self.read_only_size as usize];
        let count = self.symbols.len() as u64 + 1;
        let put = |block: &mut Vec<u8>, at: u64, bytes: &[u8]| {
            block[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
        };
        put(&mut block, 0, self.interpreter.as_bytes());
        // An empty hash table: one bucket, holding no chain. The executable exports nothing.
        let mut hash = Vec::new();
        for word in [1u32, count as u32, 0] {
            hash.extend_from_slice(&word.to_le_bytes());
        }
        hash.extend(std::iter::repeat_n(0u8, 4 * count as usize));
        put(&mut block, self.hash_at, &hash);
        for (index, (name, _)) in self.symbols.iter().enumerate() {
            let entry = self.symbols_at + SYMBOL_SIZE * (index as u64 + 1);
            put(&mut block, entry, &name.to_le_bytes());
            // STB_GLOBAL, STT_FUNC; st_other, st_shndx (undefined), st_value and st_size are 0.
            put(&mut block, entry + 4, &[0x12]);
        }
        put(&mut block, self.strings_at, &self.strings);
        for (index, (_, version)) in self.symbols.iter().enumerate() {
            put(
                &mut block,
                self.versions_at + 2 * (index as u64 + 1),
                &version.to_le_bytes(),
            );
        }
        put(&mut block, self.version_needs_at, &self.version_needs);
        let slot_type = match self.arch {
            Arch::X86_64 => object::elf::R_X86_64_GLOB_DAT,
            Arch::Aarch64 => object::elf::R_AARCH64_GLOB_DAT,
            Arch::Riscv64 => object::elf::R_RISCV_64,
        };
        for index in 0..self.symbols.len() {
            let entry = self.relocations_at + RELA_SIZE * index as u64;
            put(&mut block, entry, &self.slot(at, index).to_le_bytes());
            let info = ((index as u64 + 1) << 32) | u64::from(slot_type.0);
            put(&mut block, entry + 8, &info.to_le_bytes());
        }
        let start = at.tables_offset as usize;
        image[start..start + block.len()].copy_from_slice(&block);

        let tables = |offset: u64| at.tables_vaddr + offset;
        let mut dynamic = Vec::new();
        let mut entry = |tag: object::elf::DynamicTag, value: u64| {
            dynamic.extend_from_slice(&(tag.0 as u64).to_le_bytes());
            dynamic.extend_from_slice(&value.to_le_bytes());
        };
        use object::elf::*;
        for name in &self.needed {
            entry(DT_NEEDED, u64::from(*name));
        }
        entry(DT_HASH, tables(self.hash_at));
        entry(DT_STRTAB, tables(self.strings_at));
        entry(DT_SYMTAB, tables(self.symbols_at));
        entry(DT_STRSZ, self.strings.len() as u64);
        entry(DT_SYMENT, SYMBOL_SIZE);
        entry(DT_RELA, tables(self.relocations_at));
        entry(DT_RELASZ, RELA_SIZE * self.symbols.len() as u64);
        entry(DT_RELAENT, RELA_SIZE);
        if self.version_need_count > 0 {
            entry(DT_VERSYM, tables(self.versions_at));
            entry(DT_VERNEED, tables(self.version_needs_at));
            entry(DT_VERNEEDNUM, self.version_need_count);
        }
        entry(DT_FLAGS, DF_BIND_NOW.0);
        entry(DT_FLAGS_1, DF_1_NOW.0);
        entry(DT_DEBUG, 0);
        entry(DT_NULL, 0);
        debug_assert_eq!(dynamic.len() as u64, self.dynamic_size());
        let start = at.writable_offset as usize;
        image[start..start + dynamic.len()].copy_from_slice(&dynamic);

        for index in 0..self.symbols.len() {
            let stub = self.stub(at, index);
            let code = stub_code(self.arch, stub, self.slot(at, index))?;
            let start = (at.stubs_offset + STUB_SIZE * index as u64) as usize;
            image[start..start + code.len()].copy_from_slice(&code);
        }
        Ok(())
    }
}

/// A stub at `stub` that jumps to the address held in `slot`.
fn stub_code(arch: Arch, stub: u64, slot: u64) -> Result<Vec<u8>, ProgramLinkError> {
    let distance = i128::from(slot) - i128::from(stub);
    let out_of_range = || {
        ProgramLinkError::RelocationOutOfRange(format!(
            "the offset table slot at {slot:#x} is out of reach of its stub at {stub:#x}"
        ))
    };
    let mut code = Vec::with_capacity(STUB_SIZE as usize);
    match arch {
        Arch::X86_64 => {
            // jmp *[rip + slot]; then int3 padding.
            let displacement = i32::try_from(distance - 6).map_err(|_| out_of_range())?;
            code.extend_from_slice(&[0xff, 0x25]);
            code.extend_from_slice(&displacement.to_le_bytes());
            code.resize(STUB_SIZE as usize, 0xcc);
        }
        Arch::Aarch64 => {
            // adrp x16, slot; ldr x17, [x16, :lo12:slot]; br x17; nop
            let pages = (i128::from(slot & !0xfff) - i128::from(stub & !0xfff)) >> 12;
            if !(-(1 << 20)..(1 << 20)).contains(&pages) {
                return Err(out_of_range());
            }
            let pages = pages as u32;
            let adrp = 0x9000_0010 | (pages & 3) << 29 | ((pages >> 2) & 0x7_ffff) << 5;
            let ldr = 0xf940_0211 | (((slot & 0xfff) as u32 / 8) << 10);
            for word in [adrp, ldr, 0xd61f_0220, 0xd503_201f] {
                code.extend_from_slice(&word.to_le_bytes());
            }
        }
        Arch::Riscv64 => {
            // auipc t3, %pcrel_hi(slot); ld t3, %pcrel_lo(slot)(t3); jalr t1, t3; nop
            if !(-(1i128 << 31) + 0x800..(1i128 << 31) - 0x800).contains(&distance) {
                return Err(out_of_range());
            }
            let (hi, lo) = split_hi_lo(distance);
            for word in [
                0x0000_0e17 | hi << 12,
                0x000e_3e03 | lo << 20,
                0x000e_0367,
                0x0000_0013,
            ] {
                code.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::super::elf::link;
    use super::super::fixture::{linux, Image, Obj, ProgramHeader};
    use super::super::libraries::{Export, ExportKind, ImportLibrary};
    use super::*;

    fn library(soname: &str, exports: &[(&str, ExportKind, Option<&str>)]) -> ImportLibrary {
        ImportLibrary::new(
            soname.to_string(),
            exports.iter().map(|(name, kind, version)| {
                (
                    name.to_string(),
                    Export {
                        kind: *kind,
                        version: version.map(str::to_string),
                    },
                )
            }),
        )
        .expect("a valid library")
    }

    /// A `_start` that calls `callee` once, by each architecture's call relocation, and the
    /// offset of the call's field in `.text`.
    fn caller(arch: Arch, callee: &str) -> Obj {
        let mut object = Obj::new(arch);
        let (text, r_type, addend) = match arch {
            Arch::X86_64 => (
                object.section(".text", SectionKind::Text, &[0xe8, 0, 0, 0, 0, 0xc3], 16),
                4, // R_X86_64_PLT32 at the rel32 field
                -4,
            ),
            Arch::Aarch64 => (object.text_words(&[0x9400_0000, 0xd65f_03c0]), 283, 0), // CALL26
            // auipc ra, 0; jalr ra, 0(ra); ret
            Arch::Riscv64 => (
                object.text_words(&[0x0000_0097, 0x0000_80e7, 0x0000_8067]),
                19, // R_RISCV_CALL_PLT
                0,
            ),
        };
        object.define("_start", text, 0);
        let target = object.undefined(callee);
        let at = if arch == Arch::X86_64 { 1 } else { 0 };
        object.reloc(text, at, target, addend, r_type);
        object
    }

    /// The `(tag, value)` entries of the image's `PT_DYNAMIC`, up to `DT_NULL`.
    fn dynamic_entries(image: &Image) -> Vec<(i64, u64)> {
        let header = image
            .program_headers()
            .into_iter()
            .find(|header| header.p_type == 2)
            .expect("PT_DYNAMIC");
        let mut entries = Vec::new();
        for index in 0..header.p_filesz / 16 {
            let at = header.p_vaddr + 16 * index;
            let entry = (image.u64(at) as i64, image.u64(at + 8));
            if entry.0 == 0 {
                break;
            }
            entries.push(entry);
        }
        entries
    }

    fn c_string(image: &Image, vaddr: u64) -> String {
        let mut bytes = Vec::new();
        let mut at = vaddr;
        while image.bytes(at, 1)[0] != 0 {
            bytes.push(image.bytes(at, 1)[0]);
            at += 1;
        }
        String::from_utf8(bytes).expect("UTF-8")
    }

    /// One call to a C library function, linked for every target with no library file present:
    /// the image is dynamic, names the target's loader and the library, binds the function at
    /// its default version before the program starts, and the call reaches a stub that jumps
    /// through the function's offset-table slot.
    #[test]
    fn an_imported_function_is_called_through_a_stub_bound_at_its_version() {
        for target in NativeTarget::ALL.iter().copied() {
            let arch = target.arch;
            let object = caller(arch, "puts");
            let libraries = [
                library(
                    "libm.so.6",
                    &[("cos", ExportKind::Function, Some("GLIBC_2.2.5"))],
                ),
                library(
                    "libc.so.6",
                    &[("puts", ExportKind::Function, Some("GLIBC_2.2.5"))],
                ),
            ];
            let bytes = link(&[&object.bytes()], &libraries, target)
                .unwrap_or_else(|error| panic!("{target}: {error}"));
            let image = Image(&bytes);
            let headers = image.program_headers();
            let types: Vec<u32> = headers.iter().map(|header| header.p_type).collect();
            assert_eq!(types, [6, 3, 1, 1, 2, 0x6474_e551], "{target}");
            assert_eq!(
                headers[0],
                ProgramHeader {
                    p_type: 6,
                    p_flags: 4,
                    p_offset: 64,
                    p_vaddr: 0x40_0040,
                    p_paddr: 0x40_0040,
                    p_filesz: 6 * 56,
                    p_memsz: 6 * 56,
                    p_align: 8,
                },
                "{target}"
            );
            let interp = &headers[1];
            assert_eq!(c_string(&image, interp.p_vaddr), target.dynamic_loader());
            assert_eq!(interp.p_filesz, target.dynamic_loader().len() as u64 + 1);

            let entries = dynamic_entries(&image);
            let value = |tag: object::elf::DynamicTag| {
                entries
                    .iter()
                    .find(|(found, _)| *found == tag.0)
                    .map(|(_, value)| *value)
            };
            let strtab = value(object::elf::DT_STRTAB).expect("DT_STRTAB");
            // Only the library something is imported from is needed: libm supplied nothing.
            let needed: Vec<String> = entries
                .iter()
                .filter(|(tag, _)| *tag == object::elf::DT_NEEDED.0)
                .map(|(_, offset)| c_string(&image, strtab + offset))
                .collect();
            assert_eq!(needed, ["libc.so.6"], "{target}");
            assert_eq!(value(object::elf::DT_FLAGS), Some(8), "{target}"); // DF_BIND_NOW
            assert_eq!(value(object::elf::DT_FLAGS_1), Some(1), "{target}"); // DF_1_NOW
            assert_eq!(value(object::elf::DT_VERNEEDNUM), Some(1), "{target}");

            // The one symbol is `puts`, at version index 2, which `.gnu.version_r` names
            // `GLIBC_2.2.5` of `libc.so.6`.
            let symtab = value(object::elf::DT_SYMTAB).expect("DT_SYMTAB");
            assert_eq!(
                c_string(&image, strtab + u64::from(image.u32(symtab + 24))),
                "puts"
            );
            let versym = value(object::elf::DT_VERSYM).expect("DT_VERSYM");
            assert_eq!(image.u16(versym + 2), 2, "{target}");
            let verneed = value(object::elf::DT_VERNEED).expect("DT_VERNEED");
            assert_eq!(image.u16(verneed + 2), 1, "{target}: one version of libc");
            assert_eq!(
                c_string(&image, strtab + u64::from(image.u32(verneed + 4))),
                "libc.so.6"
            );
            let aux = verneed + u64::from(image.u32(verneed + 8));
            assert_eq!(image.u32(aux), elf_hash(b"GLIBC_2.2.5"), "{target}");
            assert_eq!(image.u16(aux + 6), 2, "{target}");
            assert_eq!(
                c_string(&image, strtab + u64::from(image.u32(aux + 8))),
                "GLIBC_2.2.5"
            );

            // The relocation fills the slot after `.dynamic` with `puts`'s address.
            let rela = value(object::elf::DT_RELA).expect("DT_RELA");
            assert_eq!(value(object::elf::DT_RELASZ), Some(24), "{target}");
            let dynamic = headers[4].p_vaddr;
            let slot = dynamic + headers[4].p_filesz;
            assert_eq!(image.u64(rela), slot, "{target}");
            let glob_dat = match arch {
                Arch::X86_64 => 6,
                Arch::Aarch64 => 1025,
                Arch::Riscv64 => 2,
            };
            assert_eq!(image.u64(rela + 8), (1 << 32) | glob_dat, "{target}");

            // The call lands on the stub, and the stub is the jump through that slot.
            let text = image.entry();
            let stub = match arch {
                Arch::X86_64 => (text as i64 + 5 + i64::from(image.u32(text + 1) as i32)) as u64,
                Arch::Aarch64 => {
                    let imm = (image.u32(text) & 0x03ff_ffff) << 6;
                    (text as i64 + (i64::from(imm as i32) >> 4)) as u64
                }
                Arch::Riscv64 => {
                    let hi = i64::from((image.u32(text) & 0xffff_f000) as i32);
                    let lo = i64::from(image.u32(text + 4) as i32 >> 20);
                    (text as i64 + hi + lo) as u64
                }
            };
            assert_eq!(
                image.bytes(stub, 16),
                stub_code(arch, stub, slot).expect("in range"),
                "{target}"
            );
        }
    }

    /// A library that is given but supplies nothing leaves the image exactly the static one.
    #[test]
    fn a_program_that_imports_nothing_stays_static() {
        for target in NativeTarget::ALL.iter().copied() {
            let mut object = Obj::new(target.arch);
            let ret: Vec<u8> = match target.arch {
                Arch::X86_64 => vec![0xc3],
                Arch::Aarch64 => 0xd65f_03c0u32.to_le_bytes().to_vec(),
                Arch::Riscv64 => 0x0000_8067u32.to_le_bytes().to_vec(),
            };
            let text = object.section(".text", SectionKind::Text, &ret, 4);
            object.define("_start", text, 0);
            let libc = library("libc.so.6", &[("puts", ExportKind::Function, None)]);
            let bytes = object.bytes();
            assert_eq!(
                link(&[&bytes], &[libc], target).expect("links"),
                link(&[&bytes], &[], linux(target.arch)).expect("links"),
                "{target}"
            );
        }
    }

    /// A variable in a shared library would need a copy relocation, which the linker does not
    /// make; the refusal names the variable and its library.
    #[test]
    fn an_imported_variable_is_refused() {
        let object = caller(Arch::X86_64, "stdout");
        let libc = library(
            "libc.so.6",
            &[("stdout", ExportKind::Data, Some("GLIBC_2.2.5"))],
        );
        assert_eq!(
            link(&[&object.bytes()], &[libc], linux(Arch::X86_64)).expect_err("refused"),
            ProgramLinkError::Unsupported(
                "`stdout` is a variable of libc.so.6; krusty's linker imports only functions from \
                 shared libraries"
                    .to_string()
            )
        );
    }

    /// A runtime object defining the thread start in a section of its own, as the runtime's
    /// one-section-per-function build does.
    fn thread_runtime() -> Obj {
        let mut runtime = Obj::new(Arch::X86_64);
        let text = runtime.section(".text.kt_thread_start", SectionKind::Text, &[0xc3], 16);
        runtime.define(RUNTIME_THREAD_START, text, 0);
        runtime
    }

    /// A function nothing calls that imports `callee`.
    fn unreached_caller(callee: &str) -> Obj {
        let mut object = Obj::new(Arch::X86_64);
        let text = object.section(
            ".text.unreached",
            SectionKind::Text,
            &[0xe8, 0, 0, 0, 0],
            16,
        );
        object.define("unreached", text, 0);
        let target = object.undefined(callee);
        object.reloc(text, 1, target, -4, 4);
        object
    }

    /// The thread start being linked in is not the thread start being used: a program that
    /// imports and never reaches it links.
    #[test]
    fn an_unreached_thread_start_does_not_stop_an_import() {
        let libz = library("libz.so.1", &[("deflate", ExportKind::Function, None)]);
        let program = caller(Arch::X86_64, "deflate");
        link(
            &[&program.bytes(), &thread_runtime().bytes()],
            &[libz],
            linux(Arch::X86_64),
        )
        .expect("links");
    }

    #[test]
    fn a_program_that_can_start_runtime_threads_cannot_import_yet() {
        let libz = library("libz.so.1", &[("deflate", ExportKind::Function, None)]);
        let program = caller(Arch::X86_64, RUNTIME_THREAD_START);
        assert_eq!(
            link(
                &[
                    &program.bytes(),
                    &unreached_caller("deflate").bytes(),
                    &thread_runtime().bytes()
                ],
                &[libz],
                linux(Arch::X86_64)
            )
            .expect_err("refused"),
            ProgramLinkError::Unsupported(
                "the program imports from libz.so.1 and can start threads with the runtime's \
                 `clone`, which gives the C library no state for them; starting threads through \
                 `pthread_create` for such a program is not implemented yet"
                    .to_string()
            )
        );
    }

    /// The first library that exports a name supplies it, as `-l` order does for a C toolchain.
    #[test]
    fn the_first_library_exporting_a_name_supplies_it() {
        let object = caller(Arch::X86_64, "compress");
        let libraries = [
            library("libz.so.1", &[("compress", ExportKind::Function, None)]),
            library("libother.so", &[("compress", ExportKind::Function, None)]),
        ];
        let objects = [object.bytes()];
        let files: Vec<Elf> = objects
            .iter()
            .map(|bytes| Elf::parse(bytes.as_slice()).expect("parses"))
            .collect();
        let imports = Imports::collect(&files, &libraries).expect("collects");
        assert_eq!(imports.needed, ["libz.so.1"]);
        assert_eq!(
            imports.functions,
            [Import {
                name: "compress".to_string(),
                library: 0,
                version: None,
            }]
        );
    }

    #[test]
    fn the_elf_hash_is_the_system_v_one() {
        // The values GNU ld records in `vna_hash` for these glibc versions.
        assert_eq!(elf_hash(b"GLIBC_2.2.5"), 0x09691a75);
        assert_eq!(elf_hash(b"GLIBC_2.34"), 0x069691b4);
        assert_eq!(elf_hash(b""), 0);
    }

    #[test]
    fn each_stub_jumps_through_its_slot() {
        assert_eq!(
            stub_code(Arch::X86_64, 0x40_1000, 0x40_3000).unwrap(),
            [
                0xff, 0x25, 0xfa, 0x1f, 0, 0, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc,
                0xcc
            ]
        );
        let words = |code: Vec<u8>| -> Vec<u32> {
            code.chunks(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect()
        };
        // adrp x16, 0x412000; ldr x17, [x16, #0x18]; br x17; nop — as llvm-mc encodes them.
        assert_eq!(
            words(stub_code(Arch::Aarch64, 0x40_1000, 0x41_2018).unwrap()),
            [0xb000_0090, 0xf940_0e11, 0xd61f_0220, 0xd503_201f]
        );
        // auipc t3, 0x2; ld t3, -8(t3); jalr t1, t3; nop
        assert_eq!(
            words(stub_code(Arch::Riscv64, 0x40_1000, 0x40_2ff8).unwrap()),
            [0x0000_2e17, 0xff8e_3e03, 0x000e_0367, 0x0000_0013]
        );
    }
}

//! Relocatable ELF objects built in Rust for the linker's tests, so every relocation kind on every
//! architecture is exercised with no C toolchain on the host.
//!
//! The objects are written by `object`'s ELF writer, an encoder independent of the linker's own
//! reading and patching; a test states the bytes it expects the linker to leave behind.

use object::write::{Object, Relocation, SectionId, Symbol, SymbolId, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, FileFlags, RelocationFlags, SectionFlags, SectionKind,
    SymbolFlags, SymbolKind, SymbolScope,
};

use super::super::target::{Arch, NativeTarget, Os};
use super::elf::link_static;
use super::ProgramLinkError;

/// The address the first text section of the first input lands at: the image base plus the ELF
/// header and two program headers (176 bytes, already 16-aligned).
pub(super) const TEXT: u64 = 0x40_00b0;

pub(super) fn linux(arch: Arch) -> NativeTarget {
    NativeTarget::new(arch, Os::Linux)
}

/// Link `objects` for `arch` on Linux, with nothing else: no runtime.
pub(super) fn link(arch: Arch, objects: &[&Obj]) -> Result<Vec<u8>, ProgramLinkError> {
    let bytes: Vec<Vec<u8>> = objects.iter().map(|object| object.bytes()).collect();
    let inputs: Vec<&[u8]> = bytes.iter().map(Vec::as_slice).collect();
    link_static(&inputs, linux(arch))
}

pub(super) fn linked(arch: Arch, objects: &[&Obj]) -> Vec<u8> {
    link(arch, objects).unwrap_or_else(|error| panic!("{arch:?}: link failed: {error}"))
}

/// The link fails with exactly `expected`: its kind and its whole message.
pub(super) fn assert_link_error(
    result: Result<Vec<u8>, ProgramLinkError>,
    expected: ProgramLinkError,
) {
    match result {
        Err(error) => assert_eq!(error, expected),
        Ok(_) => panic!("expected {expected:?}, but the link succeeded"),
    }
}

/// An x86_64 object whose `.text` is `bytes` with `_start` at its first byte.
pub(super) fn x86_64_start(bytes: &[u8]) -> (Obj, SectionId) {
    let mut object = Obj::new(Arch::X86_64);
    let text = object.section(".text", SectionKind::Text, bytes, 16);
    object.define("_start", text, 0);
    (object, text)
}

/// One relocatable object under construction.
pub(super) struct Obj {
    object: Object<'static>,
    /// Sections holding code, so a symbol defined in one is typed as a function.
    code: Vec<SectionId>,
}

impl Obj {
    /// A 64-bit little-endian object for `arch`, which is what the linker accepts, with the
    /// `e_flags` its code really needs. A RISC-V object says double-float, the target's float ABI,
    /// and no more: one that holds compressed instructions says so itself ([`Obj::set_flags`]).
    pub(super) fn new(arch: Arch) -> Self {
        let architecture = match arch {
            Arch::X86_64 => Architecture::X86_64,
            Arch::Aarch64 => Architecture::Aarch64,
            Arch::Riscv64 => Architecture::Riscv64,
        };
        let mut object = Self::of(architecture, Endianness::Little);
        if arch == Arch::Riscv64 {
            object.set_flags(object::elf::EF_RISCV_FLOAT_ABI_DOUBLE.0);
        }
        object
    }

    /// Set the ELF header's `e_flags`.
    pub(super) fn set_flags(&mut self, e_flags: u32) {
        self.object.flags = FileFlags::Elf {
            os_abi: object::elf::ELFOSABI_NONE,
            abi_version: 0,
            e_flags: object::elf::FileFlags(e_flags),
        };
    }

    /// A `.riscv.attributes` section holding `attributes` in one `riscv` subsection, encoded as
    /// the psABI's "Attributes" chapter lays them out: an odd tag carries a string, an even one a
    /// ULEB128 integer. A subsection of another vendor comes first, which the linker must skip.
    pub(super) fn riscv_attributes(&mut self, attributes: &[(u64, Attribute)]) {
        let mut encoded = Vec::new();
        for (tag, value) in attributes {
            uleb128(&mut encoded, *tag);
            match value {
                Attribute::Integer(value) => uleb128(&mut encoded, *value),
                Attribute::String(value) => {
                    encoded.extend_from_slice(value.as_bytes());
                    encoded.push(0);
                }
            }
        }
        let subsection = |vendor: &[u8], attributes: &[u8]| {
            // Tag_File, then its length (tag and length included), then the attributes.
            let mut file = vec![1];
            file.extend_from_slice(&(5 + attributes.len() as u32).to_le_bytes());
            file.extend_from_slice(attributes);
            let mut subsection = (4 + vendor.len() as u32 + 1 + file.len() as u32)
                .to_le_bytes()
                .to_vec();
            subsection.extend_from_slice(vendor);
            subsection.push(0);
            subsection.extend_from_slice(&file);
            subsection
        };
        let mut section = vec![b'A'];
        section.extend(subsection(b"gnu", &[4, 4])); // not the psABI's: stack_align 4 is ignored
        section.extend(subsection(b"riscv", &encoded));
        self.riscv_attributes_raw(&section);
    }

    /// A `.riscv.attributes` section holding exactly `bytes`.
    pub(super) fn riscv_attributes_raw(&mut self, bytes: &[u8]) {
        let id = self.section(".riscv.attributes", SectionKind::Other, bytes, 1);
        self.object.section_mut(id).flags = SectionFlags::Elf {
            sh_type: object::elf::SHT_RISCV_ATTRIBUTES,
            sh_flags: object::elf::SectionFlags(0),
        };
    }

    /// Any object `object` can write, including ones the linker must refuse.
    pub(super) fn of(architecture: Architecture, endian: Endianness) -> Self {
        Self {
            object: Object::new(BinaryFormat::Elf, architecture, endian),
            code: Vec::new(),
        }
    }

    /// A section holding `bytes`, aligned to `align`.
    pub(super) fn section(
        &mut self,
        name: &str,
        kind: SectionKind,
        bytes: &[u8],
        align: u64,
    ) -> SectionId {
        let id = self
            .object
            .add_section(Vec::new(), name.as_bytes().to_vec(), kind);
        self.object.append_section_data(id, bytes, align);
        if kind == SectionKind::Text {
            self.code.push(id);
        }
        id
    }

    /// A `.text` section holding `words`, each a little-endian 32-bit instruction.
    pub(super) fn text_words(&mut self, words: &[u32]) -> SectionId {
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        self.section(".text", SectionKind::Text, &bytes, 4)
    }

    /// A section with no file bytes (`.bss`, `.tbss`) of `size` bytes.
    pub(super) fn uninitialized(
        &mut self,
        name: &str,
        kind: SectionKind,
        size: u64,
        align: u64,
    ) -> SectionId {
        let id = self
            .object
            .add_section(Vec::new(), name.as_bytes().to_vec(), kind);
        self.object.append_section_bss(id, size, align);
        id
    }

    pub(super) fn symbol(&mut self, symbol: Symbol) -> SymbolId {
        self.object.add_symbol(symbol)
    }

    fn named(
        &mut self,
        name: &str,
        section: SymbolSection,
        value: u64,
        scope: SymbolScope,
        weak: bool,
    ) -> SymbolId {
        let kind = match section {
            SymbolSection::Section(id) if self.code.contains(&id) => SymbolKind::Text,
            _ => SymbolKind::Data,
        };
        self.symbol(Symbol {
            name: name.as_bytes().to_vec(),
            value,
            size: 0,
            kind,
            scope,
            weak,
            section,
            flags: SymbolFlags::None,
        })
    }

    /// A global (strong) definition at `offset` in `section`.
    pub(super) fn define(&mut self, name: &str, section: SectionId, offset: u64) -> SymbolId {
        let at = SymbolSection::Section(section);
        self.named(name, at, offset, SymbolScope::Linkage, false)
    }

    /// A weak definition at `offset` in `section`.
    pub(super) fn define_weak(&mut self, name: &str, section: SectionId, offset: u64) -> SymbolId {
        let at = SymbolSection::Section(section);
        self.named(name, at, offset, SymbolScope::Linkage, true)
    }

    /// A local label at `offset` in `section` (RISC-V's `PCREL_LO12` names one).
    pub(super) fn local(&mut self, name: &str, section: SectionId, offset: u64) -> SymbolId {
        let at = SymbolSection::Section(section);
        self.named(name, at, offset, SymbolScope::Compilation, false)
    }

    pub(super) fn undefined(&mut self, name: &str) -> SymbolId {
        let at = SymbolSection::Undefined;
        self.named(name, at, 0, SymbolScope::Linkage, false)
    }

    pub(super) fn undefined_weak(&mut self, name: &str) -> SymbolId {
        let at = SymbolSection::Undefined;
        self.named(name, at, 0, SymbolScope::Linkage, true)
    }

    /// A global whose value is `value` itself (`SHN_ABS`).
    pub(super) fn absolute(&mut self, name: &str, value: u64) -> SymbolId {
        let at = SymbolSection::Absolute;
        self.named(name, at, value, SymbolScope::Linkage, false)
    }

    /// A tentative definition (`SHN_COMMON`), as `-fcommon` C emits for `int x;`.
    pub(super) fn common(&mut self, name: &str, size: u64, align: u64) -> SymbolId {
        self.symbol(Symbol {
            name: name.as_bytes().to_vec(),
            value: align,
            size,
            kind: SymbolKind::Data,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Common,
            flags: SymbolFlags::None,
        })
    }

    /// An ELF `RELA` relocation of type `r_type` at `offset` in `section`.
    pub(super) fn reloc(
        &mut self,
        section: SectionId,
        offset: u64,
        symbol: SymbolId,
        addend: i64,
        r_type: u32,
    ) {
        self.object
            .add_relocation(
                section,
                Relocation {
                    offset,
                    symbol,
                    addend,
                    flags: RelocationFlags::Elf {
                        r_type: object::elf::RelocationType(r_type),
                    },
                },
            )
            .expect("a RELA relocation is always accepted");
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        self.object.write().expect("the fixture object writes")
    }

    /// The object's bytes with every relocation table retyped from `SHT_RELA` to `sh_type`.
    ///
    /// `object` writes `RELA` for every architecture the linker takes, and that is what their
    /// toolchains emit too, so an object with another table is made from one. For `SHT_REL` (9)
    /// each 24-byte entry becomes the 16-byte entry without its addend field, which is a
    /// well-formed `REL` table; any other type only relabels the section.
    pub(super) fn bytes_with_relocation_table(&self, sh_type: u32) -> Vec<u8> {
        let mut bytes = self.bytes();
        let read = |bytes: &[u8], at: usize, width: usize| {
            bytes[at..at + width]
                .iter()
                .rev()
                .fold(0usize, |value, byte| value << 8 | usize::from(*byte))
        };
        let (table, entry_size) = (read(&bytes, 0x28, 8), read(&bytes, 0x3a, 2));
        for index in 0..read(&bytes, 0x3c, 2) {
            let header = table + index * entry_size;
            if read(&bytes, header + 4, 4) != 4 {
                continue; // not SHT_RELA
            }
            bytes[header + 4..header + 8].copy_from_slice(&sh_type.to_le_bytes());
            if sh_type != 9 {
                continue;
            }
            let (start, size) = (
                read(&bytes, header + 0x18, 8),
                read(&bytes, header + 0x20, 8),
            );
            let count = size / 24;
            for entry in 0..count {
                let from = start + entry * 24;
                bytes.copy_within(from..from + 16, start + entry * 16);
            }
            bytes[header + 0x20..header + 0x28].copy_from_slice(&(count as u64 * 16).to_le_bytes());
            bytes[header + 0x38..header + 0x40].copy_from_slice(&16u64.to_le_bytes());
        }
        bytes
    }
}

/// One program header of a linked executable, every field as the file holds it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ProgramHeader {
    pub(super) p_type: u32,
    pub(super) p_flags: u32,
    pub(super) p_offset: u64,
    pub(super) p_vaddr: u64,
    pub(super) p_paddr: u64,
    pub(super) p_filesz: u64,
    pub(super) p_memsz: u64,
    pub(super) p_align: u64,
}

/// One value in a `.riscv.attributes` section.
pub(super) enum Attribute {
    Integer(u64),
    String(&'static str),
}

fn uleb128(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Reading a linked executable back: its two program headers say where each segment is.
pub(super) struct Image<'a>(pub(super) &'a [u8]);

impl Image<'_> {
    fn u16_at_offset(&self, offset: usize) -> u16 {
        u16::from_le_bytes(self.0[offset..offset + 2].try_into().expect("2 bytes"))
    }

    fn u32_at_offset(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.0[offset..offset + 4].try_into().expect("4 bytes"))
    }

    fn u64_at_offset(&self, offset: usize) -> u64 {
        u64::from_le_bytes(self.0[offset..offset + 8].try_into().expect("8 bytes"))
    }

    pub(super) fn machine(&self) -> u16 {
        self.u16_at_offset(18)
    }

    pub(super) fn entry(&self) -> u64 {
        self.u64_at_offset(24)
    }

    /// `e_flags`: the processor-specific flags, which only RISC-V defines among krusty's targets.
    pub(super) fn flags(&self) -> u32 {
        self.u32_at_offset(48)
    }

    /// Every program header, read from where `e_phoff`, `e_phentsize` and `e_phnum` say they are.
    pub(super) fn program_headers(&self) -> Vec<ProgramHeader> {
        let table = self.u64_at_offset(32) as usize;
        let size = usize::from(self.u16_at_offset(54));
        (0..usize::from(self.u16_at_offset(56)))
            .map(|index| {
                let at = table + index * size;
                ProgramHeader {
                    p_type: self.u32_at_offset(at),
                    p_flags: self.u32_at_offset(at + 4),
                    p_offset: self.u64_at_offset(at + 8),
                    p_vaddr: self.u64_at_offset(at + 16),
                    p_paddr: self.u64_at_offset(at + 24),
                    p_filesz: self.u64_at_offset(at + 32),
                    p_memsz: self.u64_at_offset(at + 40),
                    p_align: self.u64_at_offset(at + 48),
                }
            })
            .collect()
    }

    /// `(p_offset, p_vaddr)` of the writable segment: the writable `PT_LOAD`.
    fn data_segment(&self) -> (u64, u64) {
        let data = self
            .program_headers()
            .into_iter()
            .find(|header| header.p_type == 1 && header.p_flags == 6)
            .expect("a writable PT_LOAD");
        (data.p_offset, data.p_vaddr)
    }

    /// Where the writable segment starts in memory.
    pub(super) fn data(&self) -> u64 {
        self.data_segment().1
    }

    fn offset_of(&self, vaddr: u64) -> usize {
        let (data_offset, data_vaddr) = self.data_segment();
        if vaddr >= data_vaddr {
            (data_offset + (vaddr - data_vaddr)) as usize
        } else {
            (vaddr - 0x40_0000) as usize
        }
    }

    pub(super) fn bytes(&self, vaddr: u64, len: usize) -> &[u8] {
        let offset = self.offset_of(vaddr);
        &self.0[offset..offset + len]
    }

    pub(super) fn u16(&self, vaddr: u64) -> u16 {
        u16::from_le_bytes(self.bytes(vaddr, 2).try_into().expect("2 bytes"))
    }

    pub(super) fn u32(&self, vaddr: u64) -> u32 {
        u32::from_le_bytes(self.bytes(vaddr, 4).try_into().expect("4 bytes"))
    }

    pub(super) fn u64(&self, vaddr: u64) -> u64 {
        u64::from_le_bytes(self.bytes(vaddr, 8).try_into().expect("8 bytes"))
    }
}

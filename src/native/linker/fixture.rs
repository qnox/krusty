//! Relocatable ELF objects built in Rust for the linker's tests, so every relocation kind on every
//! architecture is exercised with no C toolchain on the host.
//!
//! The objects are written by `object`'s ELF writer, an encoder independent of the linker's own
//! reading and patching; a test states the bytes it expects the linker to leave behind.

use object::write::{Object, Relocation, SectionId, Symbol, SymbolId, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationFlags, SectionKind, SymbolFlags, SymbolKind,
    SymbolScope,
};

use super::super::target::{Arch, NativeTarget, Os};

/// The address the first text section of the first input lands at: the image base plus the ELF
/// header and two program headers (176 bytes, already 16-aligned).
pub(super) const TEXT: u64 = 0x40_00b0;

pub(super) fn linux(arch: Arch) -> NativeTarget {
    NativeTarget::new(arch, Os::Linux)
}

/// One relocatable object under construction.
pub(super) struct Obj {
    object: Object<'static>,
    /// Sections holding code, so a symbol defined in one is typed as a function.
    code: Vec<SectionId>,
}

impl Obj {
    /// A 64-bit little-endian object for `arch`, which is what the linker accepts.
    pub(super) fn new(arch: Arch) -> Self {
        let architecture = match arch {
            Arch::X86_64 => Architecture::X86_64,
            Arch::Aarch64 => Architecture::Aarch64,
            Arch::Riscv64 => Architecture::Riscv64,
        };
        Self::of(architecture, Endianness::Little)
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
}

/// Reading a linked executable back: its two program headers say where each segment is.
pub(super) struct Image<'a>(pub(super) &'a [u8]);

impl Image<'_> {
    fn u16_at_offset(&self, offset: usize) -> u16 {
        u16::from_le_bytes(self.0[offset..offset + 2].try_into().expect("2 bytes"))
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

    /// `(p_offset, p_vaddr)` of the writable segment.
    fn data_segment(&self) -> (u64, u64) {
        let phdr = 64 + 56;
        (self.u64_at_offset(phdr + 8), self.u64_at_offset(phdr + 16))
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

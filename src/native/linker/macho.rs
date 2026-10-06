//! The iOS simulator image: relocatable Mach-O objects in, an unsigned arm64 `MH_DYLIB` out.
//!
//! The dylib imports `write`, `mmap`, `munmap` and `exit` from `/usr/lib/libSystem.B.dylib` and
//! nothing else. There is no Apple SDK on this path. Internal references are resolved here, because
//! both ends are in the image; an absolute pointer is recorded for dyld to rebase when it slides
//! the image, and a libSystem reference is recorded for dyld to bind. A branch to libSystem cannot
//! reach it, so it goes to a stub in this image that loads the bound pointer.

use std::collections::{BTreeMap, HashMap, HashSet};

use object::macho::{
    ARM64_RELOC_BRANCH26, ARM64_RELOC_GOT_LOAD_PAGE21, ARM64_RELOC_GOT_LOAD_PAGEOFF12,
    ARM64_RELOC_PAGE21, ARM64_RELOC_PAGEOFF12, ARM64_RELOC_UNSIGNED,
};
use object::read::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
use object::{RelocationFlags, SectionIndex, SectionKind, SymbolScope};

use super::super::prebuilt;
use super::super::target::NativeTarget;
use super::ProgramLinkError;

const PAGE: u64 = 0x4000;
const LIBSYSTEM: &[&str] = &["exit", "mmap", "munmap", "write"];
const LOAD_DYLIB: &str = "/usr/lib/libSystem.B.dylib";
const INSTALL_NAME: &str = "@rpath/krusty.dylib";

/// Link `inputs` into an unsigned iOS simulator dylib.
pub(super) fn link_dylib(
    inputs: &[&[u8]],
    target: NativeTarget,
) -> Result<Vec<u8>, ProgramLinkError> {
    if target.os.static_elf() {
        return Err(ProgramLinkError::Unsupported(
            "the Mach-O linker is the iOS simulator's".to_string(),
        ));
    }
    let mut parsed = Vec::with_capacity(inputs.len());
    for (index, bytes) in inputs.iter().enumerate() {
        parsed.push(parse(&format!("input {index}"), bytes)?);
    }
    Image::link(&parsed)
}

/// Global symbols the runtime defines, without a Mach-O leading underscore.
pub(super) fn runtime_symbols(
    target: NativeTarget,
) -> Result<std::collections::HashSet<String>, ProgramLinkError> {
    let mut names = std::collections::HashSet::new();
    for (file, bytes) in prebuilt::runtime_objects(target).unwrap_or_default() {
        let parsed = parse(&format!("runtime object `{file}`"), bytes)?;
        for symbol in &parsed.symbols {
            if matches!(symbol.scope, Scope::Export | Scope::Hidden) {
                names.insert(symbol.canonical.clone());
            }
        }
    }
    Ok(names)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Local,
    Hidden,
    Export,
    Undefined,
}

enum Target {
    Symbol(usize),
    Section(usize),
}

struct Reloc {
    offset: u64,
    kind: RelKind,
    addend: i64,
    target: Target,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RelKind {
    Unsigned,
    Branch26,
    Page21,
    PageOff12,
    GotPage21,
    GotPageOff12,
}

struct InSection {
    segment: String,
    name: String,
    flags: u32,
    align: u64,
    address: u64,
    zerofill: bool,
    bytes: Vec<u8>,
    size: u64,
    relocs: Vec<Reloc>,
}

struct InSymbol {
    name: String,
    canonical: String,
    macho: String,
    section: Option<usize>,
    offset: u64,
    scope: Scope,
    weak: bool,
}

struct ObjectFile {
    sections: Vec<InSection>,
    symbols: Vec<InSymbol>,
}

struct Place {
    out: usize,
    at: u64,
}

struct OutSection {
    segment: &'static str,
    name: String,
    flags: u32,
    align: u64,
    vmaddr: u64,
    fileoff: u64,
    size: u64,
    zerofill: bool,
    bytes: Vec<u8>,
    ordinal: u8,
}

struct Def {
    input: usize,
    section: usize,
    offset: u64,
    scope: Scope,
    weak: bool,
    macho: String,
}

#[derive(Clone, Copy)]
enum Addr {
    Def(usize),
    Local {
        input: usize,
        section: usize,
        offset: u64,
    },
}

#[derive(Hash, PartialEq, Eq)]
enum SlotKey {
    Def(usize),
    Local(usize, usize),
    Import(String),
}

struct Slot {
    key: SlotKey,
    import: Option<String>,
}

struct Bind {
    vm: u64,
    name: String,
    addend: i64,
}

fn parse(what: &str, bytes: &[u8]) -> Result<ObjectFile, ProgramLinkError> {
    if bytes.len() < 32 || bytes[..4] != [0xcf, 0xfa, 0xed, 0xfe] {
        return Err(ProgramLinkError::Parse(format!(
            "{what} is not a 64-bit Mach-O object"
        )));
    }
    let cputype = u32::from_le_bytes(bytes[4..8].try_into().expect("cputype"));
    let filetype = u32::from_le_bytes(bytes[12..16].try_into().expect("filetype"));
    if cputype != 0x0100_000c || filetype != 1 {
        return Err(ProgramLinkError::ForeignObject(format!(
            "{what} is not an arm64 Mach-O relocatable object"
        )));
    }
    let file = object::File::parse(bytes).map_err(|error| {
        ProgramLinkError::Parse(format!("{what} is not a Mach-O object ({error})"))
    })?;
    let mut sections = Vec::new();
    let mut section_at = HashMap::<SectionIndex, usize>::new();
    for section in file.sections() {
        let segment = section
            .segment_name()
            .map_err(|error| ProgramLinkError::Parse(error.to_string()))?
            .unwrap_or("")
            .to_string();
        let name = section
            .name()
            .map_err(|error| ProgramLinkError::Parse(error.to_string()))?
            .to_string();
        if segment != "__TEXT" && segment != "__DATA" {
            return Err(ProgramLinkError::Unsupported(format!(
                "{what} has a section `{segment},{name}`, which the Mach-O linker does not load"
            )));
        }
        let flags = match section.flags() {
            object::SectionFlags::MachO { flags, .. } => flags.0,
            _ => 0,
        };
        let zerofill = section.kind() == SectionKind::UninitializedData;
        if matches!(
            section.kind(),
            SectionKind::Tls | SectionKind::UninitializedTls | SectionKind::TlsVariables
        ) {
            return Err(ProgramLinkError::Unsupported(format!(
                "{what} has thread-local section `{name}`"
            )));
        }
        let data = section
            .data()
            .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
        let size = section.size();
        if !zerofill && data.len() as u64 != size {
            return Err(ProgramLinkError::Parse(format!(
                "{what} section `{name}` is shorter than its header says"
            )));
        }
        let mut relocs = Vec::new();
        for (offset, reloc) in section.relocations() {
            if reloc.subtractor().is_some() {
                return Err(ProgramLinkError::Unsupported(format!(
                    "{what} uses an ARM64_RELOC_SUBTRACTOR, which the Mach-O linker does not apply"
                )));
            }
            let RelocationFlags::MachO {
                r_type, r_length, ..
            } = reloc.flags()
            else {
                return Err(ProgramLinkError::Unsupported(format!(
                    "{what} has a relocation that is not Mach-O"
                )));
            };
            let kind = if r_type == ARM64_RELOC_UNSIGNED {
                if r_length != 3 {
                    return Err(ProgramLinkError::UnsupportedRelocation {
                        arch: super::super::target::Arch::Aarch64,
                        r_type: u32::from(r_type.0),
                    });
                }
                RelKind::Unsigned
            } else if r_type == ARM64_RELOC_BRANCH26 {
                RelKind::Branch26
            } else if r_type == ARM64_RELOC_PAGE21 {
                RelKind::Page21
            } else if r_type == ARM64_RELOC_PAGEOFF12 {
                RelKind::PageOff12
            } else if r_type == ARM64_RELOC_GOT_LOAD_PAGE21 {
                RelKind::GotPage21
            } else if r_type == ARM64_RELOC_GOT_LOAD_PAGEOFF12 {
                RelKind::GotPageOff12
            } else {
                return Err(ProgramLinkError::UnsupportedRelocation {
                    arch: super::super::target::Arch::Aarch64,
                    r_type: u32::from(r_type.0),
                });
            };
            let mut addend = reloc.addend();
            if kind == RelKind::Unsigned && reloc.has_implicit_addend() {
                let at = usize::try_from(offset).unwrap_or(usize::MAX);
                let bytes = data.get(at..at + 8).ok_or_else(|| {
                    ProgramLinkError::Parse(format!(
                        "{what} has an absolute relocation past the end of `{name}`"
                    ))
                })?;
                addend = i64::from_le_bytes(bytes.try_into().expect("eight bytes"));
            }
            let target = match reloc.target() {
                RelocationTarget::Symbol(index) => Target::Symbol(index.0),
                RelocationTarget::Section(index) => {
                    Target::Section(*section_at.get(&index).ok_or_else(|| {
                        ProgramLinkError::Parse(format!(
                            "{what} relocates against a section it does not contain"
                        ))
                    })?)
                }
                RelocationTarget::Absolute => {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "{what} has an absolute relocation target"
                    )));
                }
                _ => {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "{what} has a relocation target the Mach-O linker does not model"
                    )));
                }
            };
            relocs.push(Reloc {
                offset,
                kind,
                addend,
                target,
            });
        }
        section_at.insert(section.index(), sections.len());
        let align = section.align().max(1);
        sections.push(InSection {
            segment,
            name,
            flags,
            align,
            address: section.address(),
            zerofill,
            bytes: if zerofill { Vec::new() } else { data.to_vec() },
            size,
            relocs,
        });
    }
    // A section relocation names a section by the index it had while the relocations were read.
    // Those indices are recorded before later sections exist, so a forward reference would miss.
    // Clang and Cranelift both point relocations at symbols, which is what this checks for.
    for section in &sections {
        for reloc in &section.relocs {
            if let Target::Section(index) = reloc.target {
                if index >= sections.len() {
                    return Err(ProgramLinkError::Parse(format!(
                        "{what} relocates against section {index}, which it does not contain"
                    )));
                }
            }
        }
    }
    let mut symbols = Vec::new();
    for symbol in file.symbols() {
        let name = symbol
            .name()
            .map_err(|error| ProgramLinkError::Parse(error.to_string()))?
            .to_string();
        let scope = match symbol.scope() {
            SymbolScope::Unknown => Scope::Undefined,
            SymbolScope::Compilation => Scope::Local,
            SymbolScope::Linkage => Scope::Hidden,
            SymbolScope::Dynamic => Scope::Export,
        };
        let (section, offset) = if symbol.is_undefined() || name.is_empty() {
            (None, 0)
        } else {
            match symbol.section() {
                object::SymbolSection::Section(index) => {
                    let section = *section_at.get(&index).ok_or_else(|| {
                        ProgramLinkError::Parse(format!(
                            "{what} symbol `{name}` is in a section the object does not have"
                        ))
                    })?;
                    let base = sections[section].address;
                    if symbol.address() < base {
                        return Err(ProgramLinkError::Parse(format!(
                            "{what} symbol `{name}` is before its section"
                        )));
                    }
                    (Some(section), symbol.address() - base)
                }
                object::SymbolSection::Undefined => (None, 0),
                _ => {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "{what} symbol `{name}` is not in a section"
                    )));
                }
            }
        };
        let canonical = name.strip_prefix('_').unwrap_or(&name).to_string();
        let macho = if name.starts_with('_') {
            name.clone()
        } else {
            format!("_{name}")
        };
        symbols.push(InSymbol {
            canonical,
            macho,
            name,
            section,
            offset,
            scope: if symbol.is_undefined() {
                Scope::Undefined
            } else {
                scope
            },
            weak: symbol.is_weak(),
        });
    }
    Ok(ObjectFile { sections, symbols })
}

struct Image {
    outs: Vec<OutSection>,
    places: Vec<Vec<Place>>,
    defs: Vec<Def>,
    by_name: HashMap<String, usize>,
    text_end: u64,
    data_vm: u64,
    data_file: u64,
    data_filesize: u64,
    data_vmsize: u64,
}

impl Image {
    fn link(objects: &[ObjectFile]) -> Result<Vec<u8>, ProgramLinkError> {
        let mut image = Self::layout(objects)?;
        let mut slots: Vec<Slot> = Vec::new();
        let mut slot_at: HashMap<SlotKey, usize> = HashMap::new();
        let mut stubs: Vec<String> = Vec::new();
        let mut stub_at: HashMap<String, usize> = HashMap::new();
        Self::collect_fixups(
            objects,
            &image,
            &mut slots,
            &mut slot_at,
            &mut stubs,
            &mut stub_at,
        )?;
        // Stubs are instructions at the end of `__text`. The GOT is initialized data, so it stays
        // in the file; zerofill sections stay at the end of `__DATA`, past the file bytes.
        let stub_bytes = stubs.len() as u64 * 12;
        let text = image
            .outs
            .iter_mut()
            .find(|section| section.segment == "__TEXT" && section.name == "__text")
            .ok_or_else(|| {
                ProgramLinkError::Unsupported("the Mach-O image has no __text section".to_string())
            })?;
        let stub_at_text = text.size;
        text.bytes.resize(text.bytes.len() + stub_bytes as usize, 0);
        text.size += stub_bytes;
        let got = OutSection {
            segment: "__DATA",
            name: "__got".to_string(),
            flags: 0,
            align: 8,
            vmaddr: 0,
            fileoff: 0,
            size: slots.len() as u64 * 8,
            zerofill: false,
            bytes: vec![0; slots.len() * 8],
            ordinal: 0,
        };
        // Rebuild addresses now that `__text` grew and `__got` exists. Places of the original
        // contributions do not move: stubs are appended, and the GOT is a new section.
        let text_growth = stub_bytes;
        image.finish_layout(got, text_growth)?;
        image.write(objects, &slots, &stubs, stub_at_text, &stub_at)
    }

    fn layout(objects: &[ObjectFile]) -> Result<Self, ProgramLinkError> {
        let mut groups: Vec<OutSection> = Vec::new();
        let mut index_of: HashMap<(String, String), usize> = HashMap::new();
        let mut places = Vec::with_capacity(objects.len());
        for file in objects {
            let mut file_places = Vec::with_capacity(file.sections.len());
            for section in &file.sections {
                let key = (section.segment.clone(), section.name.clone());
                let out = if let Some(out) = index_of.get(&key) {
                    *out
                } else {
                    let out = groups.len();
                    index_of.insert(key, out);
                    groups.push(OutSection {
                        segment: if section.segment == "__TEXT" {
                            "__TEXT"
                        } else {
                            "__DATA"
                        },
                        name: section.name.clone(),
                        flags: section.flags,
                        align: 1,
                        vmaddr: 0,
                        fileoff: 0,
                        size: 0,
                        zerofill: section.zerofill,
                        bytes: Vec::new(),
                        ordinal: 0,
                    });
                    out
                };
                let group = &mut groups[out];
                if group.zerofill != section.zerofill {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "section `{},{}` mixes zerofill and initialized bytes",
                        section.segment, section.name
                    )));
                }
                group.align = group.align.max(section.align);
                let at = align_up(group.size, section.align.max(1));
                if !section.zerofill {
                    group.bytes.resize(at as usize, 0);
                    group.bytes.extend_from_slice(&section.bytes);
                }
                group.size = at + section.size;
                file_places.push(Place { out, at });
            }
            places.push(file_places);
        }
        let mut defs = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        for (input, file) in objects.iter().enumerate() {
            for symbol in &file.symbols {
                if !matches!(symbol.scope, Scope::Export | Scope::Hidden) {
                    continue;
                }
                let Some(section) = symbol.section else {
                    continue;
                };
                let index = defs.len();
                defs.push(Def {
                    input,
                    section,
                    offset: symbol.offset,
                    scope: symbol.scope,
                    weak: symbol.weak,
                    macho: symbol.macho.clone(),
                });
                bind_name(&mut by_name, &defs, symbol.name.clone(), index)?;
                if symbol.canonical != symbol.name {
                    bind_name(&mut by_name, &defs, symbol.canonical.clone(), index)?;
                }
            }
        }
        Ok(Self {
            outs: groups,
            places,
            defs,
            by_name,
            text_end: 0,
            data_vm: 0,
            data_file: 0,
            data_filesize: 0,
            data_vmsize: 0,
        })
    }

    fn finish_layout(
        &mut self,
        got: OutSection,
        _text_growth: u64,
    ) -> Result<(), ProgramLinkError> {
        if got.size != 0 {
            self.outs.push(got);
        }
        // Places store indexes into `outs`. Addresses are assigned in section-rank order, which is
        // not that index order: `__text` must lead `__TEXT`, and the GOT must be file-backed,
        // ahead of zerofill. The indexes stay put.
        let header = self.header_size()?;
        let mut order: Vec<usize> = (0..self.outs.len()).collect();
        order.sort_by_key(|&index| section_rank(self.outs[index].segment, &self.outs[index].name));
        let mut cursor = align_up(header, 16);
        for index in &order {
            let section = &mut self.outs[*index];
            if section.segment != "__TEXT" {
                continue;
            }
            cursor = align_up(cursor, section.align.max(1));
            section.vmaddr = cursor;
            section.fileoff = cursor;
            cursor += section.size;
        }
        let text_vmsize = align_up(cursor, PAGE);
        self.text_end = text_vmsize;
        self.data_vm = text_vmsize;
        self.data_file = text_vmsize;
        let mut init = 0u64;
        let mut tail = 0u64;
        let mut in_bss = false;
        for index in &order {
            let section = &mut self.outs[*index];
            if section.segment != "__DATA" {
                continue;
            }
            if section.zerofill {
                in_bss = true;
                tail = align_up(if tail == 0 { init } else { tail }, section.align.max(1));
                section.vmaddr = self.data_vm + tail;
                section.fileoff = 0;
                tail += section.size;
            } else {
                if in_bss {
                    return Err(ProgramLinkError::Unsupported(
                        "initialized data follows a zerofill section".to_string(),
                    ));
                }
                init = align_up(init, section.align.max(1));
                section.vmaddr = self.data_vm + init;
                section.fileoff = self.data_file + init;
                init += section.size;
                tail = init;
            }
        }
        self.data_filesize = init;
        self.data_vmsize = align_up(tail, PAGE);
        // `n_sect` is the section's position in the load commands. `push_segment` emits `__TEXT`
        // then `__DATA`, each in section-rank order, so the ordinals follow that same walk.
        let mut ordinal = 1u8;
        for segment in ["__TEXT", "__DATA"] {
            let mut indexes: Vec<usize> = (0..self.outs.len())
                .filter(|&index| self.outs[index].segment == segment)
                .collect();
            indexes.sort_by_key(|&index| {
                section_rank(self.outs[index].segment, &self.outs[index].name)
            });
            for index in indexes {
                self.outs[index].ordinal = ordinal;
                ordinal = ordinal.checked_add(1).ok_or_else(|| {
                    ProgramLinkError::Unsupported("more than 255 sections in the dylib".to_string())
                })?;
            }
        }
        Ok(())
    }

    fn header_size(&self) -> Result<u64, ProgramLinkError> {
        let text = self
            .outs
            .iter()
            .filter(|section| section.segment == "__TEXT")
            .count();
        let data = self
            .outs
            .iter()
            .filter(|section| section.segment == "__DATA")
            .count();
        let mut size = 32u64;
        size += segment_cmd_size(text);
        size += segment_cmd_size(data);
        size += segment_cmd_size(0);
        size += dylib_cmd_size(INSTALL_NAME);
        size += 48; // LC_DYLD_INFO_ONLY
        size += 24; // LC_SYMTAB
        size += 80; // LC_DYSYMTAB
        size += dylib_cmd_size(LOAD_DYLIB);
        size += 24; // LC_BUILD_VERSION
        Ok(size)
    }

    fn collect_fixups(
        objects: &[ObjectFile],
        image: &Image,
        slots: &mut Vec<Slot>,
        slot_at: &mut HashMap<SlotKey, usize>,
        stubs: &mut Vec<String>,
        stub_of: &mut HashMap<String, usize>,
    ) -> Result<(), ProgramLinkError> {
        for (input, file) in objects.iter().enumerate() {
            for section in &file.sections {
                for reloc in &section.relocs {
                    let resolved = image.resolve_in(file, input, reloc)?;
                    let got = matches!(reloc.kind, RelKind::GotPage21 | RelKind::GotPageOff12);
                    if got {
                        let key = match &resolved {
                            Resolved::Defined(Addr::Def(index)) => SlotKey::Def(*index),
                            Resolved::Defined(Addr::Local { .. }) => match reloc.target {
                                Target::Symbol(symbol) => SlotKey::Local(input, symbol),
                                Target::Section(_) => {
                                    return Err(ProgramLinkError::Unsupported(
                                        "a GOT relocation against a section".to_string(),
                                    ));
                                }
                            },
                            Resolved::Import { canonical, .. } => {
                                SlotKey::Import(canonical.clone())
                            }
                        };
                        if !slot_at.contains_key(&key) {
                            let import = match &resolved {
                                Resolved::Import { macho, .. } => Some(macho.clone()),
                                Resolved::Defined(_) => None,
                            };
                            slot_at.insert(key_clone(&key), slots.len());
                            slots.push(Slot { key, import });
                        }
                    }
                    if reloc.kind == RelKind::Branch26 {
                        if let Resolved::Import { canonical, macho } = &resolved {
                            if !stub_of.contains_key(canonical) {
                                stub_of.insert(canonical.clone(), stubs.len());
                                stubs.push(canonical.clone());
                            }
                            let key = SlotKey::Import(canonical.clone());
                            if !slot_at.contains_key(&key) {
                                slot_at.insert(key_clone(&key), slots.len());
                                slots.push(Slot {
                                    key,
                                    import: Some(macho.clone()),
                                });
                            }
                        }
                    }
                    if matches!(reloc.kind, RelKind::Page21 | RelKind::PageOff12) {
                        if let Resolved::Import { macho, .. } = &resolved {
                            return Err(ProgramLinkError::Unsupported(format!(
                                "`{macho}` is imported from libSystem, so its address is not known \
                                 until dyld loads the dylib"
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn write(
        &self,
        objects: &[ObjectFile],
        slots: &[Slot],
        stubs: &[String],
        stub_at_text: u64,
        stub_of: &HashMap<String, usize>,
    ) -> Result<Vec<u8>, ProgramLinkError> {
        let linkedit_vm = self.data_vm + self.data_vmsize;
        let linkedit_file = align_up(self.data_file + self.data_filesize, PAGE);
        let mut rebases = Vec::new();
        let mut binds = Vec::new();
        let mut bytes = vec![0u8; linkedit_file as usize];
        for section in &self.outs {
            if section.zerofill {
                continue;
            }
            let at = section.fileoff as usize;
            bytes
                .get_mut(at..at + section.bytes.len())
                .ok_or_else(|| {
                    ProgramLinkError::Unsupported(
                        "a section landed past the end of the dylib".to_string(),
                    )
                })?
                .copy_from_slice(&section.bytes);
        }
        let text = self
            .outs
            .iter()
            .find(|section| section.name == "__text" && section.segment == "__TEXT")
            .expect("__text");
        let stub_base = text.vmaddr + stub_at_text;
        for (input, file) in objects.iter().enumerate() {
            for (section_index, section) in file.sections.iter().enumerate() {
                let place = &self.places[input][section_index];
                let out = &self.outs[place.out];
                if out.zerofill && !section.relocs.is_empty() {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "a zerofill section `{0},{1}` has a relocation",
                        out.segment, out.name
                    )));
                }
                for reloc in &section.relocs {
                    let pc = out.vmaddr + place.at + reloc.offset;
                    let resolved = self.resolve_in(file, input, reloc)?;
                    match reloc.kind {
                        RelKind::Branch26 => {
                            let target = match resolved {
                                Resolved::Defined(addr) => {
                                    self.address(addr).wrapping_add(reloc.addend as u64)
                                }
                                Resolved::Import { canonical, .. } => {
                                    let stub = stub_of[&canonical];
                                    stub_base + stub as u64 * 12
                                }
                            };
                            let insn = read_u32(&bytes, pc);
                            patch_u32(
                                &mut bytes,
                                pc,
                                encode_branch(insn, pc, target).map_err(|why| {
                                    ProgramLinkError::RelocationOutOfRange(format!(
                                        "{why} for a branch"
                                    ))
                                })?,
                            )?;
                        }
                        RelKind::Page21 | RelKind::PageOff12 => {
                            let Resolved::Defined(addr) = resolved else {
                                return Err(ProgramLinkError::Unsupported(
                                    "a page relocation against an import".to_string(),
                                ));
                            };
                            let target = self.address(addr).wrapping_add(reloc.addend as u64);
                            let insn = read_u32(&bytes, pc);
                            let patched = if reloc.kind == RelKind::Page21 {
                                encode_adrp(insn, pc, target)
                            } else {
                                encode_pageoff(insn, target)
                            }
                            .map_err(ProgramLinkError::RelocationOutOfRange)?;
                            patch_u32(&mut bytes, pc, patched)?;
                        }
                        RelKind::GotPage21 | RelKind::GotPageOff12 => {
                            let slot = slot_index(slots, &resolved, input, reloc)?;
                            let got = self.got_addr(slot);
                            let target = got.wrapping_add(reloc.addend as u64);
                            let insn = read_u32(&bytes, pc);
                            let patched = if reloc.kind == RelKind::GotPage21 {
                                encode_adrp(insn, pc, target)
                            } else {
                                encode_pageoff(insn, target)
                            }
                            .map_err(ProgramLinkError::RelocationOutOfRange)?;
                            patch_u32(&mut bytes, pc, patched)?;
                        }
                        RelKind::Unsigned => match resolved {
                            Resolved::Defined(addr) => {
                                let value = self.address(addr).wrapping_add(reloc.addend as u64);
                                patch_u64(&mut bytes, pc, value)?;
                                rebases.push(pc);
                            }
                            Resolved::Import { macho, .. } => {
                                patch_u64(&mut bytes, pc, 0)?;
                                binds.push(Bind {
                                    vm: pc,
                                    name: macho,
                                    addend: reloc.addend,
                                });
                            }
                        },
                    }
                }
            }
        }
        let got_addr = self.got_addr(0);
        for (index, slot) in slots.iter().enumerate() {
            let vm = got_addr + index as u64 * 8;
            match &slot.import {
                Some(name) => binds.push(Bind {
                    vm,
                    name: name.clone(),
                    addend: 0,
                }),
                None => {
                    let value = match &slot.key {
                        SlotKey::Def(index) => self.address(Addr::Def(*index)),
                        SlotKey::Local(input, symbol) => {
                            let symbol = &objects[*input].symbols[*symbol];
                            let section = symbol.section.ok_or_else(|| {
                                ProgramLinkError::Unsupported(
                                    "a GOT slot for a symbol that has no section".to_string(),
                                )
                            })?;
                            self.address(Addr::Local {
                                input: *input,
                                section,
                                offset: symbol.offset,
                            })
                        }
                        SlotKey::Import(_) => unreachable!("an import slot has a name"),
                    };
                    patch_u64(&mut bytes, vm, value)?;
                    rebases.push(vm);
                }
            }
        }
        for (index, canonical) in stubs.iter().enumerate() {
            let pc = stub_base + index as u64 * 12;
            let slot = slots
                .iter()
                .position(|slot| matches!(&slot.key, SlotKey::Import(name) if name == canonical))
                .ok_or_else(|| {
                    ProgramLinkError::Unsupported(format!("no GOT slot for stub `{canonical}`"))
                })?;
            let got = self.got_addr(slot);
            let adrp = encode_adrp(0x9000_0010, pc, got)
                .map_err(ProgramLinkError::RelocationOutOfRange)?;
            let ldr =
                encode_pageoff(0xF940_0210, got).map_err(ProgramLinkError::RelocationOutOfRange)?;
            patch_u32(&mut bytes, pc, adrp)?;
            patch_u32(&mut bytes, pc + 4, ldr)?;
            patch_u32(&mut bytes, pc + 8, 0xD61F_0200)?;
        }
        let linked = self.linkedit(&rebases, &binds)?;
        let mut linkedit = Vec::new();
        let rebase_off = linkedit_file + linkedit.len() as u64;
        linkedit.extend_from_slice(&linked.rebase);
        let bind_off = linkedit_file + linkedit.len() as u64;
        linkedit.extend_from_slice(&linked.bind);
        let export_off = linkedit_file + linkedit.len() as u64;
        linkedit.extend_from_slice(&linked.export);
        let symoff = linkedit_file + align_up(linkedit.len() as u64, 8);
        linkedit.resize(symoff as usize - linkedit_file as usize, 0);
        linkedit.extend_from_slice(&linked.symtab);
        let stroff = linkedit_file + linkedit.len() as u64;
        linkedit.extend_from_slice(&linked.strings);
        let linkedit_size = linkedit.len() as u64;
        bytes.resize(linkedit_file as usize, 0);
        bytes.extend_from_slice(&linkedit);
        let header = self.header(&LinkInfo {
            vm: linkedit_vm,
            file: linkedit_file,
            vmsize: align_up(linkedit_size, PAGE),
            filesize: linkedit_size,
            rebase_off,
            rebase_size: linked.rebase.len() as u32,
            bind_off,
            bind_size: linked.bind.len() as u32,
            export_off,
            export_size: linked.export.len() as u32,
            symoff,
            nsyms: (linked.symtab.len() / 16) as u32,
            nextdef: linked.defined,
            stroff,
            strsize: linked.strings.len() as u32,
        })?;
        let expected = self.header_size()?;
        if header.len() as u64 != expected {
            return Err(ProgramLinkError::Unsupported(format!(
                "the Mach-O header is {} bytes and the layout reserved {expected}",
                header.len()
            )));
        }
        let first = self
            .outs
            .iter()
            .filter(|section| !section.zerofill)
            .map(|section| section.fileoff)
            .min()
            .unwrap_or(expected);
        if header.len() as u64 > first {
            return Err(ProgramLinkError::Unsupported(
                "the Mach-O header overlaps the first section".to_string(),
            ));
        }
        bytes[..header.len()].copy_from_slice(&header);
        Ok(bytes)
    }

    fn resolve_in(
        &self,
        file: &ObjectFile,
        input: usize,
        reloc: &Reloc,
    ) -> Result<Resolved, ProgramLinkError> {
        match reloc.target {
            Target::Symbol(index) => self.resolve_symbol_in(file, input, index),
            Target::Section(section) => Ok(Resolved::Defined(Addr::Local {
                input,
                section,
                offset: 0,
            })),
        }
    }

    fn resolve_symbol_in(
        &self,
        file: &ObjectFile,
        input: usize,
        index: usize,
    ) -> Result<Resolved, ProgramLinkError> {
        let symbol = file
            .symbols
            .get(index)
            .ok_or_else(|| ProgramLinkError::Parse(format!("a relocation names symbol {index}")))?;
        if symbol.scope == Scope::Local {
            if let Some(section) = symbol.section {
                return Ok(Resolved::Defined(Addr::Local {
                    input,
                    section,
                    offset: symbol.offset,
                }));
            }
        }
        if let Some(&def) = self
            .by_name
            .get(&symbol.name)
            .or_else(|| self.by_name.get(&symbol.canonical))
        {
            return Ok(Resolved::Defined(Addr::Def(def)));
        }
        if let Some(section) = symbol.section {
            if symbol.scope != Scope::Undefined {
                return Ok(Resolved::Defined(Addr::Local {
                    input,
                    section,
                    offset: symbol.offset,
                }));
            }
        }
        if LIBSYSTEM.contains(&symbol.canonical.as_str()) {
            return Ok(Resolved::Import {
                canonical: symbol.canonical.clone(),
                macho: symbol.macho.clone(),
            });
        }
        Err(ProgramLinkError::UndefinedSymbol(symbol.canonical.clone()))
    }

    fn address(&self, addr: Addr) -> u64 {
        match addr {
            Addr::Def(index) => {
                let def = &self.defs[index];
                let place = &self.places[def.input][def.section];
                self.outs[place.out].vmaddr + place.at + def.offset
            }
            Addr::Local {
                input,
                section,
                offset,
            } => {
                let place = &self.places[input][section];
                self.outs[place.out].vmaddr + place.at + offset
            }
        }
    }

    fn got_addr(&self, slot: usize) -> u64 {
        self.outs
            .iter()
            .find(|section| section.name == "__got")
            .map(|section| section.vmaddr + slot as u64 * 8)
            .unwrap_or(0)
    }

    fn linkedit(&self, rebases: &[u64], binds: &[Bind]) -> Result<LinkEdit, ProgramLinkError> {
        let rebase = rebase_opcodes(rebases, self.data_vm);
        let bind = bind_opcodes(binds, self.data_vm);
        // A weak definition that lost to a strong one stays in `defs` but is no longer the name's
        // binding. The symbol table and the export trie name the binding.
        let live: HashSet<usize> = self.by_name.values().copied().collect();
        let mut defined: Vec<(usize, &Def)> = self
            .defs
            .iter()
            .enumerate()
            .filter(|(index, _)| live.contains(index))
            .collect();
        defined.sort_by(|(_, one), (_, other)| one.macho.cmp(&other.macho));
        defined.sort_by_key(|(_, def)| def.scope != Scope::Export);
        let mut imports: Vec<&str> = binds.iter().map(|bind| bind.name.as_str()).collect();
        imports.sort_unstable();
        imports.dedup();
        let mut exports = Vec::new();
        for (index, def) in &defined {
            if def.scope == Scope::Export {
                exports.push((def.macho.clone(), self.address(Addr::Def(*index))));
            }
        }
        let export = export_trie(&exports)?;
        let mut strings = vec![0u8];
        let mut symtab = Vec::new();
        for (index, def) in &defined {
            let strx = strings.len() as u32;
            strings.extend_from_slice(def.macho.as_bytes());
            strings.push(0);
            let place = &self.places[def.input][def.section];
            let n_type = if def.scope == Scope::Hidden {
                0x1f
            } else {
                0x0f
            };
            let n_desc: u16 = if def.weak { 0x80 } else { 0 };
            push_nlist(
                &mut symtab,
                strx,
                n_type,
                self.outs[place.out].ordinal,
                n_desc,
                self.address(Addr::Def(*index)),
            );
        }
        for name in imports {
            let strx = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            push_nlist(&mut symtab, strx, 0x01, 0, 0, 0);
        }
        Ok(LinkEdit {
            rebase,
            bind,
            export,
            symtab,
            strings,
            defined: defined.len() as u32,
        })
    }

    fn header(&self, info: &LinkInfo) -> Result<Vec<u8>, ProgramLinkError> {
        let mut header = Vec::new();
        push_u32(&mut header, 0xfeed_facf);
        push_u32(&mut header, 0x0100_000c);
        push_u32(&mut header, 0);
        push_u32(&mut header, 6); // MH_DYLIB
        let ncmds_at = header.len();
        push_u32(&mut header, 0);
        let sizeofcmds_at = header.len();
        push_u32(&mut header, 0);
        push_u32(&mut header, 0x4 | 0x80 | 0x10_0000);
        push_u32(&mut header, 0);
        let mut ncmds = 0u32;
        ncmds += 1;
        push_segment(
            &mut header,
            &Segment {
                name: "__TEXT",
                vmaddr: 0,
                vmsize: self.text_end,
                fileoff: 0,
                filesize: self.text_end,
                initprot: 5,
            },
            &self.outs,
        )?;
        ncmds += 1;
        push_segment(
            &mut header,
            &Segment {
                name: "__DATA",
                vmaddr: self.data_vm,
                vmsize: self.data_vmsize,
                fileoff: self.data_file,
                filesize: self.data_filesize,
                initprot: 3,
            },
            &self.outs,
        )?;
        ncmds += 1;
        push_segment(
            &mut header,
            &Segment {
                name: "__LINKEDIT",
                vmaddr: info.vm,
                vmsize: info.vmsize,
                fileoff: info.file,
                filesize: info.filesize,
                initprot: 1,
            },
            &[],
        )?;
        ncmds += 1;
        push_dylib(&mut header, 0xd, INSTALL_NAME);
        ncmds += 1;
        push_u32(&mut header, 0x8000_0022);
        push_u32(&mut header, 48);
        push_u32(&mut header, info.rebase_off as u32);
        push_u32(&mut header, info.rebase_size);
        push_u32(&mut header, info.bind_off as u32);
        push_u32(&mut header, info.bind_size);
        push_u32(&mut header, 0);
        push_u32(&mut header, 0);
        push_u32(&mut header, 0);
        push_u32(&mut header, 0);
        push_u32(&mut header, info.export_off as u32);
        push_u32(&mut header, info.export_size);
        ncmds += 1;
        let nundef = info.nsyms - info.nextdef;
        push_u32(&mut header, 0x2);
        push_u32(&mut header, 24);
        push_u32(&mut header, info.symoff as u32);
        push_u32(&mut header, info.nsyms);
        push_u32(&mut header, info.stroff as u32);
        push_u32(&mut header, info.strsize);
        ncmds += 1;
        push_u32(&mut header, 0xb);
        push_u32(&mut header, 80);
        push_u32(&mut header, 0); // ilocalsym
        push_u32(&mut header, 0);
        push_u32(&mut header, 0); // iextdefsym
        push_u32(&mut header, info.nextdef);
        push_u32(&mut header, info.nextdef);
        push_u32(&mut header, nundef);
        for _ in 0..12 {
            push_u32(&mut header, 0);
        }
        ncmds += 1;
        push_dylib(&mut header, 0xc, LOAD_DYLIB);
        ncmds += 1;
        push_u32(&mut header, 0x32);
        push_u32(&mut header, 24);
        push_u32(&mut header, 7); // PLATFORM_IOSSIMULATOR
        push_u32(&mut header, 15 << 16); // 15.0.0
        push_u32(&mut header, 15 << 16);
        push_u32(&mut header, 0);
        let sizeofcmds = (header.len() - 32) as u32;
        header[ncmds_at..ncmds_at + 4].copy_from_slice(&ncmds.to_le_bytes());
        header[sizeofcmds_at..sizeofcmds_at + 4].copy_from_slice(&sizeofcmds.to_le_bytes());
        Ok(header)
    }
}

/// The linkedit blobs, in the order they are written, plus how many defined symbols lead the table.
struct LinkEdit {
    rebase: Vec<u8>,
    bind: Vec<u8>,
    export: Vec<u8>,
    symtab: Vec<u8>,
    strings: Vec<u8>,
    defined: u32,
}

/// Where linkedit lives, and the pieces written into it. Offsets are file offsets.
struct LinkInfo {
    vm: u64,
    file: u64,
    vmsize: u64,
    filesize: u64,
    rebase_off: u64,
    rebase_size: u32,
    bind_off: u64,
    bind_size: u32,
    export_off: u64,
    export_size: u32,
    symoff: u64,
    nsyms: u32,
    nextdef: u32,
    stroff: u64,
    strsize: u32,
}

/// One `LC_SEGMENT_64`: its address range and the protection dyld gives it.
struct Segment {
    name: &'static str,
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    initprot: u32,
}

enum Resolved {
    Defined(Addr),
    Import { canonical: String, macho: String },
}

fn key_clone(key: &SlotKey) -> SlotKey {
    match key {
        SlotKey::Def(index) => SlotKey::Def(*index),
        SlotKey::Local(input, symbol) => SlotKey::Local(*input, *symbol),
        SlotKey::Import(name) => SlotKey::Import(name.clone()),
    }
}

fn slot_index(
    slots: &[Slot],
    resolved: &Resolved,
    input: usize,
    reloc: &Reloc,
) -> Result<usize, ProgramLinkError> {
    let key = match resolved {
        Resolved::Defined(Addr::Def(index)) => SlotKey::Def(*index),
        Resolved::Defined(Addr::Local { .. }) => match reloc.target {
            Target::Symbol(symbol) => SlotKey::Local(input, symbol),
            Target::Section(_) => {
                return Err(ProgramLinkError::Unsupported(
                    "a GOT relocation against a section".to_string(),
                ));
            }
        },
        Resolved::Import { canonical, .. } => SlotKey::Import(canonical.clone()),
    };
    slots
        .iter()
        .position(|slot| slot.key == key)
        .ok_or_else(|| ProgramLinkError::Unsupported("a GOT relocation has no slot".to_string()))
}

fn bind_name(
    map: &mut HashMap<String, usize>,
    defs: &[Def],
    key: String,
    index: usize,
) -> Result<(), ProgramLinkError> {
    if let Some(&old) = map.get(&key) {
        if old == index {
            return Ok(());
        }
        match (defs[old].weak, defs[index].weak) {
            (false, true) => return Ok(()),
            (true, false) => {
                for value in map.values_mut() {
                    if *value == old {
                        *value = index;
                    }
                }
                return Ok(());
            }
            _ => {
                return Err(ProgramLinkError::DuplicateSymbol(key));
            }
        }
    }
    map.insert(key, index);
    Ok(())
}

fn section_rank(segment: &str, name: &str) -> u8 {
    match (segment, name) {
        ("__TEXT", "__text") => 0,
        ("__TEXT", "__literal8") => 1,
        ("__TEXT", "__literal16") => 2,
        ("__TEXT", "__cstring") => 3,
        ("__TEXT", "__const") => 4,
        ("__DATA", "__data") => 10,
        ("__DATA", "__const") => 11,
        ("__DATA", "__got") => 12,
        ("__DATA", "__common") => 20,
        ("__DATA", "__bss") => 21,
        ("__TEXT", _) => 5,
        ("__DATA", _) => 13,
        _ => 30,
    }
}

fn align_up(value: u64, align: u64) -> u64 {
    let align = align.max(1);
    value.div_ceil(align) * align
}

fn segment_cmd_size(nsects: usize) -> u64 {
    (72 + 80 * nsects) as u64
}

fn dylib_cmd_size(name: &str) -> u64 {
    let raw = 24 + name.len() + 1;
    ((raw + 7) & !7) as u64
}

fn fits_signed(value: i64, bits: u32) -> bool {
    let shift = 64 - bits;
    (value << shift) >> shift == value
}

fn encode_branch(insn: u32, pc: u64, target: u64) -> Result<u32, String> {
    let delta = target as i64 - pc as i64;
    if delta % 4 != 0 {
        return Err(format!(
            "branch from {pc:#x} to {target:#x} is not 4-byte aligned"
        ));
    }
    let imm = delta >> 2;
    if !fits_signed(imm, 26) {
        return Err(format!(
            "branch from {pc:#x} to {target:#x} does not fit in 26 bits"
        ));
    }
    Ok((insn & 0xFC00_0000) | ((imm as u32) & 0x03FF_FFFF))
}

fn encode_adrp(insn: u32, pc: u64, target: u64) -> Result<u32, String> {
    let page_delta = (target & !0xfff) as i64 - (pc & !0xfff) as i64;
    let imm = page_delta >> 12;
    if !fits_signed(imm, 21) {
        return Err(format!(
            "adrp from {pc:#x} to {target:#x} does not fit in 21 bits"
        ));
    }
    let imm_u = imm as u32;
    let immlo = imm_u & 0x3;
    let immhi = (imm_u >> 2) & 0x7_ffff;
    Ok((insn & 0x9F00_001F) | (immlo << 29) | (immhi << 5))
}

fn encode_pageoff(insn: u32, target: u64) -> Result<u32, String> {
    let low = (target & 0xfff) as u32;
    if insn & 0x3B00_0000 == 0x3900_0000 {
        let mut scale = insn >> 30;
        if scale == 0 && insn & 0x0480_0000 == 0x0480_0000 {
            scale = 4;
        }
        if scale < 32 && low & ((1 << scale) - 1) != 0 {
            return Err(format!(
                "page offset of {target:#x} is not aligned to a {scale}-byte access"
            ));
        }
        let imm = low >> scale;
        Ok((insn & 0xFFC0_03FF) | (imm << 10))
    } else {
        Ok((insn & 0xFFC0_03FF) | (low << 10))
    }
}

fn read_u32(bytes: &[u8], vm: u64) -> u32 {
    let at = vm as usize;
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("instruction"))
}

fn patch_u32(bytes: &mut [u8], vm: u64, value: u32) -> Result<(), ProgramLinkError> {
    let at = usize::try_from(vm).unwrap_or(usize::MAX);
    let slot = bytes.get_mut(at..at + 4).ok_or_else(|| {
        ProgramLinkError::Unsupported(format!("a patch at {vm:#x} is outside the dylib"))
    })?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn patch_u64(bytes: &mut [u8], vm: u64, value: u64) -> Result<(), ProgramLinkError> {
    let at = usize::try_from(vm).unwrap_or(usize::MAX);
    let slot = bytes.get_mut(at..at + 8).ok_or_else(|| {
        ProgramLinkError::Unsupported(format!("a pointer at {vm:#x} is outside the dylib"))
    })?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_name(out: &mut Vec<u8>, name: &str) {
    let mut bytes = [0u8; 16];
    let take = name.len().min(16);
    bytes[..take].copy_from_slice(&name.as_bytes()[..take]);
    out.extend_from_slice(&bytes);
}

fn push_segment(
    out: &mut Vec<u8>,
    segment: &Segment,
    sections: &[OutSection],
) -> Result<(), ProgramLinkError> {
    let mut mine: Vec<&OutSection> = sections
        .iter()
        .filter(|section| section.segment == segment.name)
        .collect();
    mine.sort_by_key(|section| section_rank(section.segment, &section.name));
    let cmdsize = 72 + 80 * mine.len();
    push_u32(out, 0x19);
    push_u32(out, cmdsize as u32);
    push_name(out, segment.name);
    push_u64(out, segment.vmaddr);
    push_u64(out, segment.vmsize);
    push_u64(out, segment.fileoff);
    push_u64(out, segment.filesize);
    push_u32(out, 7);
    push_u32(out, segment.initprot);
    push_u32(out, mine.len() as u32);
    push_u32(out, 0);
    for section in mine {
        push_name(out, &section.name);
        push_name(out, segment.name);
        push_u64(out, section.vmaddr);
        push_u64(out, section.size);
        push_u32(
            out,
            if section.zerofill {
                0
            } else {
                section.fileoff as u32
            },
        );
        let align = section.align.max(1).trailing_zeros();
        push_u32(out, align);
        push_u32(out, 0);
        push_u32(out, 0);
        push_u32(out, section.flags);
        push_u32(out, 0);
        push_u32(out, 0);
        push_u32(out, 0);
    }
    Ok(())
}

fn push_dylib(out: &mut Vec<u8>, cmd: u32, name: &str) {
    let size = dylib_cmd_size(name) as u32;
    push_u32(out, cmd);
    push_u32(out, size);
    push_u32(out, 24);
    push_u32(out, 0);
    push_u32(out, 0x1_0000);
    push_u32(out, 0x1_0000);
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

fn push_nlist(out: &mut Vec<u8>, strx: u32, n_type: u8, n_sect: u8, n_desc: u16, value: u64) {
    push_u32(out, strx);
    out.push(n_type);
    out.push(n_sect);
    out.extend_from_slice(&n_desc.to_le_bytes());
    push_u64(out, value);
}

fn uleb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn sleb(out: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

fn rebase_opcodes(sites: &[u64], data_vm: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x10 | 1); // SET_TYPE_IMM | REBASE_TYPE_POINTER
    for &vm in sites {
        let (segment, offset) = if vm < data_vm {
            (0u8, vm)
        } else {
            (1, vm - data_vm)
        };
        out.push(0x20 | segment);
        uleb(&mut out, offset);
        out.push(0x50 | 1); // DO_REBASE_IMM_TIMES, once
    }
    out.push(0);
    out
}

fn bind_opcodes(binds: &[Bind], data_vm: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x10 | 1); // SET_DYLIB_ORDINAL_IMM, libSystem is ordinal 1
    out.push(0x50 | 1); // SET_TYPE_IMM | BIND_TYPE_POINTER
    for bind in binds {
        out.push(0x40); // SET_SYMBOL_TRAILING_FLAGS_IMM, flags 0
        out.extend_from_slice(bind.name.as_bytes());
        out.push(0);
        out.push(0x60); // SET_ADDEND_SLEB
        sleb(&mut out, bind.addend);
        let (segment, offset) = if bind.vm < data_vm {
            (0u8, bind.vm)
        } else {
            (1, bind.vm - data_vm)
        };
        out.push(0x70 | segment);
        uleb(&mut out, offset);
        out.push(0x90); // DO_BIND
    }
    out.push(0);
    out
}

struct TrieNode {
    address: Option<u64>,
    children: BTreeMap<u8, TrieNode>,
}

struct FlatNode {
    address: Option<u64>,
    edges: Vec<(u8, usize)>,
}

/// A dyld export trie. Each edge is one byte, so a node stays under the format's 255-child limit
/// for the symbol names this image exports. The address in a terminal is the symbol's vm address.
fn export_trie(exports: &[(String, u64)]) -> Result<Vec<u8>, ProgramLinkError> {
    let mut root = TrieNode {
        address: None,
        children: BTreeMap::new(),
    };
    for (name, address) in exports {
        if name.is_empty() {
            return Err(ProgramLinkError::Unsupported(
                "an exported symbol has an empty name".to_string(),
            ));
        }
        let mut node = &mut root;
        for byte in name.bytes() {
            node = node.children.entry(byte).or_insert_with(|| TrieNode {
                address: None,
                children: BTreeMap::new(),
            });
        }
        if node.address.is_some() {
            return Err(ProgramLinkError::DuplicateSymbol(name.clone()));
        }
        node.address = Some(*address);
    }
    let mut flat = Vec::new();
    flatten_trie(root, &mut flat);
    for node in &flat {
        if node.edges.len() > 255 {
            return Err(ProgramLinkError::Unsupported(
                "an export-trie node has more than 255 edges".to_string(),
            ));
        }
    }
    let mut widths = vec![1usize; flat.len()];
    let positions = loop {
        let mut cursor = 0u64;
        let mut pos = vec![0u64; flat.len()];
        for (index, node) in flat.iter().enumerate() {
            pos[index] = cursor;
            cursor += trie_node_len(node, &widths) as u64;
        }
        let mut next = vec![1usize; flat.len()];
        for node in &flat {
            for (_, child) in &node.edges {
                next[*child] = uleb_len(pos[*child]);
            }
        }
        if next == widths {
            break pos;
        }
        widths = next;
    };
    let mut out = Vec::new();
    for (index, node) in flat.iter().enumerate() {
        debug_assert_eq!(out.len() as u64, positions[index]);
        let mut payload = Vec::new();
        if let Some(address) = node.address {
            uleb(&mut payload, 0);
            uleb(&mut payload, address);
        }
        uleb(&mut out, payload.len() as u64);
        out.extend_from_slice(&payload);
        out.push(node.edges.len() as u8);
        for (byte, child) in &node.edges {
            out.push(*byte);
            out.push(0);
            uleb(&mut out, positions[*child]);
        }
    }
    Ok(out)
}

fn flatten_trie(node: TrieNode, out: &mut Vec<FlatNode>) {
    let index = out.len();
    out.push(FlatNode {
        address: node.address,
        edges: Vec::new(),
    });
    let mut edges = Vec::new();
    for (byte, child) in node.children {
        let child_index = out.len();
        flatten_trie(child, out);
        edges.push((byte, child_index));
    }
    out[index].edges = edges;
}

fn trie_node_len(node: &FlatNode, widths: &[usize]) -> usize {
    let payload = node
        .address
        .map(|address| uleb_len(0) + uleb_len(address))
        .unwrap_or(0);
    let mut len = uleb_len(payload as u64) + payload + 1;
    for (_, child) in &node.edges {
        len += 2 + widths[*child];
    }
    len
}

fn uleb_len(mut value: u64) -> usize {
    let mut len = 1;
    while value > 0x7f {
        value >>= 7;
        len += 1;
    }
    len
}

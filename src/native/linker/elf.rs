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
//! read-only segment is cheap to add when the runtime grows something worth protecting. The
//! decisions are recorded in `docs/IMPLEMENTATION_PLAN.md` ("Native linker").
//!
//! Every input is untrusted in the sense that matters here: krusty's own code generator produces
//! some of them, and a bug there must surface as an error naming the object, never as a panic or a
//! write outside the section a relocation belongs to.

use std::collections::{HashMap, HashSet};

use object::read::elf::{ElfFile64, SectionHeader};
use object::read::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
use object::{
    LittleEndian, RelocationFlags, SectionFlags, SectionIndex, SectionKind, SymbolIndex,
    SymbolKind, SymbolSection,
};

use super::super::prebuilt;
use super::super::target::{Arch, NativeTarget};
use super::abi;
use super::relocate::{relocate, Reloc};
use super::{ProgramLinkError, RelocationTable};

/// Every input is a 64-bit little-endian relocatable; [`parse`] refuses anything else first.
pub(super) type Elf<'data> = ElfFile64<'data, LittleEndian>;

/// Where the executable segment begins, in memory and therefore where the file is mapped.
const BASE: u64 = 0x400000;
const EHDR_SIZE: u64 = 64;
const PHDR_SIZE: u64 = 56;
const PHDR_COUNT: u64 = 2;
// The executable segment is mapped from file offset 0 at `BASE`, which `PT_LOAD` allows only if
// the two are congruent modulo the segment's alignment, the target's maximum page size.
const _: () = {
    let mut index = 0;
    while index < NativeTarget::ALL.len() {
        assert!(BASE.is_multiple_of(NativeTarget::ALL[index].arch.max_page_size()));
        index += 1;
    }
};
/// The most the image may span. Every target's small code model addresses (at most) the low 4 GiB,
/// so a larger image could not be relocated anyway; the bound also keeps layout arithmetic exact.
const IMAGE_LIMIT: u64 = 1 << 32;

/// Every global symbol the prebuilt runtime for `arch` DEFINES; empty when krusty was built without
/// a runtime for it, since then nothing can collide with a program's names.
///
/// The code generator asks for this so it never names a function of its own after one of them: a
/// Kotlin `fun cast(...)` becomes `kt_cast`, which is also the runtime's cast helper, and two
/// definitions of one symbol fail the link with nothing to say about where the Kotlin name was.
/// Reading the answer out of the runtime objects keeps it true as the runtime grows, which a list
/// written down beside them would not. A runtime object that cannot be read is an error rather
/// than a shorter list.
pub fn runtime_symbols(target: NativeTarget) -> Result<HashSet<String>, ProgramLinkError> {
    let mut names = HashSet::new();
    for (name, bytes) in prebuilt::runtime_objects(target).unwrap_or_default() {
        let file = parse(&format!("runtime object `{name}`"), bytes, target.arch)?;
        for symbol in file.symbols() {
            if !symbol.is_local() && !symbol.is_undefined() {
                names.insert(symbol_name(&symbol)?.to_string());
            }
        }
    }
    Ok(names)
}

fn parse_error(error: object::Error) -> ProgramLinkError {
    ProgramLinkError::Parse(error.to_string())
}

fn symbol_name<'data>(symbol: &impl ObjectSymbol<'data>) -> Result<&'data str, ProgramLinkError> {
    symbol.name().map_err(parse_error)
}

/// Read `bytes` (described as `what` in errors) as a relocatable object for `arch`.
///
/// The identification bytes, `e_type` and `e_machine` are checked by hand before anything else: an
/// object for another machine can otherwise parse cleanly and have its relocation numbers read as
/// this architecture's, which either fails as an unknown relocation or patches the wrong bits.
fn parse<'data>(
    what: &str,
    bytes: &'data [u8],
    arch: Arch,
) -> Result<Elf<'data>, ProgramLinkError> {
    let foreign = |why: String| Err(ProgramLinkError::ForeignObject(format!("{what} {why}")));
    if bytes.len() < EHDR_SIZE as usize || !bytes.starts_with(b"\x7fELF") {
        return Err(ProgramLinkError::Parse(format!(
            "{what} is not an ELF object"
        )));
    }
    match bytes[4] {
        2 => {}
        1 => {
            return foreign(format!(
                "is a 32-bit ELF object; {arch:?} links 64-bit ones"
            ))
        }
        class => {
            return Err(ProgramLinkError::Parse(format!(
                "{what} has an unknown ELF class {class}"
            )))
        }
    }
    match bytes[5] {
        1 => {}
        2 => return foreign(format!("is big-endian; {arch:?} is little-endian")),
        data => {
            return Err(ProgramLinkError::Parse(format!(
                "{what} has an unknown ELF byte order {data}"
            )))
        }
    }
    let e_type = u16::from_le_bytes([bytes[16], bytes[17]]);
    if e_type != 1 {
        return Err(ProgramLinkError::Parse(format!(
            "{what} is not a relocatable object (e_type {e_type})"
        )));
    }
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
    if machine != arch.elf_machine() {
        return foreign(format!(
            "is code for ELF machine {machine}, not {arch:?} (machine {})",
            arch.elf_machine()
        ));
    }
    let file =
        Elf::parse(bytes).map_err(|error| ProgramLinkError::Parse(format!("{what}: {error}")))?;
    refuse_implicit_addends(what, &file)?;
    Ok(file)
}

/// Refuse an object with any relocation table but `SHT_RELA`.
///
/// An `SHT_REL` entry has no addend field: its addend is what the field it patches already holds,
/// decoded as each relocation kind defines. `object` reports such an addend as 0, so applying the
/// entry as a `RELA` one would drop it and point the reference at the wrong place. `SHT_CREL`
/// packs entries into a variable-length encoding that may carry addends or leave them implicit
/// too. The x86_64, AArch64 and RISC-V psABIs all specify `RELA` for relocatable objects, and the
/// C compiler and code generator whose objects krusty links emit nothing else, so refusing the
/// other tables costs no real input, while reading them would need an implicit-addend decoder per
/// relocation kind that no input would ever exercise. Every table is refused, not only those that
/// patch loaded sections, so an object's shape decides the answer rather than which of its
/// sections the linker happens to read. With these refused, every relocation the linker applies
/// comes from a `RELA` table, and [`object::read::Relocation::addend`] is its exact addend.
fn refuse_implicit_addends(what: &str, file: &Elf) -> Result<(), ProgramLinkError> {
    for section in file.sections() {
        let header = section.elf_section_header();
        let table = match header.sh_type(LittleEndian) {
            object::elf::SHT_REL => RelocationTable::Rel,
            object::elf::SHT_CREL => RelocationTable::Crel,
            _ => continue,
        };
        let patched = SectionIndex(header.sh_info(LittleEndian) as usize);
        let section = file
            .section_by_index(patched)
            .ok()
            .and_then(|patched| patched.name().ok())
            .unwrap_or("?")
            .to_string();
        return Err(ProgramLinkError::UnsupportedRelocationTable {
            input: what.to_string(),
            section,
            table,
        });
    }
    Ok(())
}

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
    /// Where its bytes start in the output file; `None` for `.bss`, which has none.
    file_offset: Option<u64>,
    /// Its size in memory: every offset into it (a symbol's, a relocation's) lies within this.
    size: u64,
}

/// Where everything goes: each input section's place, and the two segments that hold them.
struct Layout {
    placed: HashMap<(usize, SectionIndex), Placed>,
    /// The target's maximum page size ([`Arch::max_page_size`]): both segments' alignment.
    page: u64,
    /// End of the read+execute segment, which is mapped from file offset 0 at `BASE`.
    text_end: u64,
    /// Where the read+write segment starts in the file and in memory. Both are the first multiple
    /// of `page` past the executable segment, so they stay congruent modulo `page` as `PT_LOAD`
    /// requires, and no page of any size the kernel may use holds bytes of both segments.
    data_file_start: u64,
    data_vaddr_start: u64,
    /// The read+write segment's `.data` bytes, and its extent in memory including `.bss`.
    data_file_size: u64,
    data_mem_size: u64,
}

impl Layout {
    fn new(files: &[Elf], page: u64) -> Result<Self, ProgramLinkError> {
        for (file_index, file) in files.iter().enumerate() {
            for section in file.sections() {
                let name = section.name().unwrap_or("?");
                if matches!(
                    section.kind(),
                    SectionKind::Tls | SectionKind::UninitializedTls
                ) {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "thread-local storage (section `{name}` of input {file_index}) is not \
                         supported by krusty's linker"
                    )));
                }
                let SectionFlags::Elf { sh_flags, .. } = section.flags() else {
                    continue;
                };
                // Notes and unwind tables are loadable but nothing in a krusty program reads them;
                // any other loadable section left out would be code or data silently missing.
                let unread = section.kind() == SectionKind::Note || name == ".eh_frame";
                if sh_flags.contains(object::elf::SHF_ALLOC)
                    && placement(section.kind()).is_none()
                    && !unread
                {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "section `{name}` of input {file_index} is loaded at run time, but is \
                         of a kind krusty's linker does not place"
                    )));
                }
            }
        }
        let mut layout = Self {
            placed: HashMap::new(),
            page,
            text_end: 0,
            data_file_start: 0,
            data_vaddr_start: 0,
            data_file_size: 0,
            data_mem_size: 0,
        };
        // Executable segment: headers, then text, then read-only data. Offsets equal virtual
        // addresses minus BASE, since the segment is mapped from file offset 0.
        let mut end = EHDR_SIZE + PHDR_SIZE * PHDR_COUNT;
        for wanted in [Placement::Text, Placement::ReadOnly] {
            end = layout.place(files, wanted, end, |at| (BASE + at, Some(at)))?;
        }
        layout.text_end = end;
        // Writable segment: data, then bss, starting on the next maximum-size page in the file as
        // well as in memory. Only its file offset and address need be congruent, so the file could
        // stay dense (as GNU ld and `ld.lld` keep it, by mapping one file page into both segments);
        // aligning the file offset too costs at most one page of padding and keeps every page of
        // the file in exactly one segment.
        let file_start = align_up(end, page);
        let vaddr_start = BASE + file_start;
        layout.data_file_start = file_start;
        layout.data_vaddr_start = vaddr_start;
        layout.data_file_size = layout.place(files, Placement::Data, 0, |at| {
            (vaddr_start + at, Some(file_start + at))
        })?;
        layout.data_mem_size =
            layout.place(files, Placement::Bss, layout.data_file_size, |at| {
                (vaddr_start + at, None)
            })?;
        if file_start + layout.data_mem_size > IMAGE_LIMIT {
            return Err(ProgramLinkError::Unsupported(
                "the program is larger than the 4 GiB its code model can address".into(),
            ));
        }
        Ok(layout)
    }

    /// Lay every section that belongs in `wanted` end to end from `cursor`, an offset within its
    /// segment, and return where they end. `locate` turns an offset into `(address, file offset)`.
    ///
    /// Empty sections are placed too: a symbol in one (a start or end marker, a zero-length array)
    /// still needs an address.
    fn place(
        &mut self,
        files: &[Elf],
        wanted: Placement,
        mut cursor: u64,
        locate: impl Fn(u64) -> (u64, Option<u64>),
    ) -> Result<u64, ProgramLinkError> {
        for (file_index, file) in files.iter().enumerate() {
            for section in file.sections() {
                if placement(section.kind()) != Some(wanted) {
                    continue;
                }
                let name = section.name().unwrap_or("?");
                let align = section.align().max(1);
                if !align.is_power_of_two() {
                    return Err(ProgramLinkError::Parse(format!(
                        "section `{name}` of input {file_index} has alignment {align}, which is \
                         not a power of two"
                    )));
                }
                // The writable segment starts on a page, so it can honour no stricter alignment.
                if align > self.page {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "section `{name}` of input {file_index} asks for {align:#x}-byte \
                         alignment; krusty's linker aligns to at most the target's maximum page \
                         size, {:#x} bytes",
                        self.page
                    )));
                }
                // A section with file bytes is sized by the bytes actually present, so the output
                // is never allocated from a size the input merely claims.
                let size = if wanted == Placement::Bss {
                    section.size()
                } else {
                    section.data().map_err(parse_error)?.len() as u64
                };
                cursor = align_up(cursor, align);
                let (vaddr, file_offset) = locate(cursor);
                self.placed.insert(
                    (file_index, section.index()),
                    Placed {
                        vaddr,
                        file_offset,
                        size,
                    },
                );
                cursor = cursor
                    .checked_add(size)
                    .filter(|end| *end <= IMAGE_LIMIT)
                    .ok_or_else(|| {
                        ProgramLinkError::Unsupported(format!(
                            "section `{name}` of input {file_index} takes the program past the \
                             4 GiB its code model can address"
                        ))
                    })?;
            }
        }
        Ok(cursor)
    }
}

/// `value` rounded up to `align`, a power of two no larger than the maximum page size; `value` is at most
/// `IMAGE_LIMIT`, so this cannot overflow.
fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

/// Every symbol's address: globals by name, locals by `(input, symbol index)`.
///
/// Globals are never entered as locals, so a weak definition does not capture its own object's
/// references when another object supplies the strong one: every reference to a global name,
/// wherever it is, resolves through the same table.
struct Symbols {
    strong: HashMap<String, u64>,
    weak: HashMap<String, u64>,
    locals: HashMap<(usize, SymbolIndex), u64>,
}

impl Symbols {
    fn new(files: &[Elf], layout: &Layout) -> Result<Self, ProgramLinkError> {
        let mut symbols = Self {
            strong: HashMap::new(),
            weak: HashMap::new(),
            locals: HashMap::new(),
        };
        for (file_index, file) in files.iter().enumerate() {
            for symbol in file.symbols() {
                let address = match symbol.section() {
                    // References, and `STT_FILE` entries: nothing to define.
                    SymbolSection::Undefined | SymbolSection::None => continue,
                    _ if symbol.kind() == SymbolKind::Tls => {
                        return Err(thread_local(symbol_name(&symbol)?))
                    }
                    SymbolSection::Absolute => symbol.address(),
                    SymbolSection::Common => {
                        return Err(ProgramLinkError::Unsupported(format!(
                            "`{}` is a common symbol (a tentative C definition), which krusty's \
                             linker does not allocate; compile with -fno-common",
                            symbol_name(&symbol)?
                        )))
                    }
                    SymbolSection::Section(index) => {
                        let Some(place) = layout.placed.get(&(file_index, index)) else {
                            if symbol.is_local() {
                                continue; // debug info, notes: nothing at run time refers to it
                            }
                            return Err(ProgramLinkError::Unsupported(format!(
                                "`{}` is defined in a section that is not loaded at run time",
                                symbol_name(&symbol)?
                            )));
                        };
                        if symbol.address() > place.size {
                            return Err(ProgramLinkError::Parse(format!(
                                "`{}` of input {file_index} lies past the end of its section",
                                symbol_name(&symbol)?
                            )));
                        }
                        place.vaddr + symbol.address()
                    }
                    _ => {
                        return Err(ProgramLinkError::Parse(format!(
                            "`{}` of input {file_index} names a section that cannot be read",
                            symbol_name(&symbol)?
                        )))
                    }
                };
                if symbol.is_local() {
                    symbols.locals.insert((file_index, symbol.index()), address);
                    continue;
                }
                let name = symbol_name(&symbol)?.to_string();
                if symbol.is_weak() {
                    symbols.weak.entry(name).or_insert(address);
                } else if symbols.strong.insert(name.clone(), address).is_some() {
                    return Err(ProgramLinkError::DuplicateSymbol(name));
                }
            }
        }
        Ok(symbols)
    }

    /// A global name's address: the strong definition, else the first weak one.
    fn global(&self, name: &str) -> Option<u64> {
        self.strong
            .get(name)
            .or_else(|| self.weak.get(name))
            .copied()
    }

    /// The value `S` of symbol `index` of input `file_index`, as a relocation sees it.
    fn resolve(
        &self,
        file_index: usize,
        file: &Elf,
        index: SymbolIndex,
    ) -> Result<u64, ProgramLinkError> {
        if let Some(address) = self.locals.get(&(file_index, index)) {
            return Ok(*address);
        }
        let symbol = file.symbol_by_index(index).map_err(parse_error)?;
        let name = symbol_name(&symbol)?;
        if symbol.kind() == SymbolKind::Tls {
            return Err(thread_local(name));
        }
        if symbol.is_local() {
            return Err(ProgramLinkError::Unsupported(format!(
                "a relocation in input {file_index} refers to local `{name}`, which has no place \
                 in the executable"
            )));
        }
        if let Some(address) = self.global(name) {
            return Ok(address);
        }
        // An undefined weak reference with no definition anywhere is null, as the ELF gABI says.
        if symbol.is_weak() && symbol.is_undefined() {
            return Ok(0);
        }
        Err(ProgramLinkError::UndefinedSymbol(name.to_string()))
    }
}

fn thread_local(name: &str) -> ProgramLinkError {
    ProgramLinkError::Unsupported(format!(
        "`{name}` is thread-local, and krusty's linker does not support thread-local storage"
    ))
}

pub(super) fn link_static(
    inputs: &[&[u8]],
    target: NativeTarget,
) -> Result<Vec<u8>, ProgramLinkError> {
    let files = inputs
        .iter()
        .enumerate()
        .map(|(index, bytes)| parse(&format!("input {index}"), bytes, target.arch))
        .collect::<Result<Vec<_>, _>>()?;
    let e_flags = abi::output_flags(target.arch, &files)?;
    let layout = Layout::new(&files, target.arch.max_page_size())?;
    let symbols = Symbols::new(&files, &layout)?;
    let entry = symbols
        .global("_start")
        .ok_or_else(|| ProgramLinkError::UndefinedSymbol("_start".into()))?;

    // ---- build the image ---------------------------------------------------------------------
    let mut image = vec![0u8; (layout.data_file_start + layout.data_file_size) as usize];
    for (file_index, file) in files.iter().enumerate() {
        for section in file.sections() {
            let Some(Placed {
                file_offset: Some(start),
                ..
            }) = layout.placed.get(&(file_index, section.index()))
            else {
                continue;
            };
            let data = section.data().map_err(parse_error)?;
            let start = *start as usize;
            image[start..start + data.len()].copy_from_slice(data);
        }
    }

    // ---- apply relocations -------------------------------------------------------------------
    // Gathered first, then applied per architecture: RISC-V's `PCREL_LO12` needs the result of the
    // `PCREL_HI20` it is paired with, which may sit anywhere in the same object.
    for (file_index, file) in files.iter().enumerate() {
        let mut relocations = Vec::new();
        for section in file.sections() {
            let Some(place) = layout.placed.get(&(file_index, section.index())).copied() else {
                continue;
            };
            let name = section.name().unwrap_or("?");
            for (offset, relocation) in section.relocations() {
                let Some(file_offset) = place.file_offset else {
                    return Err(ProgramLinkError::Parse(format!(
                        "input {file_index} relocates `{name}`, which has no bytes to patch"
                    )));
                };
                if offset > place.size {
                    return Err(ProgramLinkError::Parse(format!(
                        "input {file_index}: a relocation at {offset:#x} is past the end of \
                         `{name}` ({:#x} bytes)",
                        place.size
                    )));
                }
                let s = match relocation.target() {
                    RelocationTarget::Symbol(index) => symbols.resolve(file_index, file, index)?,
                    RelocationTarget::Section(index) => layout
                        .placed
                        .get(&(file_index, index))
                        .map(|placed| placed.vaddr)
                        .ok_or_else(|| {
                            ProgramLinkError::Unsupported(
                                "relocation against a section that is not loaded".into(),
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
                    site: (file_offset + offset) as usize,
                    section_end: (file_offset + place.size) as usize,
                    p: place.vaddr + offset,
                    s,
                    // Exact: `parse` refused every table but `RELA`, whose entries carry it.
                    a: relocation.addend(),
                });
            }
        }
        relocate(target.arch, &mut image, &relocations)?;
    }

    write_headers(&mut image, target.arch, e_flags, entry, &layout);
    Ok(image)
}

/// Write the ELF header and the two program headers over the start of the image, which the layout
/// left free for them. `flags` is the `e_flags` the inputs' ABIs merge to ([`abi::output_flags`]).
fn write_headers(image: &mut [u8], arch: Arch, flags: u32, entry: u64, layout: &Layout) {
    let mut h = Vec::with_capacity((EHDR_SIZE + PHDR_SIZE * PHDR_COUNT) as usize);
    h.extend_from_slice(b"\x7fELF");
    h.extend_from_slice(&[2, 1, 1, 0]); // 64-bit, little-endian, ELF version 1, System V ABI
    h.extend_from_slice(&[0; 8]);
    h.extend_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    h.extend_from_slice(&arch.elf_machine().to_le_bytes());
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
        h.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        h.extend_from_slice(&p_flags.to_le_bytes());
        h.extend_from_slice(&offset.to_le_bytes());
        h.extend_from_slice(&vaddr.to_le_bytes());
        h.extend_from_slice(&vaddr.to_le_bytes()); // p_paddr
        h.extend_from_slice(&filesz.to_le_bytes());
        h.extend_from_slice(&memsz.to_le_bytes());
        h.extend_from_slice(&layout.page.to_le_bytes()); // p_align
    };
    phdr(0x5, 0, BASE, layout.text_end, layout.text_end); // R+X
    phdr(
        0x6,
        layout.data_file_start,
        layout.data_vaddr_start,
        layout.data_file_size,
        layout.data_mem_size,
    ); // R+W
    image[..h.len()].copy_from_slice(&h);
}
#[cfg(test)]
mod tests {
    use object::write::{Symbol, SymbolSection};
    use object::{Architecture, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope};

    use super::super::fixture::{
        assert_link_error, link, linked, linux, x86_64_start, Image, Obj, ProgramHeader, TEXT,
    };
    use super::*;

    // ---- layout ------------------------------------------------------------------------------------

    /// The two `PT_LOAD` headers of a program whose code and read-only data end at `text_end`,
    /// whose writable segment starts at file offset `data_offset` and holds 0x10 bytes of `.data`
    /// then 0x20 of `.bss`, and whose segments are aligned to `align`.
    fn two_segments(text_end: u64, data_offset: u64, align: u64) -> Vec<ProgramHeader> {
        vec![
            ProgramHeader {
                p_type: 1,  // PT_LOAD
                p_flags: 5, // R+X
                p_offset: 0,
                p_vaddr: 0x40_0000,
                p_paddr: 0x40_0000,
                p_filesz: text_end,
                p_memsz: text_end,
                p_align: align,
            },
            ProgramHeader {
                p_type: 1,  // PT_LOAD
                p_flags: 6, // R+W
                p_offset: data_offset,
                p_vaddr: 0x40_0000 + data_offset,
                p_paddr: 0x40_0000 + data_offset,
                p_filesz: 0x10,
                p_memsz: 0x30,
                p_align: align,
            },
        ]
    }

    /// Each target's program headers, exactly. The executable segment is mapped from file offset
    /// 0 at the base; the writable one starts on the next multiple of the architecture's maximum
    /// page size, in the file and in memory, and both are aligned to it. On AArch64 that is 64 KiB,
    /// so a kernel built for 64 KiB pages maps the writable segment on a page of its own instead
    /// of over the code's last page, which starting it at 0x401000 would do.
    #[test]
    fn the_segments_are_laid_out_for_each_architecture_s_largest_page() {
        for target in NativeTarget::ALL {
            let (ret, expected) = match target.arch {
                Arch::X86_64 => (
                    vec![0xc3, 0x90, 0x90, 0x90],
                    two_segments(0xc0, 0x1000, 0x1000),
                ),
                Arch::Aarch64 => (
                    0xd65f_03c0u32.to_le_bytes().to_vec(),
                    two_segments(0xc0, 0x1_0000, 0x1_0000),
                ),
                Arch::Riscv64 => (
                    0x0000_8067u32.to_le_bytes().to_vec(),
                    two_segments(0xc0, 0x1000, 0x1000),
                ),
            };
            let mut object = Obj::new(target.arch);
            let text = object.section(".text", SectionKind::Text, &ret, 4);
            object.section(".rodata", SectionKind::ReadOnlyData, &[1; 12], 4);
            object.section(".data", SectionKind::Data, &[2; 16], 8);
            object.uninitialized(".bss", SectionKind::UninitializedData, 32, 8);
            object.define("_start", text, 0);
            let bytes = link_static(&[&object.bytes()], *target)
                .unwrap_or_else(|error| panic!("{target}: {error}"));
            let image = Image(&bytes);
            assert_eq!(image.program_headers(), expected, "{target}");
            // The file ends with the writable segment's bytes: `.bss` takes none.
            assert_eq!(bytes.len() as u64, expected[1].p_offset + 0x10, "{target}");
            assert_eq!(image.bytes(expected[1].p_vaddr, 16), [2; 16], "{target}");
        }
    }

    /// A section may ask for alignment up to the target's maximum page size, the most the writable
    /// segment's start can honour: 64 KiB on AArch64, 4 KiB on x86_64.
    #[test]
    fn a_section_may_be_aligned_to_at_most_the_target_s_largest_page() {
        let mut object = Obj::new(Arch::Aarch64);
        let text = object.text_words(&[0xd65f_03c0]);
        object.define("_start", text, 0);
        let data = object.section(".data", SectionKind::Data, &[7; 8], 0x1_0000);
        object.define("aligned", data, 0);
        let bytes = linked(Arch::Aarch64, &[&object]);
        assert_eq!(Image(&bytes).u64(0x41_0000), 0x0707_0707_0707_0707);

        let (mut object, _) = x86_64_start(&[0xc3]);
        object.section(".data", SectionKind::Data, &[7; 8], 0x1_0000);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            ProgramLinkError::Unsupported(
                "section `.data` of input 0 asks for 0x10000-byte alignment; krusty's linker \
                 aligns to at most the target's maximum page size, 0x1000 bytes"
                    .to_string(),
            ),
        );
    }

    // ---- malformed input is an error, never a panic or a stray write -----------------------------

    fn parse_error(what: &str) -> ProgramLinkError {
        ProgramLinkError::Parse(what.to_string())
    }

    #[test]
    fn an_input_that_is_not_an_object_is_a_parse_error() {
        let garbage: &[u8] = b"not an object file at all";
        assert_link_error(
            link_static(&[garbage], linux(Arch::X86_64)),
            parse_error("input 0 is not an ELF object"),
        );
    }

    #[test]
    fn a_relocation_far_outside_its_section_is_a_parse_error() {
        let (mut object, text) = x86_64_start(&[0xc3]);
        let start = object.define("start_again", text, 0);
        object.reloc(text, 0xff_ffff, start, 0, 2);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            parse_error("input 0: a relocation at 0xffffff is past the end of `.text` (0x1 bytes)"),
        );
    }

    /// A field that starts inside its section but ends past it would otherwise overwrite whatever
    /// the next input's section is.
    #[test]
    fn a_relocation_that_overruns_its_section_is_a_parse_error() {
        let (mut object, text) = x86_64_start(&[0x90, 0x90, 0x90, 0x90]);
        let start = object.define("start_again", text, 0);
        object.reloc(text, 2, start, 0, 2);
        let (neighbour, _) = {
            let mut neighbour = Obj::new(Arch::X86_64);
            let text = neighbour.section(".text", SectionKind::Text, &[0xc3; 4], 1);
            neighbour.define("next", text, 0);
            (neighbour, text)
        };
        assert_link_error(
            link(Arch::X86_64, &[&object, &neighbour]),
            parse_error("relocation type 2 at 0x4000b2 reaches past the end of its section"),
        );
    }

    /// `.bss` has no bytes in the file, so there is nothing a relocation in it could patch.
    #[test]
    fn a_relocation_in_bss_is_a_parse_error() {
        let (mut object, _) = x86_64_start(&[0xc3]);
        let bss = object.uninitialized(".bss", SectionKind::UninitializedData, 16, 8);
        let var = object.define("var", bss, 0);
        object.reloc(bss, 0, var, 0, 1);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            parse_error("input 0 relocates `.bss`, which has no bytes to patch"),
        );
    }

    #[test]
    fn a_symbol_past_the_end_of_its_section_is_a_parse_error() {
        let (mut object, text) = x86_64_start(&[0xe8, 0, 0, 0, 0]);
        let beyond = object.define("beyond", text, 0x1000);
        object.reloc(text, 1, beyond, -4, 4);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            parse_error("`beyond` of input 0 lies past the end of its section"),
        );
    }

    // ---- relocation tables ---------------------------------------------------------------------------

    /// An object whose `.data` holds `S + 8` for `_start`, by each architecture's 64-bit absolute
    /// relocation, in a relocation table of type `sh_type`.
    fn absolute_reference(arch: Arch, sh_type: u32) -> Vec<u8> {
        let (ret, r_type) = match arch {
            Arch::X86_64 => (vec![0xc3, 0x90, 0x90, 0x90], 1), // R_X86_64_64
            Arch::Aarch64 => (0xd65f_03c0u32.to_le_bytes().to_vec(), 257), // R_AARCH64_ABS64
            Arch::Riscv64 => (0x0000_8067u32.to_le_bytes().to_vec(), 2), // R_RISCV_64
        };
        let mut object = Obj::new(arch);
        let text = object.section(".text", SectionKind::Text, &ret, 4);
        let data = object.section(".data", SectionKind::Data, &[0; 8], 8);
        let start = object.define("_start", text, 0);
        object.reloc(data, 0, start, 8, r_type);
        object.bytes_with_relocation_table(sh_type)
    }

    /// An `SHT_REL` table keeps each addend in the bytes it patches, which the linker does not
    /// read; `object` reports it as 0. The object is refused on every target rather than linked
    /// with the reference 8 bytes short.
    #[test]
    fn a_rel_relocation_table_is_refused() {
        for target in NativeTarget::ALL {
            let rel = absolute_reference(target.arch, 9);
            // The input is what the test says it is: a REL table whose one entry has no addend.
            let file = Elf::parse(rel.as_slice()).expect("the REL object parses");
            let data = file.section_by_name(".data").expect("`.data`");
            let relocations: Vec<_> = data.relocations().collect();
            assert_eq!(relocations.len(), 1, "{target}");
            assert_eq!(relocations[0].0, 0, "{target}");
            assert!(relocations[0].1.has_implicit_addend(), "{target}");
            assert_eq!(relocations[0].1.addend(), 0, "{target}");

            let error = link_static(&[&rel], *target).expect_err("a REL table is refused");
            assert_eq!(
                error,
                ProgramLinkError::UnsupportedRelocationTable {
                    input: "input 0".to_string(),
                    section: ".data".to_string(),
                    table: RelocationTable::Rel,
                },
                "{target}"
            );
            assert_eq!(
                error.to_string(),
                "input 0 relocates `.data` with SHT_REL relocations, whose addends are implicit \
                 in the bytes they patch; krusty's linker reads only SHT_RELA relocations, which \
                 the x86_64, AArch64 and RISC-V psABIs specify",
                "{target}"
            );
        }
    }

    /// `SHT_CREL` is refused the same way, whether or not its entries would carry addends.
    #[test]
    fn a_crel_relocation_table_is_refused() {
        for target in NativeTarget::ALL {
            let crel = absolute_reference(target.arch, 0x4000_0014);
            let error = link_static(&[&crel], *target).expect_err("a CREL table is refused");
            assert_eq!(
                error,
                ProgramLinkError::UnsupportedRelocationTable {
                    input: "input 0".to_string(),
                    section: ".data".to_string(),
                    table: RelocationTable::Crel,
                },
                "{target}"
            );
            assert_eq!(
                error.to_string(),
                "input 0 relocates `.data` with SHT_CREL (compact) relocations; krusty's linker \
                 reads only SHT_RELA relocations, which the x86_64, AArch64 and RISC-V psABIs \
                 specify",
                "{target}"
            );
        }
    }

    /// The same object with its `RELA` table as written links, with the addend applied: the
    /// refusals above are about the table, not the relocation.
    #[test]
    fn the_same_reference_in_a_rela_table_links() {
        for target in NativeTarget::ALL {
            let rela = absolute_reference(target.arch, 4);
            let bytes =
                link_static(&[&rela], *target).unwrap_or_else(|error| panic!("{target}: {error}"));
            let image = Image(&bytes);
            assert_eq!(image.u64(image.data()), TEXT + 8, "{target}");
        }
    }

    // ---- objects for another machine --------------------------------------------------------------

    fn foreign(what: &str) -> ProgramLinkError {
        ProgramLinkError::ForeignObject(what.to_string())
    }

    /// An x86_64 object handed to a riscv64 link is named as such — even one whose only
    /// relocation's number means something (else) on riscv64 too.
    #[test]
    fn an_object_for_another_machine_is_refused() {
        let (mut object, text) = x86_64_start(&[0; 8]);
        let start = object.define("start_again", text, 0);
        object.reloc(text, 0, start, 0, 1); // R_X86_64_64, which is R_RISCV_32 by number
        assert_link_error(
            link(Arch::Riscv64, &[&object]),
            foreign("input 0 is code for ELF machine 62, not Riscv64 (machine 243)"),
        );
    }

    #[test]
    fn a_32_bit_object_is_refused() {
        let mut object = Obj::of(Architecture::I386, Endianness::Little);
        let text = object.section(".text", SectionKind::Text, &[0xc3], 1);
        object.define("_start", text, 0);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            foreign("input 0 is a 32-bit ELF object; X86_64 links 64-bit ones"),
        );
    }

    #[test]
    fn a_big_endian_object_is_refused() {
        let mut object = Obj::of(Architecture::Aarch64, Endianness::Big);
        let text = object.section(".text", SectionKind::Text, &[0xd6, 0x5f, 0x03, 0xc0], 4);
        object.define("_start", text, 0);
        assert_link_error(
            link(Arch::Aarch64, &[&object]),
            foreign("input 0 is big-endian; Aarch64 is little-endian"),
        );
    }

    // ---- symbol binding and shapes ----------------------------------------------------------------

    /// A weak definition yields to a strong one from another object — for its own object's
    /// references too, so one name never runs two bodies.
    #[test]
    fn a_strong_definition_elsewhere_overrides_a_weak_one_everywhere() {
        #[rustfmt::skip]
        let (mut weak_side, text) = x86_64_start(&[
            0xe8, 0, 0, 0, 0,             // 0: _start: call foo
            0xc3,                         // 5: ret
            0xb8, 1, 0, 0, 0, 0xc3,       // 6: foo (weak): mov eax, 1; ret
        ]);
        let weak_foo = weak_side.define_weak("foo", text, 6);
        weak_side.reloc(text, 1, weak_foo, -4, 4);
        let mut strong_side = Obj::new(Arch::X86_64);
        let strong_text =
            strong_side.section(".text", SectionKind::Text, &[0xb8, 2, 0, 0, 0, 0xc3], 16);
        strong_side.define("foo", strong_text, 0);
        let bytes = linked(Arch::X86_64, &[&weak_side, &strong_side]);
        let image = Image(&bytes);
        // The strong `foo` is the second input's text, 16-aligned after the first's 12 bytes.
        let strong_foo = TEXT + 16;
        assert_eq!(image.bytes(strong_foo, 2), [0xb8, 2]);
        let call = image.u32(TEXT + 1) as i32 as i64;
        assert_eq!(TEXT as i64 + 5 + call, strong_foo as i64);
    }

    /// With no definition anywhere, a weak reference is zero (plus its addend), not an error.
    #[test]
    fn an_undefined_weak_reference_resolves_to_zero() {
        let (mut object, _) = x86_64_start(&[0xc3]);
        let data = object.section(".data", SectionKind::Data, &[0xff; 8], 8);
        let hook = object.undefined_weak("optional_hook");
        object.reloc(data, 0, hook, 5, 1);
        let bytes = linked(Arch::X86_64, &[&object]);
        assert_eq!(Image(&bytes).u64(0x40_1000), 5);
    }

    #[test]
    fn an_undefined_strong_reference_is_named() {
        let (mut object, text) = x86_64_start(&[0xe8, 0, 0, 0, 0]);
        let missing = object.undefined("missing");
        object.reloc(text, 1, missing, -4, 4);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            ProgramLinkError::UndefinedSymbol("missing".to_string()),
        );
    }

    #[test]
    fn two_strong_definitions_are_a_duplicate() {
        let (first, _) = x86_64_start(&[0xc3]);
        let (second, _) = x86_64_start(&[0xc3]);
        assert_link_error(
            link(Arch::X86_64, &[&first, &second]),
            ProgramLinkError::DuplicateSymbol("_start".to_string()),
        );
    }

    /// An `SHN_ABS` symbol's value is its address.
    #[test]
    fn an_absolute_symbol_resolves_to_its_value() {
        let (mut object, _) = x86_64_start(&[0xc3]);
        let data = object.section(".data", SectionKind::Data, &[0; 8], 8);
        let fixed = object.absolute("fixed", 0x1234);
        object.reloc(data, 0, fixed, 1, 1);
        let mut other = Obj::new(Arch::X86_64);
        let other_data = other.section(".data", SectionKind::Data, &[0; 8], 8);
        let fixed_elsewhere = other.undefined("fixed");
        other.reloc(other_data, 0, fixed_elsewhere, 0, 1);
        let bytes = linked(Arch::X86_64, &[&object, &other]);
        let image = Image(&bytes);
        assert_eq!(image.u64(0x40_1000), 0x1235);
        assert_eq!(image.u64(0x40_1008), 0x1234);
    }

    /// A symbol in an empty section (a zero-length array, a start/end marker) still has an address.
    #[test]
    fn a_symbol_in_an_empty_section_is_placed() {
        let (mut object, _) = x86_64_start(&[0xc3]);
        let data = object.section(".data", SectionKind::Data, &[0; 8], 8);
        let empty = object.section(".data.empty", SectionKind::Data, &[], 8);
        let marker = object.define("marker", empty, 0);
        object.reloc(data, 0, marker, 0, 1);
        let bytes = linked(Arch::X86_64, &[&object]);
        assert_eq!(Image(&bytes).u64(0x40_1000), 0x40_1008);
    }

    #[test]
    fn thread_local_storage_is_unsupported_not_undefined() {
        let (mut object, text) = x86_64_start(&[0x8b, 0x05, 0, 0, 0, 0]);
        let tbss = object.uninitialized(".tbss", SectionKind::UninitializedTls, 8, 8);
        let counter = object.symbol(Symbol {
            name: b"counter".to_vec(),
            value: 0,
            size: 8,
            kind: SymbolKind::Tls,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(tbss),
            flags: SymbolFlags::None,
        });
        object.reloc(text, 2, counter, -4, 2);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            ProgramLinkError::Unsupported(
                "thread-local storage (section `.tbss` of input 0) is not supported by krusty's linker"
                    .to_string(),
            ),
        );
    }

    #[test]
    fn a_common_symbol_is_unsupported_not_undefined() {
        let (mut object, text) = x86_64_start(&[0x8b, 0x05, 0, 0, 0, 0]);
        let tentative = object.common("tentative", 8, 8);
        object.reloc(text, 2, tentative, -4, 2);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            ProgramLinkError::Unsupported(
                "`tentative` is a common symbol (a tentative C definition), which krusty's linker \
                 does not allocate; compile with -fno-common"
                    .to_string(),
            ),
        );
    }
}

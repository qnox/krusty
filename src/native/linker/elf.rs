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

use object::read::elf::ElfFile64;
use object::read::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
use object::{
    LittleEndian, RelocationFlags, SectionFlags, SectionIndex, SectionKind, SymbolIndex,
    SymbolKind, SymbolSection,
};

use super::super::prebuilt;
use super::super::target::{Arch, NativeTarget};
use super::ProgramLinkError;

/// Every input is a 64-bit little-endian relocatable; [`parse`] refuses anything else first.
type Elf<'data> = ElfFile64<'data, LittleEndian>;

/// Where the executable segment begins, in memory and therefore where the file is mapped.
const BASE: u64 = 0x400000;
const PAGE: u64 = 0x1000;
const EHDR_SIZE: u64 = 64;
const PHDR_SIZE: u64 = 56;
const PHDR_COUNT: u64 = 2;
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
pub fn runtime_symbols(arch: Arch) -> Result<HashSet<String>, ProgramLinkError> {
    let mut names = HashSet::new();
    for (name, bytes) in prebuilt::runtime_objects(arch).unwrap_or_default() {
        let file = parse(&format!("runtime object `{name}`"), bytes, arch)?;
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
    Elf::parse(bytes).map_err(|error| ProgramLinkError::Parse(format!("{what}: {error}")))
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
    /// End of the read+execute segment, which is mapped from file offset 0 at `BASE`.
    text_end: u64,
    /// Where the read+write segment starts in the file and in memory: page-aligned, so the two
    /// stay congruent modulo the page as `PT_LOAD` requires.
    data_file_start: u64,
    data_vaddr_start: u64,
    /// The read+write segment's `.data` bytes, and its extent in memory including `.bss`.
    data_file_size: u64,
    data_mem_size: u64,
}

impl Layout {
    fn new(files: &[Elf]) -> Result<Self, ProgramLinkError> {
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
        // Writable segment: data, then bss.
        let file_start = align_up(end, PAGE);
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
                if align > PAGE {
                    return Err(ProgramLinkError::Unsupported(format!(
                        "section `{name}` of input {file_index} asks for {align}-byte alignment; \
                         krusty's linker aligns to at most a page"
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

/// `value` rounded up to `align`, a power of two no larger than a page; `value` is at most
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
    let layout = Layout::new(&files)?;
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
                    a: relocation.addend(),
                });
            }
        }
        relocate(target.arch, &mut image, &relocations)?;
    }

    write_headers(&mut image, target.arch, entry, &layout);
    Ok(image)
}

/// One relocation, resolved: where it is, what it points at.
struct Reloc {
    r_type: u32,
    /// Offset of the site in the output file.
    site: usize,
    /// End, in the output file, of the section holding the site: no field may reach past it.
    section_end: usize,
    /// Address of the site: `P` in the ABI documents.
    p: u64,
    /// Value of the symbol: `S`.
    s: u64,
    /// Addend: `A`.
    a: i64,
}

impl Reloc {
    /// The `N` bytes `at` bytes past the site, which must lie within the site's section.
    fn field<'image, const N: usize>(
        &self,
        image: &'image mut [u8],
        at: usize,
    ) -> Result<&'image mut [u8; N], ProgramLinkError> {
        let start = self.site + at;
        (start + N <= self.section_end)
            .then(|| image.get_mut(start..start + N))
            .flatten()
            .and_then(|bytes| <&mut [u8; N]>::try_from(bytes).ok())
            .ok_or_else(|| {
                ProgramLinkError::Parse(format!(
                    "relocation type {} at {:#x} reaches past the end of its section",
                    self.r_type, self.p
                ))
            })
    }

    fn set64(&self, image: &mut [u8], value: u64) -> Result<(), ProgramLinkError> {
        *self.field::<8>(image, 0)? = value.to_le_bytes();
        Ok(())
    }

    /// Rewrite the 32-bit word `at` bytes past the site as `patch` of its current value.
    fn update32(
        &self,
        image: &mut [u8],
        at: usize,
        patch: impl FnOnce(u32) -> u32,
    ) -> Result<(), ProgramLinkError> {
        let field = self.field::<4>(image, at)?;
        *field = patch(u32::from_le_bytes(*field)).to_le_bytes();
        Ok(())
    }

    fn update16(
        &self,
        image: &mut [u8],
        patch: impl FnOnce(u16) -> u16,
    ) -> Result<(), ProgramLinkError> {
        let field = self.field::<2>(image, 0)?;
        *field = patch(u16::from_le_bytes(*field)).to_le_bytes();
        Ok(())
    }
}

fn out_of_range(what: &str, value: i128, p: u64) -> ProgramLinkError {
    let sign = if value < 0 { "-" } else { "" };
    ProgramLinkError::RelocationOutOfRange(format!(
        "{what}: value {sign}{:#x} at {p:#x}",
        value.unsigned_abs()
    ))
}

/// Does `value` fit in a signed field of `bits` bits?
fn fits_signed(value: i128, bits: u32) -> bool {
    let min = -(1i128 << (bits - 1));
    let max = (1i128 << (bits - 1)) - 1;
    (min..=max).contains(&value)
}

/// `S + A` and `S + A - P`, exactly. Every relocation expression is computed here, in a domain
/// wide enough that no input can wrap it: `S` and `P` are 64-bit addresses and `A` any 64-bit
/// addend, so a sum or difference of them needs 66 bits. Computed in 64 bits, an extreme addend
/// wraps an out-of-range value back into a field's range, and the check that should refuse it
/// passes it with the wrong patch. Every range check below runs on these values, and a field is
/// narrowed to its width only after its check.
fn expressions(r: &Reloc) -> (i128, i128) {
    let s_plus_a = i128::from(r.s) + i128::from(r.a);
    (s_plus_a, s_plus_a - i128::from(r.p))
}

/// A 64-bit field holds any value 64 bits can spell, whether it is read as signed or unsigned.
fn set_word64(
    image: &mut [u8],
    r: &Reloc,
    value: i128,
    what: &str,
) -> Result<(), ProgramLinkError> {
    if !(i128::from(i64::MIN)..=i128::from(u64::MAX)).contains(&value) {
        return Err(out_of_range(what, value, r.p));
    }
    r.set64(image, value as u64)
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
    let (s_plus_a, pc_relative) = expressions(r);
    match r.r_type {
        // R_X86_64_64
        1 => set_word64(image, r, s_plus_a, "64"),
        // R_X86_64_PC32, R_X86_64_PLT32: a static link has no PLT, so both are PC-relative to S.
        2 | 4 => {
            if !fits_signed(pc_relative, 32) {
                return Err(out_of_range("PC32", pc_relative, r.p));
            }
            r.update32(image, 0, |_| pc_relative as u32)
        }
        // R_X86_64_32 (zero-extended) and R_X86_64_32S (sign-extended) absolute.
        10 => {
            if u32::try_from(s_plus_a).is_err() {
                return Err(out_of_range("32", s_plus_a, r.p));
            }
            r.update32(image, 0, |_| s_plus_a as u32)
        }
        11 => {
            if !fits_signed(s_plus_a, 32) {
                return Err(out_of_range("32S", s_plus_a, r.p));
            }
            r.update32(image, 0, |_| s_plus_a as u32)
        }
        other => Err(ProgramLinkError::UnsupportedRelocation {
            arch: Arch::X86_64,
            r_type: other,
        }),
    }
}

/// Insert the low 12 bits of `x`, scaled down by the access size `1 << shift`, into the imm12
/// field (bits 21:10) of an `ADD`/`LDR`/`STR`. The instruction cannot encode the bits the scaling
/// drops, so a target not aligned to the access size is refused (as `ld.lld` does) rather than
/// silently truncated to the aligned address below it.
fn aarch64_lo12(
    image: &mut [u8],
    r: &Reloc,
    x: i128,
    shift: u32,
    what: &str,
) -> Result<(), ProgramLinkError> {
    if x as u64 & ((1 << shift) - 1) != 0 {
        return Err(out_of_range(
            &format!("{what}: target is not {}-byte aligned", 1 << shift),
            x,
            r.p,
        ));
    }
    let field = ((x as u64 & 0xfff) >> shift) as u32;
    r.update32(image, 0, |instruction| {
        (instruction & !(0xfff << 10)) | (field << 10)
    })
}

/// AArch64: every kind clang's freestanding objects and Cranelift's non-PIC output use. Fields are
/// patched into fixed 32-bit instructions per the ELF-for-AArch64 supplement.
fn relocate_aarch64(image: &mut [u8], r: &Reloc) -> Result<(), ProgramLinkError> {
    let (x, rel) = expressions(r);
    match r.r_type {
        // R_AARCH64_ABS64
        257 => set_word64(image, r, x, "ABS64"),
        // R_AARCH64_PREL32 (`.eh_frame` and friends)
        261 => {
            if !fits_signed(rel, 32) {
                return Err(out_of_range("PREL32", rel, r.p));
            }
            r.update32(image, 0, |_| rel as u32)
        }
        // R_AARCH64_ADR_PREL_PG_HI21: page delta into ADRP's immhi:immlo.
        275 => {
            let page_delta = (x & !0xfff) - i128::from(r.p & !0xfff);
            if !fits_signed(page_delta, 33) {
                return Err(out_of_range("ADR_PREL_PG_HI21", page_delta, r.p));
            }
            let pages = (page_delta >> 12) as u64;
            let immlo = ((pages & 0x3) as u32) << 29;
            let immhi = (((pages >> 2) & 0x7ffff) as u32) << 5;
            r.update32(image, 0, |instruction| {
                (instruction & !((0x3 << 29) | (0x7ffff << 5))) | immlo | immhi
            })
        }
        // R_AARCH64_ADD_ABS_LO12_NC and the LDST*_ABS_LO12_NC family.
        277 => aarch64_lo12(image, r, x, 0, "ADD_ABS_LO12_NC"),
        278 => aarch64_lo12(image, r, x, 0, "LDST8_ABS_LO12_NC"),
        284 => aarch64_lo12(image, r, x, 1, "LDST16_ABS_LO12_NC"),
        285 => aarch64_lo12(image, r, x, 2, "LDST32_ABS_LO12_NC"),
        286 => aarch64_lo12(image, r, x, 3, "LDST64_ABS_LO12_NC"),
        299 => aarch64_lo12(image, r, x, 4, "LDST128_ABS_LO12_NC"),
        // R_AARCH64_JUMP26 / R_AARCH64_CALL26: imm26 = (S+A-P) >> 2.
        282 | 283 => {
            if rel & 3 != 0 || !fits_signed(rel, 28) {
                return Err(out_of_range("CALL26", rel, r.p));
            }
            let imm26 = ((rel >> 2) as u32) & 0x03ff_ffff;
            r.update32(image, 0, |instruction| (instruction & !0x03ff_ffff) | imm26)
        }
        other => Err(ProgramLinkError::UnsupportedRelocation {
            arch: Arch::Aarch64,
            r_type: other,
        }),
    }
}

/// Does `value` split into a `hi20`/`lo12` pair? `hi20` is `(value + 0x800) >> 12`, so it is that
/// sum, not `value`, that has to fit 32 signed bits: a value within 0x800 below 2 GiB would
/// otherwise wrap `hi20` to a negative page.
fn fits_hi_lo(value: i128) -> bool {
    fits_signed(value + 0x800, 32)
}

/// RISC-V: two passes, because a `PCREL_LO12_*` relocation names the `auipc` it pairs with rather
/// than the final symbol, and takes the low half of THAT relocation's value. `RELAX` is ignored:
/// this linker performs no relaxation, so every instruction stays where the assembler put it.
fn relocate_riscv64(image: &mut [u8], relocations: &[Reloc]) -> Result<(), ProgramLinkError> {
    // Value `X = S + A - P` of every PCREL_HI20, keyed by the address of its `auipc`.
    let mut hi20_at: HashMap<u64, i128> = HashMap::new();
    let mut deferred = Vec::new();
    for r in relocations {
        let (x, rel) = expressions(r);
        match r.r_type {
            // R_RISCV_64 / R_RISCV_32
            2 => set_word64(image, r, x, "64")?,
            1 => {
                if u32::try_from(x).is_err() && !fits_signed(x, 32) {
                    return Err(out_of_range("32", x, r.p));
                }
                r.update32(image, 0, |_| x as u32)?;
            }
            // R_RISCV_BRANCH: B-type immediate.
            16 => {
                if rel & 1 != 0 || !fits_signed(rel, 13) {
                    return Err(out_of_range("BRANCH", rel, r.p));
                }
                r.update32(image, 0, |instruction| {
                    (instruction & 0x01ff_f07f) | b_type(rel as u32)
                })?;
            }
            // R_RISCV_JAL: J-type immediate.
            17 => {
                if rel & 1 != 0 || !fits_signed(rel, 21) {
                    return Err(out_of_range("JAL", rel, r.p));
                }
                r.update32(image, 0, |instruction| {
                    (instruction & 0x0000_0fff) | j_type(rel as u32)
                })?;
            }
            // R_RISCV_CALL / R_RISCV_CALL_PLT: `auipc` at P, `jalr` at P+4; no PLT in a static link.
            18 | 19 => {
                if !fits_hi_lo(rel) {
                    return Err(out_of_range("CALL", rel, r.p));
                }
                let (hi, lo) = split_hi_lo(rel);
                // Both words are checked to lie in the section before either is written.
                r.field::<8>(image, 0)?;
                r.update32(image, 0, |auipc| (auipc & 0xfff) | (hi << 12))?;
                r.update32(image, 4, |jalr| (jalr & 0x000f_ffff) | (lo << 20))?;
            }
            // R_RISCV_PCREL_HI20: remember X for the paired LO12.
            23 => {
                if !fits_hi_lo(rel) {
                    return Err(out_of_range("PCREL_HI20", rel, r.p));
                }
                let (hi, _) = split_hi_lo(rel);
                r.update32(image, 0, |auipc| (auipc & 0xfff) | (hi << 12))?;
                hi20_at.insert(r.p, rel);
            }
            // R_RISCV_PCREL_LO12_I / _S: resolved after every HI20 is known.
            24 | 25 => deferred.push(r),
            // R_RISCV_HI20 / LO12_I / LO12_S: absolute, split the same way.
            26 => {
                if !fits_hi_lo(x) {
                    return Err(out_of_range("HI20", x, r.p));
                }
                let (hi, _) = split_hi_lo(x);
                r.update32(image, 0, |lui| (lui & 0xfff) | (hi << 12))?;
            }
            27 => {
                let (_, lo) = split_hi_lo(x);
                r.update32(image, 0, |instruction| {
                    (instruction & 0x000f_ffff) | (lo << 20)
                })?;
            }
            28 => {
                let (_, lo) = split_hi_lo(x);
                r.update32(image, 0, |instruction| {
                    (instruction & 0x01ff_f07f) | s_type(lo)
                })?;
            }
            // R_RISCV_RVC_BRANCH / R_RISCV_RVC_JUMP: 16-bit compressed forms.
            44 => {
                if rel & 1 != 0 || !fits_signed(rel, 9) {
                    return Err(out_of_range("RVC_BRANCH", rel, r.p));
                }
                r.update16(image, |instruction| {
                    (instruction & 0xe383) | cb_type(rel as u32)
                })?;
            }
            45 => {
                if rel & 1 != 0 || !fits_signed(rel, 12) {
                    return Err(out_of_range("RVC_JUMP", rel, r.p));
                }
                r.update16(image, |instruction| {
                    (instruction & 0xe003) | cj_type(rel as u32)
                })?;
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
        if r.r_type == 24 {
            r.update32(image, 0, |instruction| {
                (instruction & 0x000f_ffff) | (lo << 20)
            })?;
        } else {
            r.update32(image, 0, |instruction| {
                (instruction & 0x01ff_f07f) | s_type(lo)
            })?;
        }
    }
    Ok(())
}

/// Split a 32-bit value into RISC-V's `hi20`/`lo12` pair, where `lo12` is sign-extended and `hi20`
/// is adjusted so that `(hi20 << 12) + sext(lo12) == value`. A `LO12` alone has no range to check,
/// so `value` may be anything; only its low 32 bits matter.
fn split_hi_lo(value: i128) -> (u32, u32) {
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

/// Write the ELF header and the two program headers over the start of the image, which the layout
/// left free for them.
fn write_headers(image: &mut [u8], arch: Arch, entry: u64, layout: &Layout) {
    let flags: u32 = match arch {
        Arch::X86_64 | Arch::Aarch64 => 0,
        // EF_RISCV_RVC | EF_RISCV_FLOAT_ABI_DOUBLE: what `-march=rv64gc -mabi=lp64d` objects carry.
        Arch::Riscv64 => 0x0005,
    };
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
        h.extend_from_slice(&PAGE.to_le_bytes());
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

    use super::super::fixture::{linux, Image, Obj, TEXT};
    use super::*;

    fn link(arch: Arch, objects: &[&Obj]) -> Result<Vec<u8>, ProgramLinkError> {
        let bytes: Vec<Vec<u8>> = objects.iter().map(|object| object.bytes()).collect();
        let inputs: Vec<&[u8]> = bytes.iter().map(Vec::as_slice).collect();
        link_static(&inputs, linux(arch))
    }

    fn linked(arch: Arch, objects: &[&Obj]) -> Vec<u8> {
        link(arch, objects).unwrap_or_else(|error| panic!("{arch:?}: link failed: {error}"))
    }

    /// An x86_64 object whose `.text` is `bytes` with `_start` at its first byte.
    fn x86_64_start(bytes: &[u8]) -> (Obj, object::write::SectionId) {
        let mut object = Obj::new(Arch::X86_64);
        let text = object.section(".text", SectionKind::Text, bytes, 16);
        object.define("_start", text, 0);
        (object, text)
    }

    // ---- every relocation kind, on each architecture ----------------------------------------------

    /// x86_64: `PLT32`/`PC32` are `S + A - P`, `32`/`32S` are `S + A` zero-/sign-extended, `64` is
    /// `S + A`. Only the fields change; the opcodes around them are the assembler's.
    #[test]
    fn x86_64_relocations_patch_their_fields() {
        let mut object = Obj::new(Arch::X86_64);
        #[rustfmt::skip]
        let code = [
            0xe8, 0, 0, 0, 0,                   // 0:  call foo
            0x8b, 0x05, 0, 0, 0, 0,             // 5:  mov eax, [rip + var]
            0xb8, 0, 0, 0, 0,                   // 11: mov eax, var
            0x48, 0xc7, 0xc0, 0, 0, 0, 0,       // 16: mov rax, var (sign-extended)
            0xc3,                               // 23: ret
            0xc3,                               // 24: foo: ret
        ];
        let text = object.section(".text", SectionKind::Text, &code, 16);
        let data = object.section(".data", SectionKind::Data, &[0; 0x18], 8);
        object.define("_start", text, 0);
        let foo = object.define("foo", text, 24);
        let var = object.define("var", data, 0x10);
        object.reloc(text, 1, foo, -4, 4); // R_X86_64_PLT32
        object.reloc(text, 7, var, -4, 2); // R_X86_64_PC32
        object.reloc(text, 12, var, 0, 10); // R_X86_64_32
        object.reloc(text, 19, var, 0, 11); // R_X86_64_32S
        object.reloc(data, 0, foo, 2, 1); // R_X86_64_64
        let bytes = linked(Arch::X86_64, &[&object]);
        let image = Image(&bytes);
        assert_eq!(image.machine(), 62);
        assert_eq!(image.entry(), TEXT);
        assert_eq!(image.data(), 0x40_1000);
        // foo = 0x4000c8, var = 0x401010.
        #[rustfmt::skip]
        let expected = [
            0xe8, 0x13, 0, 0, 0,                   // 0x4000c8 - 4 - 0x4000b1
            0x8b, 0x05, 0x55, 0x0f, 0, 0,          // 0x401010 - 4 - 0x4000b7
            0xb8, 0x10, 0x10, 0x40, 0,
            0x48, 0xc7, 0xc0, 0x10, 0x10, 0x40, 0,
            0xc3,
            0xc3,
        ];
        assert_eq!(image.bytes(TEXT, expected.len()), expected);
        assert_eq!(image.u64(0x40_1000), 0x40_00ca);
    }

    /// AArch64: `ADRP` takes the page delta, `ADD`/`LDST*_LO12` the low 12 bits scaled by the
    /// access size, `CALL26`/`JUMP26` the word offset, `ABS64`/`PREL32` whole values. Every expected
    /// word is what `llvm-mc` encodes for the instruction with the resolved operand.
    #[test]
    fn aarch64_relocations_patch_their_fields() {
        let mut object = Obj::new(Arch::Aarch64);
        let text = object.text_words(&[
            0x9000_0001, // 0:  adrp x1, var
            0x9100_0021, // 4:  add x1, x1, :lo12:var
            0x3940_0020, // 8:  ldrb w0, [x1, :lo12:var+1]
            0x7940_0020, // 12: ldrh w0, [x1, :lo12:var+2]
            0xb940_0020, // 16: ldr w0, [x1, :lo12:var+4]
            0xf940_0020, // 20: ldr x0, [x1, :lo12:var+8]
            0x3dc0_0020, // 24: ldr q0, [x1, :lo12:var+16]
            0x9400_0000, // 28: bl foo
            0x1400_0000, // 32: b foo
            0xd65f_03c0, // 36: foo: ret
        ]);
        let data = object.section(".data", SectionKind::Data, &[0; 0x50], 16);
        object.define("_start", text, 0);
        let foo = object.define("foo", text, 36);
        let var = object.define("var", data, 0x20);
        object.reloc(text, 0, var, 0, 275); // ADR_PREL_PG_HI21
        object.reloc(text, 4, var, 0, 277); // ADD_ABS_LO12_NC
        object.reloc(text, 8, var, 1, 278); // LDST8_ABS_LO12_NC
        object.reloc(text, 12, var, 2, 284); // LDST16_ABS_LO12_NC
        object.reloc(text, 16, var, 4, 285); // LDST32_ABS_LO12_NC
        object.reloc(text, 20, var, 8, 286); // LDST64_ABS_LO12_NC
        object.reloc(text, 24, var, 16, 299); // LDST128_ABS_LO12_NC
        object.reloc(text, 28, foo, 0, 283); // CALL26
        object.reloc(text, 32, foo, 0, 282); // JUMP26
        object.reloc(data, 0x40, foo, 0, 257); // ABS64
        object.reloc(data, 0x48, foo, 0, 261); // PREL32
        let bytes = linked(Arch::Aarch64, &[&object]);
        let image = Image(&bytes);
        assert_eq!(image.machine(), 183);
        assert_eq!(image.data(), 0x40_1000);
        // var = 0x401020, foo = 0x4000d4.
        let words: Vec<u32> = (0..10).map(|i| image.u32(TEXT + 4 * i)).collect();
        assert_eq!(
            words,
            [
                0xb000_0001, // adrp x1, #0x1000
                0x9100_8021, // add x1, x1, #0x20
                0x3940_8420, // ldrb w0, [x1, #0x21]
                0x7940_4420, // ldrh w0, [x1, #0x22]
                0xb940_2420, // ldr w0, [x1, #0x24]
                0xf940_1420, // ldr x0, [x1, #0x28]
                0x3dc0_0c20, // ldr q0, [x1, #0x30]
                0x9400_0002, // bl +8
                0x1400_0001, // b +4
                0xd65f_03c0,
            ]
        );
        assert_eq!(image.u64(0x40_1040), 0x40_00d4);
        assert_eq!(image.u32(0x40_1048), 0x40_00d4u32.wrapping_sub(0x40_1048));
    }

    /// RISC-V: absolute and PC-relative `hi20`/`lo12` pairs (with the carry when the low half is
    /// negative), `PCREL_LO12` taking the value of the `auipc` it names, `CALL` patching both
    /// instructions, and the B/J/CB/CJ immediate scatters. `RELAX` changes nothing. Every expected
    /// word is what `llvm-mc` encodes for the instruction with the resolved operand.
    #[test]
    fn riscv64_relocations_patch_their_fields() {
        let mut object = Obj::new(Arch::Riscv64);
        let mut code: Vec<u8> = [
            0x0000_0517u32, // 0:  auipc a0, %pcrel_hi(var)
            0x0005_0513,    // 4:  addi a0, a0, %pcrel_lo(0b)
            0x00b5_3023,    // 8:  sd a1, %pcrel_lo(0b)(a0)
            0x0000_0537,    // 12: lui a0, %hi(var)
            0x0005_0513,    // 16: addi a0, a0, %lo(var)
            0x00b5_3023,    // 20: sd a1, %lo(var)(a0)
            0x0000_0097,    // 24: auipc ra, 0   \ call foo
            0x0000_80e7,    // 28: jalr ra, 0(ra) /
            0x00b5_0063,    // 32: beq a0, a1, foo
            0x0000_00ef,    // 36: jal ra, foo
        ]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect();
        code.extend_from_slice(&0xc101u16.to_le_bytes()); // 40: c.beqz a0, foo
        code.extend_from_slice(&0xa001u16.to_le_bytes()); // 42: c.j foo
        code.extend_from_slice(&0x0000_8067u32.to_le_bytes()); // 44: foo: ret
        let text = object.section(".text", SectionKind::Text, &code, 4);
        let data = object.section(".data", SectionKind::Data, &[0; 0x910], 8);
        object.define("_start", text, 0);
        let foo = object.define("foo", text, 44);
        let var = object.define("var", data, 0x900);
        let hi = object.local(".Lpcrel_hi0", text, 0);
        object.reloc(text, 0, var, 0, 23); // PCREL_HI20
        object.reloc(text, 4, hi, 0, 24); // PCREL_LO12_I
        object.reloc(text, 8, hi, 0, 25); // PCREL_LO12_S
        object.reloc(text, 12, var, 0, 26); // HI20
        object.reloc(text, 16, var, 0, 27); // LO12_I
        object.reloc(text, 20, var, 0, 28); // LO12_S
        object.reloc(text, 24, foo, 0, 19); // CALL_PLT
        object.reloc(text, 24, foo, 0, 51); // RELAX
        object.reloc(text, 32, foo, 0, 16); // BRANCH
        object.reloc(text, 36, foo, 0, 17); // JAL
        object.reloc(text, 40, foo, 0, 44); // RVC_BRANCH
        object.reloc(text, 42, foo, 0, 45); // RVC_JUMP
        object.reloc(data, 0, foo, 0, 2); // R_RISCV_64
        object.reloc(data, 8, foo, 0, 1); // R_RISCV_32
        let bytes = linked(Arch::Riscv64, &[&object]);
        let image = Image(&bytes);
        assert_eq!(image.machine(), 243);
        assert_eq!(image.data(), 0x40_1000);
        // var = 0x401900 (low half 0x900 is negative: hi carries), foo = 0x4000dc.
        let words: Vec<u32> = (0..10).map(|i| image.u32(TEXT + 4 * i)).collect();
        assert_eq!(
            words,
            [
                0x0000_2517, // auipc a0, 2              (0x401900 - 0x4000b0 = 0x1850)
                0x8505_0513, // addi a0, a0, -1968
                0x84b5_3823, // sd a1, -1968(a0)
                0x0040_2537, // lui a0, 0x402
                0x9005_0513, // addi a0, a0, -1792
                0x90b5_3023, // sd a1, -1792(a0)
                0x0000_0097, // auipc ra, 0
                0x0140_80e7, // jalr ra, 20(ra)
                0x00b5_0663, // beq a0, a1, +12
                0x0080_00ef, // jal ra, +8
            ]
        );
        assert_eq!(image.u16(TEXT + 40), 0xc111); // c.beqz a0, +4
        assert_eq!(image.u16(TEXT + 42), 0xa009); // c.j +2
        assert_eq!(image.u64(0x40_1000), 0x40_00dc);
        assert_eq!(image.u32(0x40_1008), 0x40_00dc);
    }

    // ---- values that do not fit ------------------------------------------------------------------

    /// The link fails with exactly `expected`: its kind and its whole message.
    fn assert_link_error(result: Result<Vec<u8>, ProgramLinkError>, expected: ProgramLinkError) {
        match result {
            Err(error) => assert_eq!(error, expected),
            Ok(_) => panic!("expected {expected:?}, but the link succeeded"),
        }
    }

    fn out_of_range(what: &str) -> ProgramLinkError {
        ProgramLinkError::RelocationOutOfRange(what.to_string())
    }

    #[test]
    fn an_x86_64_pc32_beyond_2gib_is_out_of_range() {
        let (mut object, text) = x86_64_start(&[0xe8, 0, 0, 0, 0]);
        let here = object.local(".Lhere", text, 0);
        object.reloc(text, 1, here, 0x1_0000_0000, 2);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            out_of_range("PC32: value 0xffffffff at 0x4000b1"),
        );
    }

    #[test]
    fn an_aarch64_call_beyond_128mib_is_out_of_range() {
        let mut object = Obj::new(Arch::Aarch64);
        let text = object.text_words(&[0x9400_0000]);
        let start = object.define("_start", text, 0);
        object.reloc(text, 0, start, 0x800_0000, 283);
        assert_link_error(
            link(Arch::Aarch64, &[&object]),
            out_of_range("CALL26: value 0x8000000 at 0x4000b0"),
        );
    }

    /// `ldr x0, [x1, :lo12:var]` can only encode a multiple of 8: an address that is not is refused
    /// (as `ld.lld` does) rather than truncated to the doubleword below it.
    #[test]
    fn an_aarch64_load_from_a_misaligned_address_is_refused() {
        for (r_type, misaligned_by, what, alignment) in [
            (284, 1, "LDST16_ABS_LO12_NC", 2),
            (285, 2, "LDST32_ABS_LO12_NC", 4),
            (286, 4, "LDST64_ABS_LO12_NC", 8),
            (299, 8, "LDST128_ABS_LO12_NC", 16),
        ] {
            let mut object = Obj::new(Arch::Aarch64);
            let text = object.text_words(&[0xf940_0020]);
            let data = object.section(".data", SectionKind::Data, &[0; 32], 16);
            object.define("_start", text, 0);
            let var = object.define("var", data, 0);
            object.reloc(text, 0, var, misaligned_by, r_type);
            assert_link_error(
                link(Arch::Aarch64, &[&object]),
                out_of_range(&format!(
                    "{what}: target is not {alignment}-byte aligned: value {:#x} at 0x4000b0",
                    0x40_1000 + misaligned_by
                )),
            );
        }
    }

    #[test]
    fn a_riscv64_jal_beyond_1mib_is_out_of_range() {
        let mut object = Obj::new(Arch::Riscv64);
        let text = object.text_words(&[0x0000_00ef]);
        let start = object.define("_start", text, 0);
        object.reloc(text, 0, start, 0x10_0000, 17);
        assert_link_error(
            link(Arch::Riscv64, &[&object]),
            out_of_range("JAL: value 0x100000 at 0x4000b0"),
        );
    }

    /// `hi20` is `(value + 0x800) >> 12`, so a value within 0x800 below 2 GiB fits 32 bits but not
    /// the `lui`/`auipc` + 12-bit pair; it must be refused rather than wrap to a negative address.
    #[test]
    fn a_riscv64_hi20_that_carries_past_2gib_is_out_of_range() {
        for (r_type, what) in [(26, "HI20"), (23, "PCREL_HI20"), (19, "CALL")] {
            let mut object = Obj::new(Arch::Riscv64);
            let text = object.text_words(&[0x0000_0537, 0x0000_0013]);
            let start = object.define("_start", text, 0);
            // The relocated value is 0x7fff_ff00: `S + A` for the absolute kind, `S + A - P` (with
            // `S == P`) for the PC-relative ones.
            let addend = match r_type {
                26 => 0x7fff_ff00 - TEXT as i64,
                _ => 0x7fff_ff00,
            };
            object.reloc(text, 0, start, addend, r_type);
            assert_link_error(
                link(Arch::Riscv64, &[&object]),
                out_of_range(&format!("{what}: value 0x7fffff00 at {TEXT:#x}")),
            );
        }
    }

    // ---- extreme addends ---------------------------------------------------------------------------
    //
    // `S` is a 64-bit address and `A` any 64-bit addend, so `S + A` and `S + A - P` need 66 bits.
    // Computed in 64, a value 2^64 away from one that fits wraps onto it and passes the range
    // check with the wrong patch. Each case below is one of those: an absolute symbol at `2^63`
    // with `A = i64::MAX` is `2^64 - 1`, which 64-bit wrapping reads as `-1`, and one at
    // `2^63 + 1 + P + 0x10` with the same addend is `S + A - P = 2^64 + 0x10`, read as `0x10`. The
    // others are the other end: `A = i64::MIN` whose exact value does fit is still accepted.

    /// Where a relocation `S + A - P` must land 2^64 past `0x10`: `S` for a site at `p`.
    fn wraps_to_0x10_from(p: u64) -> u64 {
        (1u64 << 63) + 1 + p + 0x10
    }

    const TWO_TO_THE_64_PLUS_0X10: &str = "0x10000000000000010";
    const TWO_TO_THE_64_MINUS_1: &str = "0xffffffffffffffff";

    /// `u64::MAX + i64::MAX`: past what a 64-bit field holds read either way. Wrapped in 64 bits
    /// it is `i64::MAX - 1`, which the field would take.
    const PAST_64_BITS: &str = "0x17ffffffffffffffe";

    /// The absolute symbol and the value its `S + A` renders as for a field `width` bytes wide:
    /// `2^63` for a narrower one, where `2^64 - 1` is already out of range, and `u64::MAX` for a
    /// 64-bit one.
    fn beyond_the_field(width: u64) -> (u64, &'static str) {
        if width == 8 {
            (u64::MAX, PAST_64_BITS)
        } else {
            (1 << 63, TWO_TO_THE_64_MINUS_1)
        }
    }

    #[test]
    fn an_x86_64_value_2_to_the_64_away_from_fitting_is_out_of_range() {
        let p = TEXT + 1;
        let (mut object, text) = x86_64_start(&[0xe8, 0, 0, 0, 0]);
        let far = object.absolute("far", wraps_to_0x10_from(p));
        object.reloc(text, 1, far, i64::MAX, 2); // R_X86_64_PC32
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            out_of_range(&format!("PC32: value {TWO_TO_THE_64_PLUS_0X10} at {p:#x}")),
        );

        for (r_type, what, width) in [(11, "32S", 4), (1, "64", 8)] {
            let (mut object, _) = x86_64_start(&[0xc3]);
            let data = object.section(".data", SectionKind::Data, &[0; 8], 8);
            let (value, rendered) = beyond_the_field(width);
            let far = object.absolute("far", value);
            object.reloc(data, 8 - width, far, i64::MAX, r_type);
            assert_link_error(
                link(Arch::X86_64, &[&object]),
                out_of_range(&format!(
                    "{what}: value {rendered} at {:#x}",
                    0x40_1000 + 8 - width
                )),
            );
        }
    }

    #[test]
    fn an_x86_64_extreme_negative_addend_that_fits_is_applied() {
        let (mut object, _) = x86_64_start(&[0xc3]);
        let data = object.section(".data", SectionKind::Data, &[0; 16], 8);
        let origin = object.absolute("origin", 0x10);
        let below = object.absolute("below", (1 << 63) - 0x10);
        object.reloc(data, 0, origin, i64::MIN, 1); // R_X86_64_64: 0x10 - 2^63
        object.reloc(data, 8, below, i64::MIN, 11); // R_X86_64_32S: -0x10
        let bytes = linked(Arch::X86_64, &[&object]);
        let image = Image(&bytes);
        assert_eq!(image.u64(0x40_1000), 0x8000_0000_0000_0010);
        assert_eq!(image.u32(0x40_1008), 0xffff_fff0);
    }

    #[test]
    fn an_aarch64_value_2_to_the_64_away_from_fitting_is_out_of_range() {
        let mut object = Obj::new(Arch::Aarch64);
        let text = object.text_words(&[0x9400_0000]);
        object.define("_start", text, 0);
        let far = object.absolute("far", wraps_to_0x10_from(TEXT));
        object.reloc(text, 0, far, i64::MAX, 283); // R_AARCH64_CALL26
        assert_link_error(
            link(Arch::Aarch64, &[&object]),
            out_of_range(&format!(
                "CALL26: value {TWO_TO_THE_64_PLUS_0X10} at {TEXT:#x}"
            )),
        );

        let mut object = Obj::new(Arch::Aarch64);
        let text = object.text_words(&[0xd65f_03c0]);
        object.define("_start", text, 0);
        let data = object.section(".data", SectionKind::Data, &[0; 8], 8);
        let top = object.absolute("top", u64::MAX);
        object.reloc(data, 0, top, i64::MAX, 257); // R_AARCH64_ABS64
        assert_link_error(
            link(Arch::Aarch64, &[&object]),
            out_of_range(&format!("ABS64: value {PAST_64_BITS} at 0x401000")),
        );
    }

    #[test]
    fn an_aarch64_extreme_negative_addend_that_fits_is_applied() {
        let mut object = Obj::new(Arch::Aarch64);
        let text = object.text_words(&[0xd65f_03c0]);
        object.define("_start", text, 0);
        let data = object.section(".data", SectionKind::Data, &[0; 8], 8);
        let origin = object.absolute("origin", 0x10);
        object.reloc(data, 0, origin, i64::MIN, 257); // R_AARCH64_ABS64: 0x10 - 2^63
        let bytes = linked(Arch::Aarch64, &[&object]);
        assert_eq!(Image(&bytes).u64(0x40_1000), 0x8000_0000_0000_0010);
    }

    #[test]
    fn a_riscv64_value_2_to_the_64_away_from_fitting_is_out_of_range() {
        let mut object = Obj::new(Arch::Riscv64);
        let text = object.text_words(&[0x0000_00ef]);
        object.define("_start", text, 0);
        let far = object.absolute("far", wraps_to_0x10_from(TEXT));
        object.reloc(text, 0, far, i64::MAX, 17); // R_RISCV_JAL
        assert_link_error(
            link(Arch::Riscv64, &[&object]),
            out_of_range(&format!(
                "JAL: value {TWO_TO_THE_64_PLUS_0X10} at {TEXT:#x}"
            )),
        );

        let mut object = Obj::new(Arch::Riscv64);
        let text = object.text_words(&[0x0000_0537, 0x0000_8067]);
        object.define("_start", text, 0);
        let half = object.absolute("half", 1 << 63);
        object.reloc(text, 0, half, i64::MAX, 26); // R_RISCV_HI20
        assert_link_error(
            link(Arch::Riscv64, &[&object]),
            out_of_range(&format!("HI20: value {TWO_TO_THE_64_MINUS_1} at {TEXT:#x}")),
        );

        for (r_type, what, width) in [(1, "32", 4), (2, "64", 8)] {
            let mut object = Obj::new(Arch::Riscv64);
            let text = object.text_words(&[0x0000_8067]);
            object.define("_start", text, 0);
            let data = object.section(".data", SectionKind::Data, &[0; 8], 8);
            let (value, rendered) = beyond_the_field(width);
            let far = object.absolute("far", value);
            object.reloc(data, 8 - width, far, i64::MAX, r_type);
            assert_link_error(
                link(Arch::Riscv64, &[&object]),
                out_of_range(&format!(
                    "{what}: value {rendered} at {:#x}",
                    0x40_1000 + 8 - width
                )),
            );
        }
    }

    #[test]
    fn a_riscv64_extreme_negative_addend_that_fits_is_applied() {
        let mut object = Obj::new(Arch::Riscv64);
        let text = object.text_words(&[0x0000_8067]);
        object.define("_start", text, 0);
        let data = object.section(".data", SectionKind::Data, &[0; 8], 8);
        let origin = object.absolute("origin", 0x10);
        object.reloc(data, 0, origin, i64::MIN, 2); // R_RISCV_64: 0x10 - 2^63
        let bytes = linked(Arch::Riscv64, &[&object]);
        assert_eq!(Image(&bytes).u64(0x40_1000), 0x8000_0000_0000_0010);
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

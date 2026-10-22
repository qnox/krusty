//! The stubs every call through a module interface member goes to, written by the link from the
//! implementation records of every object (see `native::interface_calls`).
//!
//! Only the link sees every type of the program, so only the link can tell how many functions
//! implement a member. A member one function implements needs no stub: its stub symbol is bound to
//! that function, so every call to it is a direct call. A member two functions implement, where one
//! of them is reached from a single type, gets a stub that compares that type and jumps directly
//! either way. Any other member gets a stub making the interface table lookup a call made before:
//! `receiver.type.itable[number]`. A stub keeps the caller's arguments and return address as they
//! are and jumps, so the implementation returns straight to the caller.

use crate::fx_hash::{HashMap, HashSet};

use object::read::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
use object::{SymbolIndex, SymbolSection};

use super::super::interface_calls::{
    recorded_stub, stub_number, ITABLE_OFFSET, RECEIVER_TYPE_OFFSET, RECORD_SIZE,
};
use super::super::target::Arch;
use super::elf::{symbol_name, Elf};
use super::relocate::split_hi_lo;
use super::ProgramLinkError;

/// Each stub starts on this boundary.
const STUB_ALIGN: u64 = 16;

/// A symbol a record names: a global by name, a local by its object and index. Two references to
/// one function are one implementation exactly when they are the same `Target`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Target {
    Global(String),
    Local(usize, SymbolIndex),
}

/// What one stub does, between addresses of the linked image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The one implementation in the program.
    Direct(u64),
    /// `ty` is the only type reaching `then`; every other type reaches `otherwise`.
    Compare { ty: u64, then: u64, otherwise: u64 },
    /// The interface table lookup.
    Table,
}

/// The stub for implementations given as `(type, function)` addresses, one pair per type.
///
/// It is decided on addresses, after layout, so one function reached under two names (a local
/// symbol in the file that defines it and the module symbol another file imports it by) counts
/// once.
fn choose(records: &[(u64, u64)]) -> Kind {
    let mut functions: Vec<(u64, Vec<u64>)> = Vec::new();
    for &(ty, function) in records {
        match functions.iter_mut().find(|(known, _)| *known == function) {
            Some((_, types)) => types.push(ty),
            None => functions.push((function, vec![ty])),
        }
    }
    match functions.as_slice() {
        [(only, _)] => Kind::Direct(*only),
        [(first, first_types), (second, second_types)] => {
            match (first_types.as_slice(), second_types.as_slice()) {
                ([ty], _) => Kind::Compare {
                    ty: *ty,
                    then: *first,
                    otherwise: *second,
                },
                (_, [ty]) => Kind::Compare {
                    ty: *ty,
                    then: *second,
                    otherwise: *first,
                },
                _ => Kind::Table,
            }
        }
        _ => Kind::Table,
    }
}

#[derive(Debug)]
struct Stub {
    name: String,
    number: u32,
    /// Each type with an instance whose interface table holds a function at `number`, and that
    /// function.
    records: Vec<(Target, Target)>,
    /// Where it starts, as an offset from the start of the stubs.
    offset: u64,
    /// What it does, once [`InterfaceStubs::decide`] has seen the addresses.
    kind: Option<Kind>,
}

/// Every interface stub the program's objects call, in a deterministic order.
pub(super) struct InterfaceStubs {
    stubs: Vec<Stub>,
    size: u64,
}

impl InterfaceStubs {
    /// Read the stubs `files` call and the records that decide each one.
    pub(super) fn collect(files: &[Elf], arch: Arch) -> Result<Self, ProgramLinkError> {
        let mut called: HashSet<String> = HashSet::default();
        let mut defined: HashSet<&str> = HashSet::default();
        for file in files {
            for symbol in file.symbols() {
                if symbol.is_local() {
                    continue;
                }
                let name = symbol_name(&symbol)?;
                if stub_number(name).is_none() {
                    continue;
                }
                if symbol.section() == SymbolSection::Undefined {
                    called.insert(name.to_string());
                } else {
                    defined.insert(name);
                }
            }
        }
        if let Some(name) = defined.into_iter().next() {
            return Err(ProgramLinkError::DuplicateSymbol(format!(
                "{name} (an interface stub, which only the link defines)"
            )));
        }
        let mut records: HashMap<String, Vec<(Target, Target)>> = HashMap::default();
        for (file_index, file) in files.iter().enumerate() {
            for section in file.sections() {
                let Some(name) = section.name().ok() else {
                    continue;
                };
                if recorded_stub(name, 0).is_none() {
                    continue;
                }
                let size = section.size();
                if size % RECORD_SIZE != 0 {
                    return Err(ProgramLinkError::Parse(format!(
                        "input {file_index} has an interface implementation record section of \
                         {size} bytes, which is not a whole number of records"
                    )));
                }
                let data = section
                    .data()
                    .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
                let mut fields: HashMap<u64, Target> = HashMap::default();
                for (offset, relocation) in section.relocations() {
                    let RelocationTarget::Symbol(index) = relocation.target() else {
                        return Err(record_error(file_index, offset, "names no symbol"));
                    };
                    if relocation.addend() != 0
                        || offset % 8 != 0
                        || offset >= size
                        || offset % RECORD_SIZE == 0
                    {
                        return Err(record_error(file_index, offset, "is not a whole field"));
                    }
                    let symbol = file
                        .symbol_by_index(index)
                        .map_err(|error| ProgramLinkError::Parse(error.to_string()))?;
                    let target = if symbol.is_local() {
                        Target::Local(file_index, index)
                    } else {
                        Target::Global(symbol_name(&symbol)?.to_string())
                    };
                    fields.insert(offset, target);
                }
                for (record, words) in data.chunks_exact(RECORD_SIZE as usize).enumerate() {
                    let at = record as u64 * RECORD_SIZE;
                    let field = |offset: u64| {
                        fields
                            .get(&(at + offset))
                            .cloned()
                            .ok_or_else(|| record_error(file_index, at + offset, "is empty"))
                    };
                    let number = u64::from_le_bytes(words[..8].try_into().expect("a word"));
                    let stub = u32::try_from(number)
                        .ok()
                        .and_then(|number| recorded_stub(name, number))
                        .ok_or_else(|| record_error(file_index, at, "names no member number"))?;
                    records
                        .entry(stub)
                        .or_default()
                        .push((field(8)?, field(16)?));
                }
            }
        }
        let mut names: Vec<String> = called.into_iter().collect();
        names.sort();
        // Every stub keeps room for the longer form, so the form can be chosen once addresses are
        // known.
        let room = align_up(
            [
                Kind::Compare {
                    ty: 0,
                    then: 0,
                    otherwise: 0,
                },
                Kind::Table,
            ]
            .iter()
            .map(|kind| code_size(arch, kind))
            .max()
            .expect("two forms"),
            STUB_ALIGN,
        );
        let stubs = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| Stub {
                number: stub_number(&name).expect("collected as a stub"),
                records: records.remove(&name).unwrap_or_default(),
                name,
                offset: index as u64 * room,
                kind: None,
            })
            .collect::<Vec<_>>();
        let size = stubs.len() as u64 * room;
        Ok(Self { stubs, size })
    }

    /// The bytes the stubs take, from a `STUB_ALIGN` boundary.
    pub(super) fn size(&self) -> u64 {
        self.size
    }

    /// Choose each stub's form from the addresses `address` resolves what the records name to,
    /// and return the address every stub symbol is bound to, for the stubs placed at `at`: the
    /// implementation itself where one function answers, the stub's code otherwise.
    pub(super) fn decide(
        &mut self,
        at: u64,
        address: impl Fn(&Target) -> Result<u64, ProgramLinkError>,
    ) -> Result<Vec<(String, u64)>, ProgramLinkError> {
        let mut bound = Vec::with_capacity(self.stubs.len());
        for stub in &mut self.stubs {
            let records = stub
                .records
                .iter()
                .map(|(ty, function)| Ok((address(ty)?, address(function)?)))
                .collect::<Result<Vec<_>, ProgramLinkError>>()?;
            let kind = choose(&records);
            stub.kind = Some(kind);
            let symbol = match kind {
                Kind::Direct(function) => function,
                Kind::Compare { .. } | Kind::Table => at + stub.offset,
            };
            bound.push((stub.name.clone(), symbol));
        }
        Ok(bound)
    }

    /// Write every stub [`InterfaceStubs::decide`] gave code into `image`, the stubs starting at
    /// file offset `offset` and address `at`.
    pub(super) fn write(
        &self,
        image: &mut [u8],
        offset: u64,
        at: u64,
        arch: Arch,
    ) -> Result<(), ProgramLinkError> {
        for stub in &self.stubs {
            let pc = at + stub.offset;
            let kind = stub.kind.expect("decided before the stubs are written");
            let code = match kind {
                Kind::Direct(_) => continue,
                Kind::Compare {
                    ty,
                    then,
                    otherwise,
                } => compare(arch, pc, ty, then, otherwise),
                Kind::Table => table(arch, stub.number),
            }
            .ok_or_else(|| {
                ProgramLinkError::RelocationOutOfRange(format!(
                    "the interface stub `{}` at {pc:#x} cannot reach its implementation",
                    stub.name
                ))
            })?;
            debug_assert_eq!(code.len() as u64, code_size(arch, &kind));
            let start = (offset + stub.offset) as usize;
            image[start..start + code.len()].copy_from_slice(&code);
        }
        Ok(())
    }
}

fn record_error(file_index: usize, offset: u64, why: &str) -> ProgramLinkError {
    ProgramLinkError::Parse(format!(
        "input {file_index}: the interface implementation record field at {offset:#x} {why}"
    ))
}

fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

fn code_size(arch: Arch, kind: &Kind) -> u64 {
    match (arch, kind) {
        // A direct call needs no stub: the stub symbol is bound to the implementation.
        (_, Kind::Direct(_)) => 0,
        (Arch::X86_64, Kind::Compare { .. }) => 25,
        (Arch::X86_64, Kind::Table) => 21,
        (Arch::Aarch64, Kind::Compare { .. }) => 44,
        (Arch::Aarch64, Kind::Table) => 24,
        (Arch::Riscv64, Kind::Compare { .. }) => 32,
        (Arch::Riscv64, Kind::Table) => 28,
    }
}

/// The displacement from `from` to `to`, if a signed 32-bit field holds it.
fn rel32(from: u64, to: u64) -> Option<i32> {
    i32::try_from(i128::from(to) - i128::from(from)).ok()
}

fn words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// AArch64 `adrp x16, to; add x16, x16, :lo12:to`, placed at `pc`.
fn aarch64_address_x16(pc: u64, to: u64) -> Option<[u32; 2]> {
    let pages = (i128::from(to & !0xfff) - i128::from(pc & !0xfff)) >> 12;
    if !(-(1 << 20)..(1 << 20)).contains(&pages) {
        return None;
    }
    let pages = pages as u32;
    let adrp = 0x9000_0010 | (pages & 3) << 29 | ((pages >> 2) & 0x7_ffff) << 5;
    let add = 0x9100_0210 | ((to & 0xfff) as u32) << 10;
    Some([adrp, add])
}

/// RISC-V `auipc t3, %pcrel_hi(to)` at `pc`, and the low twelve bits its partner adds.
fn riscv_pcrel_t3(pc: u64, to: u64) -> Option<(u32, u32)> {
    let distance = i128::from(to) - i128::from(pc);
    if !(-(1i128 << 31) + 0x800..(1i128 << 31) - 0x800).contains(&distance) {
        return None;
    }
    let (hi, lo) = split_hi_lo(distance);
    Some((0x0000_0e17 | hi << 12, lo))
}

const AARCH64_BR_X16: u32 = 0xd61f_0200;
const RISCV_JR_T3: u32 = 0x000e_0067;

/// A jump to `then` when the receiver's type is `ty`, and to `otherwise` when it is not, from a
/// stub at `pc`. The receiver is the first argument; the caller has checked it is not null.
fn compare(arch: Arch, pc: u64, ty: u64, then: u64, otherwise: u64) -> Option<Vec<u8>> {
    Some(match arch {
        Arch::X86_64 => {
            // lea r11, [rip + ty]; cmp r11, [rdi + type]; je then; jmp otherwise
            let mut code = vec![0x4c, 0x8d, 0x1d];
            code.extend_from_slice(&rel32(pc + 7, ty)?.to_le_bytes());
            code.extend_from_slice(&[0x4c, 0x3b, 0x9f]);
            code.extend_from_slice(&RECEIVER_TYPE_OFFSET.to_le_bytes());
            code.extend_from_slice(&[0x0f, 0x84]);
            code.extend_from_slice(&rel32(pc + 20, then)?.to_le_bytes());
            code.push(0xe9);
            code.extend_from_slice(&rel32(pc + 25, otherwise)?.to_le_bytes());
            code
        }
        Arch::Aarch64 => {
            // ldr x17, [x0, #type]; adrp x16, ty; add x16, x16, :lo12:ty; cmp x16, x17;
            // b.ne 1f; adrp x16, then; add x16, x16, :lo12:then; br x16;
            // 1: adrp x16, otherwise; add x16, x16, :lo12:otherwise; br x16
            let [ty_page, ty_low] = aarch64_address_x16(pc + 4, ty)?;
            let [then_page, then_low] = aarch64_address_x16(pc + 20, then)?;
            let [other_page, other_low] = aarch64_address_x16(pc + 32, otherwise)?;
            words(&[
                0xf940_0011 | (RECEIVER_TYPE_OFFSET / 8) << 10,
                ty_page,
                ty_low,
                0xeb11_021f,
                0x5400_0001 | (16 / 4) << 5,
                then_page,
                then_low,
                AARCH64_BR_X16,
                other_page,
                other_low,
                AARCH64_BR_X16,
            ])
        }
        Arch::Riscv64 => {
            // ld t4, type(a0); auipc t3, %pcrel_hi(ty); addi t3, t3, %pcrel_lo(ty);
            // bne t3, t4, 1f; auipc t3, %pcrel_hi(then); jalr zero, %pcrel_lo(then)(t3);
            // 1: auipc t3, %pcrel_hi(otherwise); jalr zero, %pcrel_lo(otherwise)(t3)
            let (ty_hi, ty_lo) = riscv_pcrel_t3(pc + 4, ty)?;
            let (then_hi, then_lo) = riscv_pcrel_t3(pc + 16, then)?;
            let (other_hi, other_lo) = riscv_pcrel_t3(pc + 24, otherwise)?;
            words(&[
                0x0005_3e83 | RECEIVER_TYPE_OFFSET << 20,
                ty_hi,
                0x000e_0e13 | ty_lo << 20,
                // bne t3, t4, +12
                0x01de_1063 | ((12 >> 1) & 0xf) << 8,
                then_hi,
                RISCV_JR_T3 | then_lo << 20,
                other_hi,
                RISCV_JR_T3 | other_lo << 20,
            ])
        }
    })
}

/// The interface table lookup `receiver.type.itable[number]`, jumped through.
fn table(arch: Arch, number: u32) -> Option<Vec<u8>> {
    let entry = number.checked_mul(8)?;
    Some(match arch {
        Arch::X86_64 => {
            // mov r11, [rdi + type]; mov r11, [r11 + itable]; jmp [r11 + 8 * number]
            let entry = i32::try_from(entry).ok()?;
            let mut code = vec![0x4c, 0x8b, 0x9f];
            code.extend_from_slice(&RECEIVER_TYPE_OFFSET.to_le_bytes());
            code.extend_from_slice(&[0x4d, 0x8b, 0x9b]);
            code.extend_from_slice(&ITABLE_OFFSET.to_le_bytes());
            code.extend_from_slice(&[0x41, 0xff, 0xa3]);
            code.extend_from_slice(&entry.to_le_bytes());
            code
        }
        Arch::Aarch64 => {
            // ldr x16, [x0, #type]; ldr x16, [x16, #itable]; mov x17, #(8 * number);
            // ldr x16, [x16, x17]; br x16
            words(&[
                0xf940_0010 | (RECEIVER_TYPE_OFFSET / 8) << 10,
                0xf940_0210 | (ITABLE_OFFSET / 8) << 10,
                0xd280_0011 | (entry & 0xffff) << 5,
                0xf2a0_0011 | (entry >> 16) << 5,
                0xf871_6a10,
                AARCH64_BR_X16,
            ])
        }
        Arch::Riscv64 => {
            // ld t3, type(a0); ld t3, itable(t3); lui t4, %hi(8 * number);
            // add t3, t3, t4; ld t3, %lo(8 * number)(t3); jalr zero, 0(t3)
            let (hi, lo) = split_hi_lo(i128::from(entry));
            words(&[
                0x0005_3e03 | RECEIVER_TYPE_OFFSET << 20,
                0x000e_3e03 | ITABLE_OFFSET << 20,
                0x0000_0eb7 | hi << 12,
                0x01de_0e33,
                0x000e_3e03 | lo << 20,
                RISCV_JR_T3,
                0x0000_0013,
            ])
        }
    })
}

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::super::super::interface_calls::implementations_section;
    use super::super::fixture::{assert_link_error, link, linked, Image, Obj};
    use super::*;

    const PC: u64 = 0x40_1000;

    /// Each form on each architecture, as LLVM's disassembler reads it (`llvm-mc --disassemble`).
    #[test]
    fn each_form_is_encoded_for_each_architecture() {
        let cases: [(Arch, Kind, &[u8]); 6] = [
            // lea r11, [rip + 0xf001]; cmp r11, [rdi]; je 0x402010; jmp 0x403020
            (
                Arch::X86_64,
                Kind::Compare {
                    ty: 0x41_0008,
                    then: 0x40_2010,
                    otherwise: 0x40_3020,
                },
                &[
                    0x4c, 0x8d, 0x1d, 0x01, 0xf0, 0x00, 0x00, 0x4c, 0x3b, 0x9f, 0x00, 0x00, 0x00,
                    0x00, 0x0f, 0x84, 0xfc, 0x0f, 0x00, 0x00, 0xe9, 0x07, 0x20, 0x00, 0x00,
                ],
            ),
            // mov r11, [rdi]; mov r11, [r11 + 152]; jmp [r11 + 0x100018]
            (
                Arch::X86_64,
                Kind::Table,
                &[
                    0x4c, 0x8b, 0x9f, 0x00, 0x00, 0x00, 0x00, 0x4d, 0x8b, 0x9b, 0x98, 0x00, 0x00,
                    0x00, 0x41, 0xff, 0xa3, 0x18, 0x00, 0x10, 0x00,
                ],
            ),
            // ldr x17, [x0]; adrp x16, 0x410000; add x16, x16, #8; cmp x16, x17; b.ne +16;
            // adrp x16, 0x402000; add x16, x16, #0x10; br x16;
            // adrp x16, 0x403000; add x16, x16, #0x20; br x16
            (
                Arch::Aarch64,
                Kind::Compare {
                    ty: 0x41_0008,
                    then: 0x40_2010,
                    otherwise: 0x40_3020,
                },
                &[
                    0x11, 0x00, 0x40, 0xf9, 0x70, 0x00, 0x00, 0xf0, 0x10, 0x22, 0x00, 0x91, 0x1f,
                    0x02, 0x11, 0xeb, 0x81, 0x00, 0x00, 0x54, 0x10, 0x00, 0x00, 0xb0, 0x10, 0x42,
                    0x00, 0x91, 0x00, 0x02, 0x1f, 0xd6, 0x10, 0x00, 0x00, 0xd0, 0x10, 0x82, 0x00,
                    0x91, 0x00, 0x02, 0x1f, 0xd6,
                ],
            ),
            // ldr x16, [x0]; ldr x16, [x16, #152]; mov x17, #0x18; movk x17, #0x10, lsl #16;
            // ldr x16, [x16, x17]; br x16
            (
                Arch::Aarch64,
                Kind::Table,
                &[
                    0x10, 0x00, 0x40, 0xf9, 0x10, 0x4e, 0x40, 0xf9, 0x11, 0x03, 0x80, 0xd2, 0x11,
                    0x02, 0xa0, 0xf2, 0x10, 0x6a, 0x71, 0xf8, 0x00, 0x02, 0x1f, 0xd6,
                ],
            ),
            // ld t4, 0(a0); auipc t3, 15; addi t3, t3, 4; bne t3, t4, +12;
            // auipc t3, 1; jr t3; auipc t3, 2; jr 8(t3)
            (
                Arch::Riscv64,
                Kind::Compare {
                    ty: 0x41_0008,
                    then: 0x40_2010,
                    otherwise: 0x40_3020,
                },
                &[
                    0x83, 0x3e, 0x05, 0x00, 0x17, 0xfe, 0x00, 0x00, 0x13, 0x0e, 0x4e, 0x00, 0x63,
                    0x16, 0xde, 0x01, 0x17, 0x1e, 0x00, 0x00, 0x67, 0x00, 0x0e, 0x00, 0x17, 0x2e,
                    0x00, 0x00, 0x67, 0x00, 0x8e, 0x00,
                ],
            ),
            // ld t3, 0(a0); ld t3, 152(t3); lui t4, 256; add t3, t3, t4; ld t3, 24(t3); jr t3; nop
            (
                Arch::Riscv64,
                Kind::Table,
                &[
                    0x03, 0x3e, 0x05, 0x00, 0x03, 0x3e, 0x8e, 0x09, 0xb7, 0x0e, 0x10, 0x00, 0x33,
                    0x0e, 0xde, 0x01, 0x03, 0x3e, 0x8e, 0x01, 0x67, 0x00, 0x0e, 0x00, 0x13, 0x00,
                    0x00, 0x00,
                ],
            ),
        ];
        for (arch, kind, expected) in cases {
            let code = match kind {
                Kind::Direct(_) => unreachable!("a direct call has no stub"),
                Kind::Compare {
                    ty,
                    then,
                    otherwise,
                } => compare(arch, PC, ty, then, otherwise),
                Kind::Table => table(arch, 0x2_0003),
            }
            .expect("in reach");
            assert_eq!(code, expected, "{arch:?} {kind:?}");
            assert_eq!(
                code.len() as u64,
                code_size(arch, &kind),
                "{arch:?} {kind:?}"
            );
        }
    }

    #[test]
    fn a_target_out_of_reach_is_none() {
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            assert_eq!(compare(arch, 0, 1 << 33, 0, 0), None, "{arch:?}");
        }
    }

    #[test]
    fn the_form_follows_the_functions_and_the_types_reaching_them() {
        // One function, from any number of types.
        assert_eq!(choose(&[(1, 10), (2, 10), (3, 10)]), Kind::Direct(10));
        // Two functions, one of which a single type reaches.
        assert_eq!(
            choose(&[(1, 10), (2, 10), (3, 20)]),
            Kind::Compare {
                ty: 3,
                then: 20,
                otherwise: 10
            }
        );
        assert_eq!(
            choose(&[(1, 10), (2, 20)]),
            Kind::Compare {
                ty: 1,
                then: 10,
                otherwise: 20
            }
        );
        // Two functions each shared by two types, three functions, and none at all.
        assert_eq!(choose(&[(1, 10), (2, 10), (3, 20), (4, 20)]), Kind::Table);
        assert_eq!(choose(&[(1, 10), (2, 20), (3, 30)]), Kind::Table);
        assert_eq!(choose(&[]), Kind::Table);
    }

    const R_X86_64_64: u32 = 1;
    const R_X86_64_PLT32: u32 = 4;

    /// A program whose `_start` calls stub `kt_itf_m_0` (`call stub; ret`), with two functions
    /// `f` and `g` and three types, and a record for each `(type, function)` given.
    fn program(records: &[(usize, usize)]) -> Obj {
        let numbered: Vec<_> = records.iter().map(|&(ty, f)| (0, ty, f)).collect();
        numbered_program(&numbered)
    }

    /// `program`, with each record naming its member number: `(number, type, function)`.
    fn numbered_program(records: &[(u64, usize, usize)]) -> Obj {
        let (mut object, text) = super::super::fixture::x86_64_start(&[
            0xe8, 0, 0, 0, 0, 0xc3, 0xcc, 0xcc, // _start
            0xc3, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, // f
            0xc3, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, // g
        ]);
        let stub = object.undefined("kt_itf_m_0");
        object.reloc(text, 1, stub, -4, R_X86_64_PLT32);
        let functions = [object.local("f", text, 8), object.local("g", text, 16)];
        let mut marks = [0xa_u8; 24];
        marks[8..16].fill(0xb);
        marks[16..].fill(0xc);
        let data = object.section(".rodata", SectionKind::ReadOnlyData, &marks, 8);
        let types = [
            object.local("type_a", data, 0),
            object.local("type_b", data, 8),
            object.local("type_c", data, 16),
        ];
        let mut contents = vec![0; records.len() * RECORD_SIZE as usize];
        for (record, &(number, _, _)) in
            contents.chunks_exact_mut(RECORD_SIZE as usize).zip(records)
        {
            record[..8].copy_from_slice(&number.to_le_bytes());
        }
        let section = object.section(
            &implementations_section("m"),
            SectionKind::ReadOnlyDataWithRel,
            &contents,
            8,
        );
        for (index, &(_, ty, function)) in records.iter().enumerate() {
            let at = index as u64 * RECORD_SIZE;
            object.reloc(section, at + 8, types[ty], 0, R_X86_64_64);
            object.reloc(section, at + 16, functions[function], 0, R_X86_64_64);
        }
        object
    }

    /// Where `_start`'s call lands, and the `len` bytes there.
    fn stub_of(image: &[u8], len: usize) -> (u64, Vec<u8>) {
        let image = Image(image);
        let call = image.entry();
        let displacement = image.u32(call + 1) as i32;
        let stub = (call as i64 + 5 + i64::from(displacement)) as u64;
        (stub, image.bytes(stub, len).to_vec())
    }

    #[test]
    fn a_member_one_function_implements_is_called_directly() {
        let image = linked(Arch::X86_64, &[&program(&[(0, 0), (1, 0)])]);
        let (target, _) = stub_of(&image, 0);
        assert_eq!(
            target,
            Image(&image).entry() + 8,
            "the call lands on f itself"
        );
    }

    #[test]
    fn a_member_two_functions_implement_compares_the_lone_type() {
        let image = linked(Arch::X86_64, &[&program(&[(0, 0), (1, 0), (2, 1)])]);
        let (stub, code) = stub_of(&image, 25);
        assert_eq!(stub % STUB_ALIGN, 0);
        let entry = Image(&image).entry();
        let at = |field: usize, end: u64| {
            let displacement = i32::from_le_bytes(code[field..field + 4].try_into().unwrap());
            (stub as i64 + end as i64 + i64::from(displacement)) as u64
        };
        assert_eq!(&code[..3], &[0x4c, 0x8d, 0x1d]);
        let ty = at(3, 7);
        assert_eq!(&code[14..16], &[0x0f, 0x84]);
        assert_eq!(at(16, 20), entry + 16, "the lone type reaches g");
        assert_eq!(code[20], 0xe9);
        assert_eq!(at(21, 25), entry + 8, "every other type reaches f");
        assert_eq!(
            Image(&image).bytes(ty, 8),
            [0xc; 8],
            "the lone type is type_c"
        );
    }

    /// Records of another member number say nothing about this member.
    #[test]
    fn a_record_counts_only_for_the_member_it_numbers() {
        let image = linked(
            Arch::X86_64,
            &[&numbered_program(&[(0, 0, 0), (1, 1, 1), (7, 2, 1)])],
        );
        let (target, _) = stub_of(&image, 0);
        assert_eq!(
            target,
            Image(&image).entry() + 8,
            "only f implements member 0, so the call lands on f itself"
        );
    }

    #[test]
    fn a_member_with_no_lone_type_reads_the_interface_table() {
        let image = linked(Arch::X86_64, &[&program(&[])]);
        let (_, code) = stub_of(&image, 21);
        assert_eq!(code, table(Arch::X86_64, 0).unwrap());
    }

    #[test]
    fn the_records_are_not_loaded() {
        let with = linked(Arch::X86_64, &[&program(&[(0, 0), (1, 0), (2, 1)])]);
        let without = linked(Arch::X86_64, &[&program(&[(0, 0)])]);
        let text = |image: &[u8]| Image(image).program_headers()[0].p_filesz;
        assert_eq!(text(&with), text(&without));
    }

    #[test]
    fn an_object_defining_a_stub_is_refused() {
        let (mut object, text) = super::super::fixture::x86_64_start(&[0xc3]);
        object.define("kt_itf_m_0", text, 0);
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            ProgramLinkError::DuplicateSymbol(
                "kt_itf_m_0 (an interface stub, which only the link defines)".into(),
            ),
        );
    }

    #[test]
    fn a_record_section_that_is_not_whole_records_is_refused() {
        let (mut object, _) = super::super::fixture::x86_64_start(&[0xc3]);
        object.section(
            &implementations_section("m"),
            SectionKind::ReadOnlyData,
            &[0; 16],
            8,
        );
        assert_link_error(
            link(Arch::X86_64, &[&object]),
            ProgramLinkError::Parse(
                "input 0 has an interface implementation record section of 16 bytes, which is \
                 not a whole number of records"
                    .into(),
            ),
        );
    }
}

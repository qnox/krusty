//! The stubs every call through a module interface member goes to, written by the link from the
//! implementation records of every object (see `native::interface_calls`).
//!
//! Only the link sees every type of the program, so only the link can tell how many functions
//! implement a member. A member one function implements needs no stub: its stub symbol is bound to
//! that function, so every call to it is a direct call. A member two functions implement, where one
//! of them is reached from a single type, gets a stub that compares that type and jumps directly
//! either way. Any other member gets a stub making the interface table lookup a call made before:
//! `receiver.type.itable[index]`, at the index the link numbers it with (see
//! [`super::interface_tables`]). A stub keeps the caller's arguments and return address as they
//! are and jumps, so the implementation returns straight to the caller.

use crate::fx_hash::HashSet;

use object::read::{Object, ObjectSymbol};
use object::SymbolSection;

use super::super::interface_calls::{stub_number, ITABLE_OFFSET, RECEIVER_TYPE_OFFSET};
use super::super::target::Arch;
use super::elf::{symbol_name, Elf};
pub(super) use super::interface_records::Target;
use super::interface_tables::InterfaceTables;
use super::relocate::split_hi_lo;
use super::ProgramLinkError;

/// Each stub starts on this boundary.
const STUB_ALIGN: u64 = 16;

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

/// Whether implementations `(type, function)` may need a table lookup: what [`choose`] answers
/// before the addresses that can only merge two functions into one are known.
fn may_read_a_table(records: &[(Target, Target)]) -> bool {
    let mut functions: Vec<(&Target, usize)> = Vec::new();
    for (_, function) in records {
        match functions.iter_mut().find(|(known, _)| *known == function) {
            Some((_, types)) => *types += 1,
            None => functions.push((function, 1)),
        }
    }
    match functions.as_slice() {
        [_] => false,
        [(_, first), (_, second)] => *first > 1 && *second > 1,
        _ => true,
    }
}

#[derive(Debug)]
struct Stub {
    name: String,
    /// Each type with an instance that implements the member, and the function it implements it
    /// with.
    records: Vec<(Target, Target)>,
    /// The member's index in the interface tables, where its implementations may need a table.
    index: Option<u32>,
    /// Where it starts, as an offset from the start of the stubs.
    offset: u64,
    /// What it does, once [`InterfaceStubs::decide`] has seen the addresses.
    kind: Option<Kind>,
}

/// Every interface stub the program's objects call, in a deterministic order, and the interface
/// tables the stubs that look a table up read.
pub(super) struct InterfaceStubs {
    stubs: Vec<Stub>,
    size: u64,
    tables: InterfaceTables,
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
        let mut records = super::interface_records::read(files)?;
        let mut names: Vec<String> = called.into_iter().collect();
        names.sort();
        for name in &names {
            records.refuse_unidentified_calls(name)?;
        }
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
        let mut stubs = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| Stub {
                records: records.implementations.remove(&name).unwrap_or_default(),
                name,
                index: None,
                offset: index as u64 * room,
                kind: None,
            })
            .collect::<Vec<_>>();
        let size = stubs.len() as u64 * room;
        // A stub's form is chosen on addresses, which only merge implementations: one that cannot
        // be a table lookup by its records cannot become one. So the members that may need a
        // table are known before layout, and only those are numbered.
        let reads_a_table = stubs
            .iter()
            .map(|stub| may_read_a_table(&stub.records))
            .collect::<Vec<_>>();
        let (tables, indices) = InterfaceTables::number(
            stubs
                .iter()
                .zip(&reads_a_table)
                .filter(|(_, reads)| **reads)
                .map(|(stub, _)| stub.records.as_slice()),
        )?;
        for (stub, index) in stubs
            .iter_mut()
            .zip(&reads_a_table)
            .filter(|(_, reads)| **reads)
            .map(|(stub, _)| stub)
            .zip(indices)
        {
            stub.index = Some(index);
        }
        Ok(Self {
            stubs,
            size,
            tables,
        })
    }

    /// The interface tables the stubs read.
    pub(super) fn tables(&self) -> &InterfaceTables {
        &self.tables
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
                Kind::Table => {
                    let index = stub
                        .index
                        .expect("a table lookup is decided only for a numbered member");
                    table(arch, index)
                }
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

    use super::super::super::interface_calls::{
        identities_section, implementations_section, padded, DEPENDENCY_IMPLEMENTATIONS_SECTION,
        DEPENDENCY_PROPERTIES_SECTION, RECORD_SIZE,
    };
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
            // mov r11, [rdi]; mov r11, [r11 + 160]; jmp [r11 + 0x100018]
            (
                Arch::X86_64,
                Kind::Table,
                &[
                    0x4c, 0x8b, 0x9f, 0x00, 0x00, 0x00, 0x00, 0x4d, 0x8b, 0x9b, 0xa0, 0x00, 0x00,
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
            // ldr x16, [x0]; ldr x16, [x16, #160]; mov x17, #0x18; movk x17, #0x10, lsl #16;
            // ldr x16, [x16, x17]; br x16
            (
                Arch::Aarch64,
                Kind::Table,
                &[
                    0x10, 0x00, 0x40, 0xf9, 0x10, 0x52, 0x40, 0xf9, 0x11, 0x03, 0x80, 0xd2, 0x11,
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
            // ld t3, 0(a0); ld t3, 160(t3); lui t4, 256; add t3, t3, t4; ld t3, 24(t3); jr t3; nop
            (
                Arch::Riscv64,
                Kind::Table,
                &[
                    0x03, 0x3e, 0x05, 0x00, 0x03, 0x3e, 0x0e, 0x0a, 0xb7, 0x0e, 0x10, 0x00, 0x33,
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
        for (record, &(number, _, _)) in contents
            .as_chunks_mut::<{ RECORD_SIZE as usize }>()
            .0
            .iter_mut()
            .zip(records)
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

    /// The byte filling each of `typed_program`'s types before its `itable` field.
    const TYPE_MARKS: [u8; 3] = [0xa1, 0xb2, 0xc3];

    /// A program whose `_start` calls stub `kt_itf_m_0`, with functions `f`, `g` and `h`, three
    /// `KType`-sized types, records `(number, type, function)` of module `m`, identities
    /// `(number, identity)` module `m` names, and implementations `(identity, type, function)`
    /// another module records by identity.
    fn typed_program(
        records: &[(u64, usize, usize)],
        identities: &[(u64, &[u8])],
        dependency: &[(&[u8], usize, usize)],
    ) -> Obj {
        let (mut object, text) = super::super::fixture::x86_64_start(&[
            0xe8, 0, 0, 0, 0, 0xc3, 0xcc, 0xcc, // _start
            0xc3, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, // f
            0xc3, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, // g
            0xc3, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, // h
        ]);
        let stub = object.undefined("kt_itf_m_0");
        object.reloc(text, 1, stub, -4, R_X86_64_PLT32);
        let functions = [
            object.local("f", text, 8),
            object.local("g", text, 16),
            object.local("h", text, 24),
        ];
        const TYPE: u64 = 168;
        let mut marks = Vec::new();
        for mark in TYPE_MARKS {
            marks.extend_from_slice(&[mark; ITABLE_OFFSET as usize]);
            marks.resize(marks.len() + (TYPE - u64::from(ITABLE_OFFSET)) as usize, 0);
        }
        let data = object.section(".rodata", SectionKind::ReadOnlyData, &marks, 8);
        let types = [
            object.local("type_a", data, 0),
            object.local("type_b", data, TYPE),
            object.local("type_c", data, 2 * TYPE),
        ];
        let mut contents = vec![0; records.len() * RECORD_SIZE as usize];
        for (record, &(number, _, _)) in contents
            .as_chunks_mut::<{ RECORD_SIZE as usize }>()
            .0
            .iter_mut()
            .zip(records)
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
        let mut named = Vec::new();
        for &(number, identity) in identities {
            named.extend_from_slice(&number.to_le_bytes());
            named.extend_from_slice(&(identity.len() as u64).to_le_bytes());
            named.extend_from_slice(identity);
            named.resize(
                named.len() + (padded(identity.len() as u64) as usize - identity.len()),
                0,
            );
        }
        object.section(
            &identities_section("m"),
            SectionKind::ReadOnlyData,
            &named,
            8,
        );
        let mut implemented = Vec::new();
        let mut fields = Vec::new();
        for &(identity, ty, function) in dependency {
            fields.push((implemented.len() as u64, ty, function));
            implemented.extend_from_slice(&(identity.len() as u64).to_le_bytes());
            implemented.extend_from_slice(&[0; 16]);
            implemented.extend_from_slice(identity);
            implemented.resize(
                implemented.len() + (padded(identity.len() as u64) as usize - identity.len()),
                0,
            );
        }
        let section = object.section(
            DEPENDENCY_IMPLEMENTATIONS_SECTION,
            SectionKind::ReadOnlyDataWithRel,
            &implemented,
            8,
        );
        for (at, ty, function) in fields {
            object.reloc(section, at + 8, types[ty], 0, R_X86_64_64);
            object.reloc(section, at + 16, functions[function], 0, R_X86_64_64);
        }
        object
    }

    /// A dependent module's type, recorded by the member's identity, is one more implementation
    /// of the member that identity names.
    #[test]
    fn an_implementation_recorded_by_identity_joins_the_member_it_names() {
        let image = linked(
            Arch::X86_64,
            &[&typed_program(
                &[(0, 0, 0)],
                &[(0, b"greet")],
                &[(b"greet", 2, 1)],
            )],
        );
        let (_, code) = stub_of(&image, 25);
        assert_eq!(
            &code[..3],
            &[0x4c, 0x8d, 0x1d],
            "two functions now implement the member, so the stub compares"
        );
    }

    #[test]
    fn an_identity_no_module_names_is_no_implementation() {
        let image = linked(
            Arch::X86_64,
            &[&typed_program(
                &[(0, 0, 0)],
                &[(0, b"greet")],
                &[(b"other", 2, 1)],
            )],
        );
        let (target, _) = stub_of(&image, 0);
        assert_eq!(target, Image(&image).entry() + 8, "only f implements it");
    }

    /// The link writes each type's table and points the type at it; a stub reads the index the
    /// link numbered the member with.
    #[test]
    fn the_link_writes_the_interface_tables() {
        let image = linked(
            Arch::X86_64,
            &[&typed_program(
                &[(0, 0, 0), (0, 1, 1)],
                &[(0, b"greet")],
                &[(b"greet", 2, 2)],
            )],
        );
        let (_, code) = stub_of(&image, 21);
        assert_eq!(code, table(Arch::X86_64, 0).unwrap());
        let entry = Image(&image).entry();
        for (mark, function) in TYPE_MARKS
            .into_iter()
            .zip([entry + 8, entry + 16, entry + 24])
        {
            let ty = image
                .windows(ITABLE_OFFSET as usize)
                .position(|bytes| bytes.iter().all(|&byte| byte == mark))
                .expect("the type is in the image")
                + ITABLE_OFFSET as usize;
            let table = u64::from_le_bytes(image[ty..ty + 8].try_into().unwrap());
            assert_eq!(Image(&image).u64(table), function);
        }
    }

    /// A property accessor's number has no identity, so a call through it is refused once
    /// another module's type implements a property of the module's interfaces.
    #[test]
    fn a_call_through_a_property_another_module_implements_is_refused() {
        let program = |namespaces: &[u8]| {
            let mut object = typed_program(&[(0, 0, 0)], &[(0, b"")], &[]);
            object.section(
                DEPENDENCY_PROPERTIES_SECTION,
                SectionKind::ReadOnlyData,
                namespaces,
                8,
            );
            object
        };
        let named = |namespace: &[u8]| {
            let mut entry = (namespace.len() as u64).to_le_bytes().to_vec();
            entry.extend_from_slice(namespace);
            entry.resize(8 + padded(namespace.len() as u64) as usize, 0);
            entry
        };
        assert_link_error(
            link(Arch::X86_64, &[&program(&named(b"m"))]),
            ProgramLinkError::Unsupported(
                "a call through `kt_itf_m_0`, a property of an interface another module's type \
                 implements"
                    .into(),
            ),
        );
        let image = linked(Arch::X86_64, &[&program(&named(b"other"))]);
        let (target, _) = stub_of(&image, 0);
        assert_eq!(target, Image(&image).entry() + 8, "only f implements it");
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

//! Applying relocations: each architecture's relocation kinds, patched into the linked image.
//!
//! The linker resolves every relocation to its site, `P`, `S` and `A` first ([`Reloc`]); this
//! module computes each kind's value exactly, checks it against its field, and writes it. Nothing
//! here knows about layout or symbols, which is what lets the per-architecture encoders be tested
//! byte for byte on their own.

use std::collections::HashMap;

use super::super::target::Arch;
use super::ProgramLinkError;

/// One relocation, resolved: where it is, what it points at.
pub(super) struct Reloc {
    pub(super) r_type: u32,
    /// Offset of the site in the output file.
    pub(super) site: usize,
    /// End, in the output file, of the section holding the site: no field may reach past it.
    pub(super) section_end: usize,
    /// Address of the site: `P` in the ABI documents.
    pub(super) p: u64,
    /// Value of the symbol: `S`.
    pub(super) s: u64,
    /// Addend: `A`.
    pub(super) a: i64,
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
pub(super) fn relocate(
    arch: Arch,
    image: &mut [u8],
    relocations: &[Reloc],
) -> Result<(), ProgramLinkError> {
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

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::super::fixture::{assert_link_error, link, linked, x86_64_start, Image, Obj, TEXT};
    use super::*;

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
        // The writable segment starts on the next 64 KiB page: AArch64's maximum page size.
        assert_eq!(image.data(), 0x41_0000);
        // var = 0x410020, foo = 0x4000d4.
        let words: Vec<u32> = (0..10).map(|i| image.u32(TEXT + 4 * i)).collect();
        assert_eq!(
            words,
            [
                0x9000_0081, // adrp x1, #0x10000
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
        assert_eq!(image.u64(0x41_0040), 0x40_00d4);
        assert_eq!(image.u32(0x41_0048), 0x40_00d4u32.wrapping_sub(0x41_0048));
    }

    /// RISC-V: absolute and PC-relative `hi20`/`lo12` pairs (with the carry when the low half is
    /// negative), `PCREL_LO12` taking the value of the `auipc` it names, `CALL` patching both
    /// instructions, and the B/J/CB/CJ immediate scatters. `RELAX` changes nothing. Every expected
    /// word is what `llvm-mc` encodes for the instruction with the resolved operand.
    #[test]
    fn riscv64_relocations_patch_their_fields() {
        let mut object = Obj::new(Arch::Riscv64);
        // The code holds compressed instructions, so the object says so: EF_RISCV_RVC | lp64d.
        object.set_flags(0x5);
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
        assert_eq!(image.flags(), 0x5);
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
                    0x41_0000 + misaligned_by
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
            out_of_range(&format!("ABS64: value {PAST_64_BITS} at 0x410000")),
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
        assert_eq!(Image(&bytes).u64(0x41_0000), 0x8000_0000_0000_0010);
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
}

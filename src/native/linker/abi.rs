//! The ABI each input object says it was built for, checked against the target's and merged into
//! the executable's `e_flags`.
//!
//! Only RISC-V states an ABI in its objects. The x86_64 and AArch64 psABIs define no `e_flags`
//! bits and no build attributes that a static link must reconcile, so their executables carry 0.
//! A RISC-V object states its ABI twice: in `e_flags` (the psABI's "ELF Object Files" chapter)
//! and in a `.riscv.attributes` section (its "Attributes" chapter). Both are read here, before
//! anything is laid out, so an object built for another ABI is refused rather than linked into a
//! program that calls it with the wrong registers, stack or pointer width.
//!
//! What the psABI requires of a linker, and what this module does:
//!
//! - `EF_RISCV_FLOAT_ABI` must be the same in every object: floating-point arguments are passed in
//!   different registers under each ABI. krusty's riscv64 targets are `lp64d`, the ABI the
//!   `riscv64-unknown-linux-gnu` triple names and the runtime is compiled for, so every input must
//!   say double-float, and the executable does too.
//! - `EF_RISCV_RVE` must be the same in every object: RV64E code has 16 integer registers and the
//!   `lp64e` ABI. The target is RV64I, so it must be clear.
//! - `EF_RISCV_RV64ILP32` marks 32-bit pointers on RV64. The target's pointers are 64-bit, so it
//!   must be clear.
//! - `EF_RISCV_RVC` says the code may contain compressed instructions, which may then sit on any
//!   2-byte boundary. It is not an ABI choice but a property of the code, so the executable
//!   carries it when any input does: a union, as GNU ld and `ld.lld` compute it.
//! - `EF_RISCV_TSO` says the code relies on the RVTSO memory model. RVWMO code is also correct
//!   under the stronger RVTSO, so the executable needs RVTSO when any input does: again a union.
//! - Any other bit is reserved by the psABI or left to vendors. The linker cannot know what it
//!   asks for, so it refuses the object instead of guessing it is harmless.
//!
//! From `.riscv.attributes`, the attributes a linker can reconcile without knowing the ISA:
//!
//! - `Tag_RISCV_stack_align` must be 16, the stack alignment `lp64d` guarantees; code that assumes
//!   a smaller one can leave the stack misaligned for its callees.
//! - `Tag_RISCV_arch` must name an RV64I instruction set (`rv64i…`). Its extensions are not
//!   compared: an executable may use any extension the machine running it has, and a linker merges
//!   the extension lists rather than refusing them.
//! - `Tag_RISCV_atomic_abi` A6C and A7 map sequentially consistent loads and stores to different
//!   fence sequences, so objects using the two cannot be combined; A6S and an unknown mapping
//!   combine with either.
//!
//! The other attributes are safe to drop in a static link that relaxes nothing: the executable has
//! no section headers to carry them and no loader reads them; `Tag_RISCV_unaligned_access` only
//! permits code to rely on fast unaligned accesses, which is between that code and the machine;
//! `Tag_RISCV_priv_spec*` concern privileged code, which a user program is not; and
//! `Tag_RISCV_x3_reg_usage` matters to a linker that relaxes accesses to be `gp`-relative, which
//! this one never does. Unknown attributes are skipped by the psABI's rule that an odd tag carries
//! a string and an even one an integer.

use object::read::elf::{FileHeader, SectionHeader};
use object::read::Object;
use object::LittleEndian;

use super::super::target::Arch;
use super::elf::Elf;
use super::{AbiMismatch, ProgramLinkError, RiscvAtomicAbi, RiscvFloatAbi};

const EF_RISCV_RVC: u32 = object::elf::EF_RISCV_RVC.0;
const EF_RISCV_FLOAT_ABI: u32 = object::elf::EF_RISCV_FLOAT_ABI;
const EF_RISCV_RVE: u32 = object::elf::EF_RISCV_RVE.0;
const EF_RISCV_TSO: u32 = object::elf::EF_RISCV_TSO.0;
const EF_RISCV_RV64ILP32: u32 = object::elf::EF_RISCV_RV64ILP32.0;
/// Every bit the RISC-V psABI assigns a meaning.
const EF_RISCV_KNOWN: u32 =
    EF_RISCV_RVC | EF_RISCV_FLOAT_ABI | EF_RISCV_RVE | EF_RISCV_TSO | EF_RISCV_RV64ILP32;

/// The float ABI of every krusty riscv64 target: `lp64d`.
const RISCV_FLOAT_ABI: RiscvFloatAbi = RiscvFloatAbi::Double;
/// The stack alignment `lp64d` guarantees at a call.
const RISCV_STACK_ALIGN: u64 = 16;

const TAG_RISCV_STACK_ALIGN: u64 = 4;
const TAG_RISCV_ARCH: u64 = 5;
const TAG_RISCV_ATOMIC_ABI: u64 = 14;

impl RiscvFloatAbi {
    fn from_flags(e_flags: u32) -> Self {
        match e_flags & EF_RISCV_FLOAT_ABI {
            0x0 => Self::Soft,
            0x2 => Self::Single,
            0x4 => Self::Double,
            _ => Self::Quad,
        }
    }

    fn flags(self) -> u32 {
        match self {
            Self::Soft => 0x0,
            Self::Single => 0x2,
            Self::Double => 0x4,
            Self::Quad => 0x6,
        }
    }
}

/// The executable's `e_flags` for `arch`, once every input is checked compatible with the target
/// and with each other. Inputs are described as `input <index>`, as elsewhere in the linker.
pub(super) fn output_flags(arch: Arch, files: &[Elf]) -> Result<u32, ProgramLinkError> {
    match arch {
        Arch::X86_64 | Arch::Aarch64 => Ok(0),
        Arch::Riscv64 => riscv_output_flags(files),
    }
}

fn riscv_output_flags(files: &[Elf]) -> Result<u32, ProgramLinkError> {
    let mut output = RISCV_FLOAT_ABI.flags();
    // The first input with a definite atomic mapping (A6C or A7), and which one.
    let mut atomic: Option<(String, RiscvAtomicAbi)> = None;
    for (index, file) in files.iter().enumerate() {
        let input = format!("input {index}");
        let incompatible = |mismatch| {
            Err(ProgramLinkError::IncompatibleAbi {
                input: input.clone(),
                mismatch,
            })
        };
        let e_flags = file.elf_header().e_flags(LittleEndian).0;
        let unknown = e_flags & !EF_RISCV_KNOWN;
        if unknown != 0 {
            return incompatible(AbiMismatch::RiscvUnknownFlags(unknown));
        }
        if e_flags & EF_RISCV_RV64ILP32 != 0 {
            return incompatible(AbiMismatch::RiscvRv64Ilp32);
        }
        if e_flags & EF_RISCV_RVE != 0 {
            return incompatible(AbiMismatch::RiscvRve);
        }
        let found = RiscvFloatAbi::from_flags(e_flags);
        if found != RISCV_FLOAT_ABI {
            return incompatible(AbiMismatch::RiscvFloatAbi {
                found,
                expected: RISCV_FLOAT_ABI,
            });
        }
        output |= e_flags & (EF_RISCV_RVC | EF_RISCV_TSO);

        let attributes = riscv_attributes(&input, file)?;
        if let Some(found) = attributes.stack_align {
            if found != RISCV_STACK_ALIGN {
                return incompatible(AbiMismatch::RiscvStackAlign {
                    found,
                    expected: RISCV_STACK_ALIGN,
                });
            }
        }
        if let Some(isa) = attributes.arch {
            if !isa.starts_with("rv64i") {
                return incompatible(AbiMismatch::RiscvArch(isa));
            }
        }
        if let Some(found) = attributes.atomic_abi {
            match &atomic {
                Some((first, other)) if *other != found => {
                    return incompatible(AbiMismatch::RiscvAtomicAbi {
                        found,
                        other_input: first.clone(),
                        other: *other,
                    })
                }
                Some(_) => {}
                None => atomic = Some((input.clone(), found)),
            }
        }
    }
    Ok(output)
}

/// The attributes of one object's `.riscv.attributes` sections that a link must reconcile.
#[derive(Default)]
struct RiscvAttributes {
    stack_align: Option<u64>,
    arch: Option<String>,
    /// A6C or A7; A6S and an unknown mapping combine with anything, so they are not recorded.
    atomic_abi: Option<RiscvAtomicAbi>,
}

/// Read every `SHT_RISCV_ATTRIBUTES` section of `file` (described as `input`). Only the `riscv`
/// vendor's subsections are the psABI's; any other vendor's are skipped, as the attributes
/// format intends.
fn riscv_attributes(input: &str, file: &Elf) -> Result<RiscvAttributes, ProgramLinkError> {
    let malformed = |error: object::Error| {
        ProgramLinkError::Parse(format!("{input}: `.riscv.attributes`: {error}"))
    };
    let mut found = RiscvAttributes::default();
    for section in file.sections() {
        let header = section.elf_section_header();
        if header.sh_type(LittleEndian) != object::elf::SHT_RISCV_ATTRIBUTES {
            continue;
        }
        let attributes = header
            .attributes(LittleEndian, file.data())
            .map_err(malformed)?;
        let mut subsections = attributes.subsections().map_err(malformed)?;
        while let Some(subsection) = subsections.next().map_err(malformed)? {
            if subsection.vendor() != b"riscv" {
                continue;
            }
            let mut subsubsections = subsection.subsubsections();
            while let Some(subsubsection) = subsubsections.next().map_err(malformed)? {
                let mut reader = subsubsection.attributes();
                while let Some(tag) = reader.read_tag().map_err(malformed)? {
                    match tag {
                        TAG_RISCV_STACK_ALIGN => {
                            found.stack_align = Some(reader.read_integer().map_err(malformed)?);
                        }
                        TAG_RISCV_ARCH => {
                            let isa = reader.read_string().map_err(malformed)?;
                            found.arch = Some(String::from_utf8_lossy(isa).into_owned());
                        }
                        TAG_RISCV_ATOMIC_ABI => {
                            found.atomic_abi = match reader.read_integer().map_err(malformed)? {
                                1 => Some(RiscvAtomicAbi::A6C),
                                3 => Some(RiscvAtomicAbi::A7),
                                _ => None,
                            };
                        }
                        odd if odd % 2 == 1 => {
                            reader.read_string().map_err(malformed)?;
                        }
                        _ => {
                            reader.read_integer().map_err(malformed)?;
                        }
                    }
                }
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::super::fixture::{assert_link_error, link, linked, Attribute, Image, Obj};
    use super::*;

    /// A riscv64 object whose `_start` returns, with `e_flags` set to `e_flags`. `_start` is weak
    /// so that several of these link together, each asking only its flags be reconciled.
    fn riscv(e_flags: u32) -> Obj {
        let mut object = Obj::new(Arch::Riscv64);
        object.set_flags(e_flags);
        let text = object.text_words(&[0x0000_8067]);
        object.define_weak("_start", text, 0);
        object
    }

    fn incompatible(input: usize, mismatch: AbiMismatch) -> ProgramLinkError {
        ProgramLinkError::IncompatibleAbi {
            input: format!("input {input}"),
            mismatch,
        }
    }

    // ---- e_flags -----------------------------------------------------------------------------------

    /// The executable says double-float, the one float ABI every input shares, and compressed
    /// instructions or RVTSO when any input uses them.
    #[test]
    fn riscv64_output_flags_keep_the_float_abi_and_merge_rvc_and_tso() {
        for (inputs, expected) in [
            (vec![0x4], 0x4),
            (vec![0x5], 0x5),
            (vec![0x4, 0x5], 0x5),
            (vec![0x5, 0x4], 0x5),
            (vec![0x14, 0x4], 0x14),
            (vec![0x4, 0x5, 0x14], 0x15),
        ] {
            let objects: Vec<Obj> = inputs.iter().map(|flags| riscv(*flags)).collect();
            let objects: Vec<&Obj> = objects.iter().collect();
            let bytes = linked(Arch::Riscv64, &objects);
            assert_eq!(Image(&bytes).flags(), expected, "{inputs:#x?}");
        }
    }

    /// x86_64 and AArch64 define no `e_flags`: their executables carry 0.
    #[test]
    fn x86_64_and_aarch64_output_flags_are_zero() {
        let mut object = Obj::new(Arch::X86_64);
        let text = object.section(".text", SectionKind::Text, &[0xc3], 1);
        object.define("_start", text, 0);
        assert_eq!(Image(&linked(Arch::X86_64, &[&object])).flags(), 0);

        let mut object = Obj::new(Arch::Aarch64);
        let text = object.text_words(&[0xd65f_03c0]);
        object.define("_start", text, 0);
        assert_eq!(Image(&linked(Arch::Aarch64, &[&object])).flags(), 0);
    }

    /// An object for any float ABI but `lp64d` is refused, named, whichever input it is.
    #[test]
    fn a_riscv64_object_with_another_float_abi_is_refused() {
        for (e_flags, found) in [
            (0x1, RiscvFloatAbi::Soft),
            (0x3, RiscvFloatAbi::Single),
            (0x7, RiscvFloatAbi::Quad),
        ] {
            assert_link_error(
                link(Arch::Riscv64, &[&riscv(0x5), &riscv(e_flags)]),
                incompatible(
                    1,
                    AbiMismatch::RiscvFloatAbi {
                        found,
                        expected: RiscvFloatAbi::Double,
                    },
                ),
            );
        }
        assert_eq!(
            incompatible(
                1,
                AbiMismatch::RiscvFloatAbi {
                    found: RiscvFloatAbi::Soft,
                    expected: RiscvFloatAbi::Double,
                }
            )
            .to_string(),
            "input 1 is built for the lp64 (soft-float) ABI, but the target uses lp64d \
             (double-float); objects for different floating-point ABIs pass arguments in \
             different registers and cannot be linked"
        );
    }

    #[test]
    fn a_riscv64_rve_object_is_refused() {
        assert_link_error(
            link(Arch::Riscv64, &[&riscv(0xd)]),
            incompatible(0, AbiMismatch::RiscvRve),
        );
        assert_eq!(
            incompatible(0, AbiMismatch::RiscvRve).to_string(),
            "input 0 is built for RV64E (EF_RISCV_RVE: 16 integer registers, the lp64e ABI); \
             the target is RV64I with the lp64d ABI"
        );
    }

    #[test]
    fn a_riscv64_ilp32_object_is_refused() {
        assert_link_error(
            link(Arch::Riscv64, &[&riscv(0x25)]),
            incompatible(0, AbiMismatch::RiscvRv64Ilp32),
        );
        assert_eq!(
            incompatible(0, AbiMismatch::RiscvRv64Ilp32).to_string(),
            "input 0 is built for the RV64ILP32 ABI (EF_RISCV_RV64ILP32: 32-bit pointers); the \
             target's lp64d ABI has 64-bit pointers"
        );
    }

    /// A bit the psABI reserves, or leaves to a vendor, is refused rather than assumed harmless.
    #[test]
    fn a_riscv64_object_with_unknown_flags_is_refused() {
        assert_link_error(
            link(Arch::Riscv64, &[&riscv(0x0100_0045)]),
            incompatible(0, AbiMismatch::RiscvUnknownFlags(0x0100_0040)),
        );
        assert_eq!(
            incompatible(0, AbiMismatch::RiscvUnknownFlags(0x0100_0040)).to_string(),
            "input 0 sets ELF flags 0x1000040, which the RISC-V psABI reserves or leaves to \
             vendors; krusty's linker cannot tell whether the object suits the target"
        );
    }

    // ---- .riscv.attributes -------------------------------------------------------------------------

    /// A riscv64 object like `riscv(0x5)` with a `.riscv.attributes` section holding `attributes`.
    fn with_attributes(attributes: &[(u64, Attribute)]) -> Obj {
        let mut object = riscv(0x5);
        object.riscv_attributes(attributes);
        object
    }

    /// What clang writes for the runtime (`rv64gc`, 16-byte stack) links, and so do attributes
    /// the linker does not reconcile, another vendor's subsection among them.
    #[test]
    fn riscv64_attributes_for_the_target_link() {
        let runtime_like = with_attributes(&[
            (4, Attribute::Integer(16)),
            (
                5,
                Attribute::String("rv64i2p1_m2p0_a2p1_f2p2_d2p2_c2p0_zicsr2p0"),
            ),
        ]);
        let others = with_attributes(&[
            (6, Attribute::Integer(1)),   // Tag_RISCV_unaligned_access
            (8, Attribute::Integer(1)),   // Tag_RISCV_priv_spec
            (14, Attribute::Integer(2)),  // Tag_RISCV_atomic_abi: A6S
            (16, Attribute::Integer(1)),  // Tag_RISCV_x3_reg_usage
            (67, Attribute::String("?")), // unknown, odd: a string
            (68, Attribute::Integer(9)),  // unknown, even: an integer
        ]);
        let a7 = with_attributes(&[(14, Attribute::Integer(3))]);
        let bytes = linked(Arch::Riscv64, &[&runtime_like, &others, &a7]);
        assert_eq!(Image(&bytes).flags(), 0x5);
    }

    #[test]
    fn a_riscv64_object_assuming_another_stack_alignment_is_refused() {
        let object = with_attributes(&[(4, Attribute::Integer(4))]);
        assert_link_error(
            link(Arch::Riscv64, &[&riscv(0x5), &object]),
            incompatible(
                1,
                AbiMismatch::RiscvStackAlign {
                    found: 4,
                    expected: 16,
                },
            ),
        );
        assert_eq!(
            incompatible(
                1,
                AbiMismatch::RiscvStackAlign {
                    found: 4,
                    expected: 16
                }
            )
            .to_string(),
            "input 1 assumes a 4-byte-aligned stack (Tag_RISCV_stack_align); the target's ABI \
             aligns it to 16 bytes"
        );
    }

    #[test]
    fn a_riscv64_object_for_another_base_isa_is_refused() {
        for isa in ["rv32i2p1_m2p0", "rv64e2p0_c2p0"] {
            let object = with_attributes(&[(5, Attribute::String(isa))]);
            assert_link_error(
                link(Arch::Riscv64, &[&object]),
                incompatible(0, AbiMismatch::RiscvArch(isa.to_string())),
            );
        }
        assert_eq!(
            incompatible(0, AbiMismatch::RiscvArch("rv32i2p1_m2p0".to_string())).to_string(),
            "input 0 is built for `rv32i2p1_m2p0` (Tag_RISCV_arch), which is not an RV64I \
             instruction set"
        );
    }

    /// A6C and A7 are refused together, in either order, naming both inputs.
    #[test]
    fn riscv64_objects_with_conflicting_atomic_abis_are_refused() {
        let a6c = with_attributes(&[(14, Attribute::Integer(1))]);
        let a7 = with_attributes(&[(14, Attribute::Integer(3))]);
        assert_link_error(
            link(Arch::Riscv64, &[&a6c, &riscv(0x5), &a7]),
            incompatible(
                2,
                AbiMismatch::RiscvAtomicAbi {
                    found: RiscvAtomicAbi::A7,
                    other_input: "input 0".to_string(),
                    other: RiscvAtomicAbi::A6C,
                },
            ),
        );
        let error = link(Arch::Riscv64, &[&a7, &a6c]).expect_err("A7 then A6C");
        assert_eq!(
            error,
            incompatible(
                1,
                AbiMismatch::RiscvAtomicAbi {
                    found: RiscvAtomicAbi::A6C,
                    other_input: "input 0".to_string(),
                    other: RiscvAtomicAbi::A7,
                },
            )
        );
        assert_eq!(
            error.to_string(),
            "input 1 maps atomics by the A6C atomic ABI (Tag_RISCV_atomic_abi), which cannot be \
             linked with input 0's A7: they order sequentially consistent loads and stores with \
             different fences"
        );
    }

    /// An attributes section that ends mid-attribute is a parse error, not an ignored section.
    #[test]
    fn a_truncated_riscv64_attributes_section_is_a_parse_error() {
        let mut object = riscv(0x5);
        // Tag_RISCV_arch with its string cut off before the terminating NUL.
        object.riscv_attributes_raw(&[
            b'A', 17, 0, 0, 0, b'r', b'i', b's', b'c', b'v', 0, 1, 7, 0, 0, 0, 5, b'r',
        ]);
        assert_link_error(
            link(Arch::Riscv64, &[&object]),
            ProgramLinkError::Parse(
                "input 0: `.riscv.attributes`: Invalid ELF attribute string value".to_string(),
            ),
        );
    }
}

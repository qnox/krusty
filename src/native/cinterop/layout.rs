//! The size and alignment of every C type for a target, and where each struct field sits.
//!
//! krusty's targets are all LP64 Linux, so the scalar widths agree; what differs is
//! `__builtin_va_list` and one bitfield rule. The record algorithm is the System V one that the
//! x86-64, AAPCS64 and RISC-V psABIs share, with GCC's `packed` and `aligned` attributes, written
//! after clang's `ItaniumRecordLayoutBuilder` so a layout here is clang's for the same input:
//!
//! - A bitfield goes at the next free bit unless it would then cross a boundary of its declared
//!   type's alignment, in which case it starts at the next such boundary. A zero-width bitfield
//!   moves the next field to that boundary.
//! - A non-bitfield starts at the next byte, rounded up to its alignment.
//! - `packed` makes each field's alignment one byte (one bit for a bitfield), and `aligned(n)`
//!   raises a field's or record's alignment to at least `n`.
//! - On x86-64 and RISC-V an unnamed bitfield does not raise the record's alignment; AAPCS64 says
//!   it does ("without exception for zero-sized or anonymous bit-fields", §7.1.7).

use std::collections::HashMap;
use std::rc::Rc;

use super::model::{
    Declarations, Enum, FloatKind, IntRank, RecordId, RecordKind, Signedness, Type, TypeId,
};
use crate::native::target::{Arch, NativeTarget};

/// A type's size and alignment, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    pub(super) size: u64,
    pub(super) align: u64,
}

/// A struct's or union's size and alignment, and where each field starts, in bits from the start
/// of the record (a non-bitfield always starts on a byte).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecordLayout {
    pub(super) size: u64,
    pub(super) align: u64,
    pub(super) field_offsets: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LayoutError(pub(super) String);

/// Computes layouts for one target, remembering each record's.
pub(super) struct LayoutEngine {
    arch: Arch,
    records: HashMap<RecordId, Rc<RecordLayout>>,
}

/// The largest alignment `__attribute__((aligned))` without an argument means, in bytes.
pub(super) const DEFAULT_ATTRIBUTE_ALIGNMENT: u64 = 16;

/// Above this size an `_Atomic` type keeps its value type's layout; at or below it, the size is
/// rounded up to a power of two and the alignment made equal to it.
const MAX_ATOMIC_PROMOTE_SIZE: u64 = 16;

impl LayoutEngine {
    pub(super) fn new(target: NativeTarget) -> Self {
        Self {
            arch: target.arch,
            records: HashMap::new(),
        }
    }

    /// Whether a plain `char` is signed: it is on x86-64, and unsigned on AArch64 and RISC-V.
    pub(super) fn char_is_signed(&self) -> bool {
        matches!(self.arch, Arch::X86_64)
    }

    pub(super) fn layout(
        &mut self,
        declarations: &Declarations,
        ty: TypeId,
    ) -> Result<Layout, LayoutError> {
        Ok(match declarations.ty(ty) {
            // GNU C gives `void` and function types a size of one, for pointer arithmetic.
            Type::Void | Type::Function(_) => Layout { size: 1, align: 1 },
            Type::Int { rank, .. } => scalar(rank.size()),
            Type::Float(kind) => scalar(float_size(*kind)),
            Type::Complex(kind) => Layout {
                size: 2 * float_size(*kind),
                align: float_size(*kind),
            },
            Type::VaList => match self.arch {
                // `struct __va_list_tag[1]`: two unsigned offsets and two pointers.
                Arch::X86_64 => Layout { size: 24, align: 8 },
                // `struct __va_list`: three pointers and two int offsets.
                Arch::Aarch64 => Layout { size: 32, align: 8 },
                // A plain `void *`.
                Arch::Riscv64 => scalar(8),
            },
            Type::Pointer(_) => scalar(8),
            Type::Array { element, length } => {
                let Some(length) = length else {
                    return Err(LayoutError(format!(
                        "`{}` is an array of unknown length",
                        declarations.spell(ty)
                    )));
                };
                let element = self.layout(declarations, *element)?;
                Layout {
                    size: element.size * length,
                    align: element.align,
                }
            }
            Type::Record(record) => {
                let layout = self.record(declarations, *record)?;
                Layout {
                    size: layout.size,
                    align: layout.align,
                }
            }
            Type::Enum(enumeration) => {
                let enumeration = declarations.enumeration(*enumeration);
                let Some((rank, _)) = enum_underlying(enumeration) else {
                    return Err(LayoutError(format!(
                        "`{}` is an incomplete type",
                        declarations.spell(ty)
                    )));
                };
                let base = scalar(rank.size());
                Layout {
                    size: base.size,
                    align: base.align.max(enumeration.aligned.unwrap_or(1)),
                }
            }
            Type::Typedef(typedef) => {
                let typedef = declarations.typedef(*typedef);
                let base = self.layout(declarations, typedef.ty)?;
                Layout {
                    size: base.size,
                    align: typedef.aligned.unwrap_or(base.align),
                }
            }
            Type::Qualified { base, .. } => self.layout(declarations, *base)?,
            Type::Atomic(value) => {
                let value = self.layout(declarations, *value)?;
                if value.size == 0 {
                    Layout {
                        size: 1,
                        align: value.align,
                    }
                } else if value.size <= MAX_ATOMIC_PROMOTE_SIZE {
                    scalar(value.size.next_power_of_two())
                } else {
                    value
                }
            }
            Type::Vector { size, .. } => {
                let align = size.next_power_of_two();
                // AArch64 caps a vector's alignment at its 16-byte registers; x86-64 and RISC-V
                // align a vector to its whole size.
                let cap = match self.arch {
                    Arch::Aarch64 => 16,
                    Arch::X86_64 | Arch::Riscv64 => align,
                };
                Layout {
                    size: size.next_multiple_of(align),
                    align: align.min(cap),
                }
            }
        })
    }

    pub(super) fn record(
        &mut self,
        declarations: &Declarations,
        id: RecordId,
    ) -> Result<Rc<RecordLayout>, LayoutError> {
        if let Some(layout) = self.records.get(&id) {
            return Ok(layout.clone());
        }
        let layout = Rc::new(self.lay_out_record(declarations, id)?);
        self.records.insert(id, layout.clone());
        Ok(layout)
    }

    fn lay_out_record(
        &mut self,
        declarations: &Declarations,
        id: RecordId,
    ) -> Result<RecordLayout, LayoutError> {
        let record = declarations.record(id);
        let Some(fields) = &record.fields else {
            return Err(LayoutError(format!(
                "`{}` is an incomplete type",
                declarations.spell_record(id)
            )));
        };
        let union = record.kind == RecordKind::Union;
        let mut builder = RecordBuilder {
            union,
            data_bits: 0,
            unfilled_bits: 0,
            align: record.aligned.unwrap_or(1),
            field_offsets: Vec::with_capacity(fields.len()),
        };
        for (index, field) in fields.iter().enumerate() {
            let packed = record.packed || field.packed;
            match field.bit_width {
                Some(width) => {
                    let storage = self.layout(declarations, field.ty)?;
                    if width > storage.size * 8 {
                        return Err(LayoutError(format!(
                            "bitfield `{}` is wider than its type `{}`",
                            field.name.as_deref().unwrap_or("<unnamed>"),
                            declarations.spell(field.ty)
                        )));
                    }
                    builder.bitfield(BitfieldInput {
                        width,
                        storage,
                        packed,
                        explicit_align: field.aligned,
                        // AAPCS64 counts an unnamed bitfield's type toward the record's alignment.
                        counts_toward_alignment: field.name.is_some() || self.arch == Arch::Aarch64,
                    });
                }
                None => {
                    // A flexible array member (C11 §6.7.2.1p18) takes no space but its alignment.
                    let flexible_element = match declarations.ty(declarations.canonical(field.ty)) {
                        Type::Array {
                            element,
                            length: None,
                        } if index + 1 == fields.len() => Some(*element),
                        _ => None,
                    };
                    let layout = match flexible_element {
                        Some(element) => Layout {
                            size: 0,
                            align: self.layout(declarations, element)?.align,
                        },
                        None => self.layout(declarations, field.ty)?,
                    };
                    let natural = if packed { 1 } else { layout.align };
                    builder.field(layout.size, natural.max(field.aligned.unwrap_or(1)));
                }
            }
        }
        Ok(builder.finish())
    }
}

impl Declarations {
    fn spell_record(&self, id: RecordId) -> String {
        let record = self.record(id);
        let keyword = match record.kind {
            RecordKind::Struct => "struct",
            RecordKind::Union => "union",
        };
        format!(
            "{keyword} {}",
            record.tag.as_deref().unwrap_or("<anonymous>")
        )
    }
}

struct BitfieldInput {
    width: u64,
    storage: Layout,
    packed: bool,
    explicit_align: Option<u64>,
    counts_toward_alignment: bool,
}

/// The running state of one record's layout, in bits.
struct RecordBuilder {
    union: bool,
    /// The end of the last field, rounded up to a byte.
    data_bits: u64,
    /// How many of the bits below `data_bits` the last bitfield left free for the next one.
    unfilled_bits: u64,
    /// In bytes.
    align: u64,
    field_offsets: Vec<u64>,
}

impl RecordBuilder {
    fn field(&mut self, size: u64, align: u64) {
        let offset = if self.union {
            0
        } else {
            self.data_bits.next_multiple_of(align * 8)
        };
        self.field_offsets.push(offset);
        self.unfilled_bits = 0;
        self.data_bits = if self.union {
            self.data_bits.max(size * 8)
        } else {
            offset + size * 8
        };
        self.align = self.align.max(align);
    }

    fn bitfield(&mut self, input: BitfieldInput) {
        let BitfieldInput {
            width,
            storage,
            packed,
            explicit_align,
            counts_toward_alignment,
        } = input;
        let storage_bits = storage.size * 8;
        let mut align_bits = if packed && width != 0 {
            1
        } else {
            storage.align * 8
        };
        if let Some(explicit) = explicit_align {
            align_bits = align_bits.max(explicit * 8);
        }
        let mut offset = if self.union {
            0
        } else {
            self.data_bits - self.unfilled_bits
        };
        if width == 0 || (offset & (align_bits - 1)) + width > storage_bits {
            offset = offset.next_multiple_of(align_bits);
        } else if let Some(explicit) = explicit_align {
            offset = offset.next_multiple_of(explicit * 8);
        }
        self.field_offsets.push(offset);
        if self.union {
            self.data_bits = self.data_bits.max(width.next_multiple_of(8));
        } else {
            let end = offset + width;
            self.data_bits = end.next_multiple_of(8);
            self.unfilled_bits = self.data_bits - end;
        }
        if counts_toward_alignment {
            // Whole bytes only: a packed bitfield's one-bit alignment raises nothing.
            self.align = self.align.max(align_bits / 8);
        }
    }

    fn finish(self) -> RecordLayout {
        RecordLayout {
            size: (self.data_bits / 8).next_multiple_of(self.align),
            align: self.align,
            field_offsets: self.field_offsets,
        }
    }
}

fn scalar(size: u64) -> Layout {
    Layout { size, align: size }
}

fn float_size(kind: FloatKind) -> u64 {
    match kind {
        FloatKind::Float => 4,
        FloatKind::Double => 8,
        // x86-64's 80-bit x87 format padded to 16 bytes; AArch64's and RISC-V's IEEE binary128.
        FloatKind::LongDouble => 16,
    }
}

/// The integer type an enum is laid out as, and whether it is signed (`None` before the enum is
/// defined). As GCC and clang choose it for C: `unsigned int` when no enumerator is negative and
/// all fit, `int` when some is negative and all fit, else the 64-bit type; `packed` picks the
/// smallest type that holds every enumerator.
pub(super) fn enum_underlying(enumeration: &Enum) -> Option<(IntRank, Signedness)> {
    let enumerators = enumeration.enumerators.as_ref()?;
    let min = enumerators.iter().map(|e| e.value).min().unwrap_or(0);
    let max = enumerators.iter().map(|e| e.value).max().unwrap_or(0);
    let signedness = if min < 0 {
        Signedness::Signed
    } else {
        Signedness::Unsigned
    };
    let fits = |rank: IntRank| {
        let bits = rank.size() * 8;
        if min < 0 {
            min >= -(1i128 << (bits - 1)) && max < (1i128 << (bits - 1))
        } else {
            max < (1i128 << bits)
        }
    };
    let candidates: &[IntRank] = if enumeration.packed {
        &[
            IntRank::Char,
            IntRank::Short,
            IntRank::Int,
            IntRank::Long,
            IntRank::Int128,
        ]
    } else {
        &[IntRank::Int, IntRank::Long, IntRank::Int128]
    };
    let rank = candidates
        .iter()
        .copied()
        .find(|&rank| fits(rank))
        .unwrap_or(IntRank::Int128);
    Some((rank, signedness))
}

#[cfg(test)]
mod tests;

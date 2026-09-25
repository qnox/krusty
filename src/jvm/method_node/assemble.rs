//! Laying a [`MethodNode`] out as class-file bytes against some constant pool.
//!
//! The pool is abstract ([`ConstantSink`]): the production sink is the [`ClassWriter`] of the class
//! the body lands in. Constants are interned in the order ASM's `MethodWriter` interns them when a
//! tree node is accepted: the try/catch types first (ASM visits `tryCatchBlocks` before the
//! instructions), then each instruction's operands in list order. The caller interns local names
//! and descriptors afterwards, as ASM does on `visitLocalVariable`.
//!
//! Encodings are the ones ASM picks: the one-byte `xload_n`/`xstore_n` forms for slots 0-3, `ldc`
//! for a pool index below 256, `wide` only when a slot or increment needs it.

use super::nodes::{Constant, Handle, Insn, LabelId, MethodNode, Node};
use crate::jvm::classfile::ClassWriter;

/// A constant pool a body can be assembled against. Each call interns (or finds) an entry and
/// returns its index.
pub trait ConstantSink {
    fn class(&mut self, name: &str) -> u16;
    fn field(&mut self, owner: &str, name: &str, desc: &str) -> u16;
    fn method(&mut self, owner: &str, name: &str, desc: &str, interface: bool) -> u16;
    /// A loadable constant, for `ldc` or a bootstrap argument.
    fn constant(&mut self, constant: &Constant) -> u16;
    /// A `CONSTANT_InvokeDynamic` together with the bootstrap-method entry it names.
    fn invoke_dynamic(
        &mut self,
        name: &str,
        desc: &str,
        bootstrap: &Handle,
        arguments: &[Constant],
    ) -> u16;
}

fn handle_member(sink: &mut ClassWriter, handle: &Handle) -> u16 {
    match handle.kind {
        1..=4 => sink.fieldref(&handle.owner, &handle.name, &handle.desc),
        _ if handle.interface => {
            sink.interface_methodref(&handle.owner, &handle.name, &handle.desc)
        }
        _ => sink.methodref(&handle.owner, &handle.name, &handle.desc),
    }
}

impl ConstantSink for ClassWriter {
    fn class(&mut self, name: &str) -> u16 {
        self.class_ref(name)
    }

    fn field(&mut self, owner: &str, name: &str, desc: &str) -> u16 {
        self.fieldref(owner, name, desc)
    }

    fn method(&mut self, owner: &str, name: &str, desc: &str, interface: bool) -> u16 {
        if interface {
            self.interface_methodref(owner, name, desc)
        } else {
            self.methodref(owner, name, desc)
        }
    }

    fn constant(&mut self, constant: &Constant) -> u16 {
        match constant {
            Constant::Int(value) => self.const_int(*value),
            Constant::Float(bits) => self.const_float(f32::from_bits(*bits)),
            Constant::Long(value) => self.const_long(*value),
            Constant::Double(bits) => self.const_double(f64::from_bits(*bits)),
            Constant::String(value) => self.const_string_kt(value),
            Constant::Class(name) => self.class_ref(name),
            Constant::MethodType(desc) => self.method_type_ref(desc),
            Constant::Handle(handle) => {
                let member = handle_member(self, handle);
                self.method_handle_ref(handle.kind, member)
            }
        }
    }

    fn invoke_dynamic(
        &mut self,
        name: &str,
        desc: &str,
        bootstrap: &Handle,
        arguments: &[Constant],
    ) -> u16 {
        let member = handle_member(self, bootstrap);
        let handle = self.method_handle_ref(bootstrap.kind, member);
        let arguments = arguments
            .iter()
            .map(|argument| self.constant(argument))
            .collect();
        let entry = self.add_bootstrap(handle, arguments);
        self.invoke_dynamic_ref(entry, name, desc)
    }
}

/// Why a node could not be laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssembleError {
    /// A jump, switch, table entry or line names a label no [`Node::Label`] places.
    UnplacedLabel(LabelId),
    /// More than one node places the same label, making its position ambiguous.
    DuplicateLabel(LabelId),
    /// A [`Insn::Jump`] carries an opcode that is not a conditional branch, `goto`, or `jsr`.
    InvalidJumpOpcode(u8),
    /// A table switch does not have exactly one label for every value in its inclusive range.
    InvalidTableSwitch { low: i32, high: i32, labels: usize },
    /// A lookup switch does not have one label per key, or its keys are not strictly increasing.
    InvalidLookupSwitch,
    /// A local variable's range ends before it starts.
    InvertedLocalRange(LabelId, LabelId),
    /// The code array would exceed the JVM's 65535-byte limit.
    CodeTooLarge,
    /// An `invokeinterface` whose descriptor cannot be read, so its argument count is unknown.
    MalformedDescriptor(String),
}

/// A local-variable table entry, with its name and descriptor still to be interned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledLocal {
    pub start_pc: u16,
    pub length: u16,
    pub slot: u16,
    pub name: String,
    pub desc: String,
}

/// A laid-out `Code` attribute, minus the frames the caller computes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledCode {
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: Vec<u8>,
    /// `(start_pc, end_pc, handler_pc, catch_type)` in try/catch order; `catch_type` 0 catches all.
    pub exception_table: Vec<(u16, u16, u16, u16)>,
    /// `(start_pc, line)` in node order.
    pub line_numbers: Vec<(u16, u16)>,
    pub local_variables: Vec<AssembledLocal>,
    /// The offset each label stands at, by label id; `None` for a label no node places.
    pub(super) label_offsets: Vec<Option<u16>>,
}

impl AssembledCode {
    /// The offset `label` stands at, if a node places it.
    pub fn offset_of(&self, label: LabelId) -> Option<u16> {
        self.label_offsets.get(label.0 as usize).copied().flatten()
    }
}

/// An instruction with its pool operands interned; only branch offsets are left to lay out.
enum Encoded {
    Fixed(Vec<u8>),
    Jump {
        op: u8,
        target: LabelId,
        /// A wide `goto`/`jsr`, or an inverted conditional followed by `goto_w`.
        wide: bool,
    },
    TableSwitch {
        low: i32,
        high: i32,
        default: LabelId,
        labels: Vec<LabelId>,
    },
    LookupSwitch {
        default: LabelId,
        keys: Vec<i32>,
        labels: Vec<LabelId>,
    },
}

enum Item {
    Label(LabelId),
    Code(Encoded),
}

fn size_at(encoded: &Encoded, at: usize) -> usize {
    let pad = (4 - (at + 1) % 4) % 4;
    match encoded {
        Encoded::Fixed(bytes) => bytes.len(),
        Encoded::Jump {
            op: 0xa7 | 0xa8,
            wide: true,
            ..
        } => 5,
        Encoded::Jump { wide: true, .. } => 8,
        Encoded::Jump { wide: false, .. } => 3,
        Encoded::TableSwitch { labels, .. } => 1 + pad + 12 + 4 * labels.len(),
        Encoded::LookupSwitch { labels, .. } => 1 + pad + 8 + 8 * labels.len(),
    }
}

fn with_u2(op: u8, index: u16) -> Vec<u8> {
    let [high, low] = index.to_be_bytes();
    vec![op, high, low]
}

/// The argument words an `invokeinterface` passes, receiver included.
fn interface_argument_count(desc: &str) -> Result<u8, AssembleError> {
    let malformed = || AssembleError::MalformedDescriptor(desc.to_string());
    let (parameters, _) = crate::jvm::names::parse_method_descriptor(desc).ok_or_else(malformed)?;
    let words: usize = parameters
        .iter()
        .map(|parameter| {
            if matches!(*parameter, "J" | "D") {
                2
            } else {
                1
            }
        })
        .sum();
    u8::try_from(words + 1).map_err(|_| malformed())
}

/// A local load, store or `ret`: the one-byte form for slots 0-3 (`ret` has none), the one-byte
/// operand up to slot 255, `wide` beyond.
fn var_encoding(op: u8, slot: u16) -> Vec<u8> {
    match (op, slot) {
        (0x15..=0x19, 0..=3) => vec![0x1a + (op - 0x15) * 4 + slot as u8],
        (0x36..=0x3a, 0..=3) => vec![0x3b + (op - 0x36) * 4 + slot as u8],
        (_, 0..=255) => vec![op, slot as u8],
        _ => {
            let [high, low] = slot.to_be_bytes();
            vec![0xc4, op, high, low]
        }
    }
}

fn is_jump_opcode(op: u8) -> bool {
    matches!(op, 0x99..=0xa8 | 0xc6 | 0xc7)
}

/// The conditional that skips a following `goto_w` when the original condition is false.
fn inverse_condition(op: u8) -> Option<u8> {
    Some(match op {
        0x99 => 0x9a,
        0x9a => 0x99,
        0x9b => 0x9c,
        0x9c => 0x9b,
        0x9d => 0x9e,
        0x9e => 0x9d,
        0x9f => 0xa0,
        0xa0 => 0x9f,
        0xa1 => 0xa2,
        0xa2 => 0xa1,
        0xa3 => 0xa4,
        0xa4 => 0xa3,
        0xa5 => 0xa6,
        0xa6 => 0xa5,
        0xc6 => 0xc7,
        0xc7 => 0xc6,
        _ => return None,
    })
}

impl Insn {
    /// The instruction's bytes, its operands interned into `sink`, when they do not depend on where
    /// it stands; `None` for a jump or a switch, whose offsets and padding do.
    pub fn encode_in_place(
        &self,
        sink: &mut impl ConstantSink,
    ) -> Result<Option<Vec<u8>>, AssembleError> {
        Ok(match encode(self, sink)? {
            Encoded::Fixed(bytes) => Some(bytes),
            Encoded::Jump { .. } | Encoded::TableSwitch { .. } | Encoded::LookupSwitch { .. } => {
                None
            }
        })
    }
}

fn encode(insn: &Insn, sink: &mut impl ConstantSink) -> Result<Encoded, AssembleError> {
    Ok(Encoded::Fixed(match insn {
        Insn::Op(op) => vec![*op],
        Insn::Int { op: 0x10, operand } => vec![0x10, *operand as i8 as u8],
        Insn::Int { op: 0x11, operand } => {
            let [high, low] = (*operand as i16).to_be_bytes();
            vec![0x11, high, low]
        }
        Insn::Int { op, operand } => vec![*op, *operand as u8],
        Insn::Var { op, slot } => var_encoding(*op, *slot),
        Insn::Iinc { slot, delta } => match (u8::try_from(*slot), i8::try_from(*delta)) {
            (Ok(slot), Ok(delta)) => vec![0x84, slot, delta as u8],
            _ => {
                let [slot_high, slot_low] = slot.to_be_bytes();
                let [delta_high, delta_low] = delta.to_be_bytes();
                vec![0xc4, 0x84, slot_high, slot_low, delta_high, delta_low]
            }
        },
        Insn::Type { op, class } => with_u2(*op, sink.class(class)),
        Insn::Field {
            op,
            owner,
            name,
            desc,
        } => with_u2(*op, sink.field(owner, name, desc)),
        Insn::Method {
            op,
            owner,
            name,
            desc,
            interface,
        } => {
            let mut bytes = with_u2(*op, sink.method(owner, name, desc, *interface));
            if *op == 0xb9 {
                bytes.extend([interface_argument_count(desc)?, 0]);
            }
            bytes
        }
        Insn::InvokeDynamic {
            name,
            desc,
            bootstrap,
            arguments,
        } => {
            let mut bytes = with_u2(0xba, sink.invoke_dynamic(name, desc, bootstrap, arguments));
            bytes.extend([0, 0]);
            bytes
        }
        Insn::Ldc(constant) => {
            let index = sink.constant(constant);
            match (constant.is_wide(), u8::try_from(index)) {
                (true, _) => with_u2(0x14, index),
                (false, Ok(index)) => vec![0x12, index],
                (false, Err(_)) => with_u2(0x13, index),
            }
        }
        Insn::MultiANewArray { desc, dims } => {
            let mut bytes = with_u2(0xc5, sink.class(desc));
            bytes.push(*dims);
            bytes
        }
        Insn::Jump { op, target } => {
            if !is_jump_opcode(*op) {
                return Err(AssembleError::InvalidJumpOpcode(*op));
            }
            return Ok(Encoded::Jump {
                op: *op,
                target: *target,
                wide: false,
            });
        }
        Insn::TableSwitch {
            low,
            high,
            default,
            labels,
        } => {
            let count = i64::from(*high) - i64::from(*low) + 1;
            if count <= 0 || usize::try_from(count).ok() != Some(labels.len()) {
                return Err(AssembleError::InvalidTableSwitch {
                    low: *low,
                    high: *high,
                    labels: labels.len(),
                });
            }
            return Ok(Encoded::TableSwitch {
                low: *low,
                high: *high,
                default: *default,
                labels: labels.clone(),
            });
        }
        Insn::LookupSwitch {
            default,
            keys,
            labels,
        } => {
            if keys.len() != labels.len() || keys.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(AssembleError::InvalidLookupSwitch);
            }
            return Ok(Encoded::LookupSwitch {
                default: *default,
                keys: keys.clone(),
                labels: labels.clone(),
            });
        }
    }))
}

impl MethodNode {
    /// Lay the body out, interning its operands into `sink`.
    pub fn assemble(&self, sink: &mut impl ConstantSink) -> Result<AssembledCode, AssembleError> {
        let catch_types: Vec<u16> = self
            .try_catch_blocks
            .iter()
            .map(|block| {
                block
                    .catch_type
                    .as_deref()
                    .map_or(0, |name| sink.class(name))
            })
            .collect();

        let mut items = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            match node {
                Node::Label(label) => items.push(Item::Label(*label)),
                Node::Line { .. } => {}
                Node::Insn(insn) => items.push(Item::Code(encode(insn, sink)?)),
            }
        }

        // A transform may duplicate a label accidentally. Do not silently let the last placement
        // win: every control-flow and table edge must name one unambiguous position.
        let mut placed = vec![false; self.label_count as usize];
        for item in &items {
            let Item::Label(label) = item else { continue };
            let Some(slot) = placed.get_mut(label.0 as usize) else {
                return Err(AssembleError::UnplacedLabel(*label));
            };
            if std::mem::replace(slot, true) {
                return Err(AssembleError::DuplicateLabel(*label));
            }
        }

        // Switch padding depends on position, and widening a branch moves everything after it.
        // Iterate both to a fixed point. Widening is monotonic: a wide branch never shrinks again,
        // matching ASM's resize pass and guaranteeing convergence.
        let mut offsets = vec![0usize; items.len() + 1];
        loop {
            let mut at = 0;
            let mut changed = false;
            for (index, item) in items.iter().enumerate() {
                changed |= offsets[index] != at;
                offsets[index] = at;
                if let Item::Code(encoded) = item {
                    at += size_at(encoded, at);
                }
            }
            changed |= offsets[items.len()] != at;
            offsets[items.len()] = at;
            if at > u16::MAX as usize {
                return Err(AssembleError::CodeTooLarge);
            }

            let mut label_offsets = vec![None; self.label_count as usize];
            for (index, item) in items.iter().enumerate() {
                if let Item::Label(label) = item {
                    label_offsets[label.0 as usize] = Some(offsets[index]);
                }
            }
            let mut widened = false;
            for (index, item) in items.iter_mut().enumerate() {
                let Item::Code(Encoded::Jump { target, wide, .. }) = item else {
                    continue;
                };
                let to = label_offsets
                    .get(target.0 as usize)
                    .copied()
                    .flatten()
                    .ok_or(AssembleError::UnplacedLabel(*target))?;
                let delta = to as isize - offsets[index] as isize;
                if !*wide && i16::try_from(delta).is_err() {
                    *wide = true;
                    widened = true;
                }
            }
            if !changed && !widened {
                break;
            }
        }
        let code_len = offsets[items.len()];

        let mut label_offsets = vec![None; self.label_count as usize];
        for (index, item) in items.iter().enumerate() {
            if let Item::Label(label) = item {
                if let Some(slot) = label_offsets.get_mut(label.0 as usize) {
                    *slot = Some(offsets[index]);
                }
            }
        }
        let offset_of = |label: LabelId| {
            label_offsets
                .get(label.0 as usize)
                .copied()
                .flatten()
                .ok_or(AssembleError::UnplacedLabel(label))
        };

        let mut code = Vec::with_capacity(code_len);
        for (index, item) in items.iter().enumerate() {
            let Item::Code(encoded) = item else { continue };
            let here = offsets[index];
            let relative = |label: LabelId| -> Result<i32, AssembleError> {
                Ok(offset_of(label)? as i32 - here as i32)
            };
            match encoded {
                Encoded::Fixed(bytes) => code.extend_from_slice(bytes),
                Encoded::Jump {
                    op,
                    target,
                    wide: false,
                } => {
                    let delta = i16::try_from(relative(*target)?)
                        .expect("layout widens every branch outside the signed-u16 range");
                    code.push(*op);
                    code.extend(delta.to_be_bytes());
                }
                Encoded::Jump {
                    op: op @ (0xa7 | 0xa8),
                    target,
                    wide: true,
                } => {
                    code.push(if *op == 0xa7 { 0xc8 } else { 0xc9 });
                    code.extend(relative(*target)?.to_be_bytes());
                }
                Encoded::Jump {
                    op,
                    target,
                    wide: true,
                } => {
                    code.push(inverse_condition(*op).expect("only conditions reach this arm"));
                    code.extend(8i16.to_be_bytes());
                    code.push(0xc8);
                    let goto_at = here + 3;
                    let delta = offset_of(*target)? as i32 - goto_at as i32;
                    code.extend(delta.to_be_bytes());
                }
                Encoded::TableSwitch {
                    low,
                    high,
                    default,
                    labels,
                } => {
                    code.push(0xaa);
                    code.resize(code.len() + (4 - (here + 1) % 4) % 4, 0);
                    code.extend(relative(*default)?.to_be_bytes());
                    code.extend(low.to_be_bytes());
                    code.extend(high.to_be_bytes());
                    for label in labels {
                        code.extend(relative(*label)?.to_be_bytes());
                    }
                }
                Encoded::LookupSwitch {
                    default,
                    keys,
                    labels,
                } => {
                    code.push(0xab);
                    code.resize(code.len() + (4 - (here + 1) % 4) % 4, 0);
                    code.extend(relative(*default)?.to_be_bytes());
                    code.extend((keys.len() as i32).to_be_bytes());
                    for (key, label) in keys.iter().zip(labels) {
                        code.extend(key.to_be_bytes());
                        code.extend(relative(*label)?.to_be_bytes());
                    }
                }
            }
        }

        let exception_table = self
            .try_catch_blocks
            .iter()
            .zip(catch_types)
            .map(|(block, catch_type)| {
                Ok((
                    offset_of(block.start)? as u16,
                    offset_of(block.end)? as u16,
                    offset_of(block.handler)? as u16,
                    catch_type,
                ))
            })
            .collect::<Result<_, AssembleError>>()?;
        let line_numbers = self
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Line { line, start } => Some(offset_of(*start).map(|pc| (pc as u16, *line))),
                _ => None,
            })
            .collect::<Result<_, _>>()?;
        let local_variables = self
            .local_variables
            .iter()
            .map(|local| {
                let (start, end) = (offset_of(local.start)?, offset_of(local.end)?);
                if end < start {
                    return Err(AssembleError::InvertedLocalRange(local.start, local.end));
                }
                Ok(AssembledLocal {
                    start_pc: start as u16,
                    length: (end - start) as u16,
                    slot: local.slot,
                    name: local.name.clone(),
                    desc: local.desc.clone(),
                })
            })
            .collect::<Result<_, _>>()?;

        Ok(AssembledCode {
            max_stack: self.max_stack,
            max_locals: self.max_locals,
            code,
            exception_table,
            line_numbers,
            local_variables,
            label_offsets: label_offsets
                .into_iter()
                .map(|offset| offset.map(|offset| offset as u16))
                .collect(),
        })
    }
}

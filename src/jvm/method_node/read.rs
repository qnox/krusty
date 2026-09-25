//! Decoding a class-file method into a [`MethodNode`].
//!
//! Every byte offset the method's tables name (a branch or switch target, a try/catch boundary, a
//! line start, a local's range) becomes one label, numbered in offset order so two reads of the
//! same body give equal nodes. Every pool index becomes the symbolic operand it names in the
//! DEFINING class's pool, which is what frees the body to move into another class.

use std::collections::{BTreeMap, BTreeSet};

use super::nodes::{Insn, LabelId, LocalVariable, MethodNode, Node, TryCatchBlock};
use super::pool::{ConstantPoolView, SourcePool};
use crate::jvm::bytecode::instruction_len;
use crate::jvm::classreader::{ExcEntry, MethodCode, MethodLocal};

/// Why a body could not be decoded: the offset of the offending instruction or table entry, and
/// what was wrong there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MalformedCode {
    pub offset: usize,
    pub reason: &'static str,
}

pub(super) fn malformed(offset: usize, reason: &'static str) -> MalformedCode {
    MalformedCode { offset, reason }
}

const GOTO: u8 = 0xa7;
const JSR: u8 = 0xa8;

fn u1(code: &[u8], at: usize) -> u8 {
    code[at]
}

fn u2(code: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([code[at], code[at + 1]])
}

fn i4(code: &[u8], at: usize) -> i32 {
    i32::from_be_bytes([code[at], code[at + 1], code[at + 2], code[at + 3]])
}

/// Where a switch's operands start: after the opcode and the padding to a 4-byte boundary.
fn switch_operands(pc: usize) -> usize {
    pc + 1 + (4 - (pc + 1) % 4) % 4
}

/// Every byte offset the instruction at `pc` may transfer control to.
fn branch_targets(code: &[u8], pc: usize) -> Vec<isize> {
    let relative = |delta: isize| pc as isize + delta;
    match code[pc] {
        0x99..=0xa8 | 0xc6 | 0xc7 => vec![relative(u2(code, pc + 1) as i16 as isize)],
        0xc8 | 0xc9 => vec![relative(i4(code, pc + 1) as isize)],
        0xaa => {
            let p = switch_operands(pc);
            let (low, high) = (i4(code, p + 4), i4(code, p + 8));
            let count = (high as i64 - low as i64 + 1).max(0) as usize;
            std::iter::once(i4(code, p))
                .chain((0..count).map(|k| i4(code, p + 12 + k * 4)))
                .map(|delta| relative(delta as isize))
                .collect()
        }
        0xab => {
            let p = switch_operands(pc);
            let pairs = i4(code, p + 4).max(0) as usize;
            std::iter::once(i4(code, p))
                .chain((0..pairs).map(|k| i4(code, p + 12 + k * 8)))
                .map(|delta| relative(delta as isize))
                .collect()
        }
        _ => Vec::new(),
    }
}

/// A method's `Code` attribute as far as a node needs it: the bytes and the tables keyed by their
/// offsets, every pool reference still an index into the pool it is decoded against.
#[derive(Clone, Copy, Debug)]
pub struct CodeAttribute<'a> {
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: &'a [u8],
    pub handlers: &'a [ExcEntry],
    /// `(start_pc, line)` in table order.
    pub lines: &'a [(u16, u16)],
    pub locals: &'a [MethodLocal],
}

impl<'a> From<&'a MethodCode> for CodeAttribute<'a> {
    fn from(body: &'a MethodCode) -> CodeAttribute<'a> {
        CodeAttribute {
            max_stack: body.max_stack,
            max_locals: body.max_locals,
            code: &body.code,
            handlers: &body.handlers,
            lines: &body.lines,
            locals: &body.locals,
        }
    }
}

impl MethodNode {
    /// Decode `body`, the `Code` of the method `name``desc` with `access` flags, against the pool
    /// of the class it was read from.
    pub fn read(
        access: u16,
        name: &str,
        desc: &str,
        body: &MethodCode,
    ) -> Result<MethodNode, MalformedCode> {
        MethodNode::read_code(access, name, desc, &CodeAttribute::from(body), body)
    }

    /// Decode `body` against `pool`, the pool of the class the body was written into.
    ///
    /// Fails on anything the node form cannot represent faithfully: a truncated or unknown
    /// instruction, a table naming an offset inside an instruction, a pool entry of the wrong kind,
    /// or one of krusty's own coroutine-site markers (which never reach a class file). A branch to
    /// the end of the code and a line number starting there are malformed (JVMS 4.9.2, 4.7.12); a
    /// protected or local range may end there, and the node then ends with a label.
    pub fn read_code(
        access: u16,
        name: &str,
        desc: &str,
        body: &CodeAttribute<'_>,
        pool: &(impl ConstantPoolView + ?Sized),
    ) -> Result<MethodNode, MalformedCode> {
        let code = body.code;
        let end = code.len();

        let mut starts = Vec::new();
        let mut pc = 0;
        while pc < end {
            if code[pc] == 0xfe {
                return Err(malformed(pc, "coroutine-site marker in a class-file body"));
            }
            let len = instruction_len(code, pc)
                .filter(|len| pc + len <= end)
                .ok_or_else(|| malformed(pc, "truncated instruction"))?;
            starts.push(pc);
            pc += len;
        }
        let is_boundary = |offset: usize| offset == end || starts.binary_search(&offset).is_ok();

        let mut label_offsets = BTreeSet::new();
        let mut mark = |offset: isize, at: usize, reason| {
            let offset = usize::try_from(offset).map_err(|_| malformed(at, reason))?;
            if !is_boundary(offset) {
                return Err(malformed(at, reason));
            }
            label_offsets.insert(offset);
            Ok(())
        };
        for &pc in &starts {
            for target in branch_targets(code, pc) {
                if target as usize == end {
                    return Err(malformed(pc, "branch past the last instruction"));
                }
                mark(target, pc, "branch into the middle of an instruction")?;
            }
        }
        for handler in body.handlers {
            let at = handler.start_pc as usize;
            for offset in [handler.start_pc, handler.end_pc, handler.handler_pc] {
                mark(
                    offset as isize,
                    at,
                    "exception range off an instruction boundary",
                )?;
            }
        }
        for &(start, _) in body.lines {
            if start as usize == end {
                return Err(malformed(end, "line number past the last instruction"));
            }
            mark(
                start as isize,
                start as usize,
                "line number off an instruction boundary",
            )?;
        }
        for local in body.locals {
            let (start, stop) = (
                local.start_pc as usize,
                local.start_pc as usize + local.length as usize,
            );
            mark(
                start as isize,
                start,
                "local variable off an instruction boundary",
            )?;
            mark(
                stop as isize,
                start,
                "local variable off an instruction boundary",
            )?;
        }
        let labels: BTreeMap<usize, LabelId> = label_offsets
            .into_iter()
            .enumerate()
            .map(|(id, offset)| (offset, LabelId(id as u32)))
            .collect();
        let label = |offset: isize| labels[&(offset as usize)];

        let mut lines_at: BTreeMap<usize, Vec<u16>> = BTreeMap::new();
        for &(start, line) in body.lines {
            lines_at.entry(start as usize).or_default().push(line);
        }

        let mut pool = SourcePool { pool, at: 0 };
        let mut nodes = Vec::with_capacity(starts.len() + labels.len());
        for &pc in &starts {
            pool.at = pc;
            if let Some(&id) = labels.get(&pc) {
                nodes.push(Node::Label(id));
                for &line in lines_at.get(&pc).into_iter().flatten() {
                    nodes.push(Node::Line { line, start: id });
                }
            }
            nodes.push(Node::Insn(decode(code, pc, &pool, &label)?));
        }
        if let Some(&id) = labels.get(&end) {
            nodes.push(Node::Label(id));
        }

        pool.at = 0;
        let try_catch_blocks = body
            .handlers
            .iter()
            .map(|handler| {
                Ok(TryCatchBlock {
                    start: label(handler.start_pc as isize),
                    end: label(handler.end_pc as isize),
                    handler: label(handler.handler_pc as isize),
                    catch_type: match handler.catch_type {
                        0 => None,
                        index => Some(pool.class(index)?),
                    },
                })
            })
            .collect::<Result<_, MalformedCode>>()?;
        let local_variables = body
            .locals
            .iter()
            .map(|local| LocalVariable {
                name: local.name.clone(),
                desc: local.descriptor.clone(),
                start: label(local.start_pc as isize),
                end: label(local.start_pc as isize + local.length as isize),
                slot: local.slot,
            })
            .collect();

        Ok(MethodNode {
            access,
            name: name.to_string(),
            desc: desc.to_string(),
            nodes,
            try_catch_blocks,
            local_variables,
            max_stack: body.max_stack,
            max_locals: body.max_locals,
            label_count: labels.len() as u32,
        })
    }
}

impl Insn {
    /// Decode `bytes`, exactly one instruction that names no label (anything but a jump or a
    /// switch), against `pool`.
    pub fn decode(
        bytes: &[u8],
        pool: &(impl ConstantPoolView + ?Sized),
    ) -> Result<Insn, MalformedCode> {
        match bytes.first() {
            None => return Err(malformed(0, "truncated instruction")),
            Some(0xfe) => return Err(malformed(0, "coroutine-site marker in a class-file body")),
            Some(_) => {}
        }
        if instruction_len(bytes, 0) != Some(bytes.len()) {
            return Err(malformed(0, "not exactly one instruction"));
        }
        if matches!(bytes[0], 0x99..=0xa8 | 0xaa | 0xab | 0xc6..=0xc9) {
            return Err(malformed(0, "a jump or switch needs its labels"));
        }
        decode(bytes, 0, &SourcePool { pool, at: 0 }, &|_| {
            unreachable!("an instruction that is no jump or switch names no label")
        })
    }
}

/// Decode the instruction at `pc`, whose length and branch targets are already validated.
fn decode(
    code: &[u8],
    pc: usize,
    pool: &SourcePool<'_, impl ConstantPoolView + ?Sized>,
    label: &impl Fn(isize) -> LabelId,
) -> Result<Insn, MalformedCode> {
    let op = code[pc];
    let targets = branch_targets(code, pc);
    Ok(match op {
        0x00..=0x0f
        | 0x2e..=0x35
        | 0x4f..=0x83
        | 0x85..=0x98
        | 0xac..=0xb1
        | 0xbe
        | 0xbf
        | 0xc2
        | 0xc3 => Insn::Op(op),
        0x10 => Insn::Int {
            op,
            operand: u1(code, pc + 1) as i8 as i32,
        },
        0x11 => Insn::Int {
            op,
            operand: u2(code, pc + 1) as i16 as i32,
        },
        0xbc => Insn::Int {
            op,
            operand: u1(code, pc + 1) as i32,
        },
        0x12 => Insn::Ldc(pool.constant(u1(code, pc + 1) as u16)?),
        0x13 | 0x14 => Insn::Ldc(pool.constant(u2(code, pc + 1))?),
        0x15..=0x19 | 0x36..=0x3a | 0xa9 => Insn::Var {
            op,
            slot: u1(code, pc + 1) as u16,
        },
        0x1a..=0x2d => Insn::Var {
            op: 0x15 + (op - 0x1a) / 4,
            slot: ((op - 0x1a) % 4) as u16,
        },
        0x3b..=0x4e => Insn::Var {
            op: 0x36 + (op - 0x3b) / 4,
            slot: ((op - 0x3b) % 4) as u16,
        },
        0x84 => Insn::Iinc {
            slot: u1(code, pc + 1) as u16,
            delta: u1(code, pc + 2) as i8 as i16,
        },
        0x99..=0xa8 | 0xc6 | 0xc7 => Insn::Jump {
            op,
            target: label(targets[0]),
        },
        0xc8 => Insn::Jump {
            op: GOTO,
            target: label(targets[0]),
        },
        0xc9 => Insn::Jump {
            op: JSR,
            target: label(targets[0]),
        },
        0xaa => {
            let p = switch_operands(pc);
            Insn::TableSwitch {
                low: i4(code, p + 4),
                high: i4(code, p + 8),
                default: label(targets[0]),
                labels: targets[1..].iter().map(|&target| label(target)).collect(),
            }
        }
        0xab => {
            let p = switch_operands(pc);
            let pairs = i4(code, p + 4).max(0) as usize;
            Insn::LookupSwitch {
                default: label(targets[0]),
                keys: (0..pairs).map(|k| i4(code, p + 8 + k * 8)).collect(),
                labels: targets[1..].iter().map(|&target| label(target)).collect(),
            }
        }
        0xb2..=0xb5 => {
            let (owner, name, desc) = pool.field(u2(code, pc + 1))?;
            Insn::Field {
                op,
                owner,
                name,
                desc,
            }
        }
        0xb6..=0xb9 => {
            let (owner, name, desc, interface) = pool.method(u2(code, pc + 1))?;
            Insn::Method {
                op,
                owner,
                name,
                desc,
                interface,
            }
        }
        0xba => pool.invoke_dynamic(u2(code, pc + 1))?,
        0xbb | 0xbd | 0xc0 | 0xc1 => Insn::Type {
            op,
            class: pool.class(u2(code, pc + 1))?,
        },
        0xc4 => match code[pc + 1] {
            0x84 => Insn::Iinc {
                slot: u2(code, pc + 2),
                delta: u2(code, pc + 4) as i16,
            },
            wide @ (0x15..=0x19 | 0x36..=0x3a | 0xa9) => Insn::Var {
                op: wide,
                slot: u2(code, pc + 2),
            },
            _ => return Err(pool.fail("wide prefix on an instruction that has no wide form")),
        },
        0xc5 => Insn::MultiANewArray {
            desc: pool.class(u2(code, pc + 1))?,
            dims: u1(code, pc + 3),
        },
        _ => return Err(pool.fail("unknown opcode")),
    })
}

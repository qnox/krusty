//! The `StackMapTable` a method carries, computed from its final instructions the way kotlinc's
//! class writer computes it.
//!
//! kotlinc records no frame while it generates code. Its writer is an ASM
//! `ClassWriter(COMPUTE_MAXS | COMPUTE_FRAMES)`, so the table is ASM's dataflow over the method as
//! written (see [`FrameComputation`]), encoded against the previous frame, and every block nothing
//! reaches is rewritten to `nop`s ending in `athrow` and cut out of the protected ranges. This module
//! does the same for krusty's methods.
//!
//! It runs twice per method:
//!
//! - when the method is added, so the classes its frames name are interned where ASM interns them:
//!   after the method's own constants and before the next method's, from the body kotlinc's
//!   bytecode rewrites leave;
//! - when the class is written, after kotlinc's bytecode rewrites, over the final code, exception
//!   table, line numbers and local ranges. This is the table the class carries.
//!
//! Block boundaries shape a handler's merged locals. ASM splits a block at every label, and the
//! labels that survive into a class file are the ones a jump, a protected range, a line number or a
//! local's range stands at; those are the ones used here, which reproduces every frame of kotlinc's
//! own output.
//!
//! A non-empty emitted body the computation cannot step is an internal backend error. There is no
//! recorded-frame fallback: accepting one would make two authorities for the verifier state and
//! preserve exactly the emitter-specific bookkeeping this path replaces.

use std::ops::Range;

use super::bytecode_analysis::{
    entry_frame, ComputedFrame, ComputedFrames, Decline, FrameComputation, Handler, PoolView,
    TypedHandler, VerificationType,
};
use super::{u2, ClassWriter, ConstPool, LvtEntry};
use crate::jvm::inline::{disassemble, insn_offsets_at};

const NOP: u8 = 0x00;
const ATHROW: u8 = 0xbf;

/// A method's body as the frame computation reads it.
pub(super) struct Body<'a> {
    pub access: u16,
    pub name: &'a str,
    pub descriptor: &'a str,
    pub code: &'a [u8],
    /// `(start, end, handler, catch type)` by byte offset.
    pub exceptions: &'a [(u16, u16, u16, u16)],
    /// Byte offsets a label stood at besides jumps and protected ranges: line numbers and the
    /// bounds of local ranges.
    pub labels: Vec<usize>,
}

/// The computed frames of a body, with the byte offset of each instruction.
pub(super) struct Computed {
    frames: ComputedFrames,
    offsets: Vec<usize>,
}

impl Computed {
    /// The byte ranges of the unreachable blocks, each ending before the next block starts.
    fn unreachable_bytes(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.frames
            .unreachable
            .iter()
            .map(|range| self.offsets[range.start]..self.offsets[range.end])
    }
}

impl ClassWriter {
    /// The frames `body` implies, or why none can be computed.
    pub(super) fn compute_frames(&self, body: &Body<'_>) -> Result<Computed, Decline> {
        let insns = disassemble(body.code).ok_or(Decline::UnsupportedControlFlow)?;
        let offsets = insn_offsets_at(&insns, 0);
        if offsets.last() != Some(&body.code.len()) {
            return Err(Decline::UnsupportedControlFlow);
        }
        let index_of = |pc: u16| offsets.binary_search(&usize::from(pc)).ok();
        let handlers = body
            .exceptions
            .iter()
            .map(|&(start, end, handler, catch_type)| {
                Some(TypedHandler {
                    range: Handler {
                        start: index_of(start)?,
                        end: index_of(end)?,
                        handler: index_of(handler)?,
                    },
                    catch_type: match catch_type {
                        0 => None,
                        index => Some(self.class_name(index)?.to_string()),
                    },
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(Decline::UnsupportedControlFlow)?;
        let mut labels = vec![false; insns.len() + 1];
        for &pc in &body.labels {
            let at = offsets
                .binary_search(&pc)
                .map_err(|_| Decline::UnsupportedControlFlow)?;
            labels[at] = true;
        }
        let frames = FrameComputation {
            insns: &insns,
            handlers: &handlers,
            labels: &labels,
            this_class: &self.internal_name,
            pool: self,
        }
        .compute(body.access, body.name, body.descriptor)?;
        Ok(Computed { frames, offsets })
    }

    /// Encode `computed` as a `StackMapTable` body, interning the classes the written entries name
    /// in the order ASM writes them. `None` when the method needs no table.
    pub(super) fn encode_frames(
        &mut self,
        body: &Body<'_>,
        computed: &Computed,
    ) -> Option<Vec<u8>> {
        if computed.frames.frames.is_empty() {
            return None;
        }
        let entry = entry_frame(body.access, body.name, body.descriptor, &self.internal_name)
            .expect("a successfully computed method must have a valid entry frame");
        Some(encode(
            &computed.frames.frames,
            &entry,
            &computed.offsets,
            &mut self.cp,
        ))
    }

    /// Intern the classes the table of the method last added names, where kotlinc's writer interns
    /// them. ASM computes that table after kotlinc's bytecode rewrites, so it is the table of the
    /// rewritten body, in which a folded temporary names no class. A constructor or class
    /// initializer gets its line and local tables only after it is added, so its rewrite cannot be
    /// decided yet; its table as emitted stands in.
    pub(super) fn intern_frame_classes(&mut self, body: &Body<'_>, computed: &Computed) {
        let index = self.methods.len() - 1;
        let method = &self.methods[index];
        let rewritten = if body.name == "<init>" || body.name == "<clinit>" {
            None
        } else {
            method
                .rewrite_source
                .as_deref()
                .and_then(|source| self.rewritten(method, source))
        };
        let Some(rewritten) = rewritten else {
            self.encode_frames(body, computed);
            return;
        };
        let body = Body {
            access: body.access,
            name: body.name,
            descriptor: body.descriptor,
            code: &rewritten.code,
            exceptions: &rewritten.exceptions,
            labels: table_labels(&rewritten.lnt, &rewritten.lvt, rewritten.code.len()),
        };
        let computed = self.compute_frames(&body).unwrap_or_else(|decline| {
            panic!(
                "cannot compute rewritten JVM frames for {}{}: {decline:?}",
                body.name, body.descriptor
            )
        });
        self.encode_frames(&body, &computed);
    }

    /// Replace every method's table with the one its final body implies, rewriting unreachable
    /// blocks as ASM does. Every non-empty emitted body must pass the authoritative computation.
    pub(super) fn compute_stack_maps(&mut self) {
        for index in 0..self.methods.len() {
            let method = &self.methods[index];
            let Some(code) = method.code.as_deref().filter(|code| !code.is_empty()) else {
                continue;
            };
            let name = self
                .cp
                .utf8_at(method.name)
                .expect("an added method must retain its name")
                .to_string();
            let descriptor = self
                .cp
                .utf8_at(method.desc)
                .expect("an added method must retain its descriptor")
                .to_string();
            let body = Body {
                access: method.access,
                name: &name,
                descriptor: &descriptor,
                code,
                exceptions: &method.exceptions,
                labels: table_labels(&method.lnt, &method.lvt, code.len()),
            };
            let computed = self.compute_frames(&body).unwrap_or_else(|decline| {
                panic!("cannot compute final JVM frames for {name}{descriptor}: {decline:?}")
            });
            let dead: Vec<Range<usize>> = computed.unreachable_bytes().collect();
            let mut code = code.to_vec();
            let mut exceptions = method.exceptions.clone();
            for range in &dead {
                code[range.start..range.end - 1].fill(NOP);
                code[range.end - 1] = ATHROW;
                exceptions = remove_range(&exceptions, range);
            }
            let body = Body {
                access: body.access,
                name: &name,
                descriptor: &descriptor,
                code: &code,
                exceptions: &exceptions,
                labels: Vec::new(),
            };
            let stackmap = self.encode_frames(&body, &computed);
            let method = &mut self.methods[index];
            if !dead.is_empty() {
                method.max_stack = method.max_stack.max(1);
                method.code = Some(code);
                method.exceptions = exceptions;
            }
            method.stackmap = stackmap;
        }
    }
}

/// The byte offsets a method's line numbers and local ranges put a label at.
fn table_labels(lnt: &[(u16, u16)], lvt: &[LvtEntry], code_len: usize) -> Vec<usize> {
    let mut labels: Vec<usize> = lnt.iter().map(|&(pc, _)| usize::from(pc)).collect();
    for &(_, _, _, start, length) in lvt {
        let start = usize::from(start.unwrap_or(0));
        labels.push(start);
        labels.push(length.map_or(code_len, |length| start + usize::from(length)));
    }
    labels
}

/// The labels of an emitted body that the class file will record: its line marks and the bounds of
/// its local ranges.
pub(super) fn builder_labels(
    lines: &[(u16, u16)],
    locals: &[(u16, Option<u16>, u16, String, String)],
    code_len: usize,
) -> Vec<usize> {
    let mut labels: Vec<usize> = lines.iter().map(|&(pc, _)| usize::from(pc)).collect();
    for (start, length, ..) in locals {
        labels.push(usize::from(*start));
        labels.push(length.map_or(code_len, |length| usize::from(*start) + usize::from(length)));
    }
    labels
}

/// The exception table with `dead` cut out of every protected range, as ASM's
/// `Handler.removeRange` does: a range inside it goes, a range overlapping one end is shortened,
/// and a range around it is split in two, in place.
fn remove_range(
    exceptions: &[(u16, u16, u16, u16)],
    dead: &Range<usize>,
) -> Vec<(u16, u16, u16, u16)> {
    let (cut_start, cut_end) = (dead.start as u16, dead.end as u16);
    let mut out = Vec::with_capacity(exceptions.len() + 1);
    for &(start, end, handler, catch_type) in exceptions {
        if cut_start >= end || cut_end <= start {
            out.push((start, end, handler, catch_type));
        } else if cut_start <= start {
            if cut_end < end {
                out.push((cut_end, end, handler, catch_type));
            }
        } else if cut_end >= end {
            out.push((start, cut_start, handler, catch_type));
        } else {
            out.push((start, cut_start, handler, catch_type));
            out.push((cut_end, end, handler, catch_type));
        }
    }
    out
}

/// ASM's frame compression: each frame against the previous one (the entry frame first), as a
/// `same`, `same_locals_1_stack_item`, `chop`, `append` or `full` frame.
fn encode(
    frames: &[ComputedFrame],
    entry: &[VerificationType],
    offsets: &[usize],
    cp: &mut ConstPool,
) -> Vec<u8> {
    let mut body = Vec::new();
    u2(&mut body, frames.len() as u16);
    let mut previous_locals: &[VerificationType] = entry;
    let mut previous_offset: Option<usize> = None;
    for frame in frames {
        let offset = offsets[frame.index];
        let delta = match previous_offset {
            None => offset,
            Some(previous) => offset - previous - 1,
        } as u16;
        previous_offset = Some(offset);
        let (locals, stack) = (&frame.locals[..], &frame.stack[..]);
        let common = locals.len().min(previous_locals.len());
        let prefix = locals[..common] == previous_locals[..common];
        let same_locals = prefix && locals.len() == previous_locals.len();
        let grown = locals.len() as isize - previous_locals.len() as isize;
        let mut write = |body: &mut Vec<u8>, types: &[VerificationType]| {
            for value in types {
                write_type(value, offsets, body, cp);
            }
        };
        if stack.is_empty() && same_locals {
            if delta < 64 {
                body.push(delta as u8);
            } else {
                body.push(251);
                u2(&mut body, delta);
            }
        } else if stack.is_empty() && prefix && (1..=3).contains(&grown) {
            body.push((251 + grown) as u8);
            u2(&mut body, delta);
            write(&mut body, &locals[previous_locals.len()..]);
        } else if stack.is_empty() && prefix && (-3..=-1).contains(&grown) {
            body.push((251 + grown) as u8);
            u2(&mut body, delta);
        } else if stack.len() == 1 && same_locals {
            if delta < 64 {
                body.push(64 + delta as u8);
            } else {
                body.push(247);
                u2(&mut body, delta);
            }
            write(&mut body, stack);
        } else {
            body.push(255);
            u2(&mut body, delta);
            u2(&mut body, locals.len() as u16);
            write(&mut body, locals);
            u2(&mut body, stack.len() as u16);
            write(&mut body, stack);
        }
        previous_locals = locals;
    }
    body
}

fn write_type(value: &VerificationType, offsets: &[usize], out: &mut Vec<u8>, cp: &mut ConstPool) {
    match value {
        VerificationType::Top => out.push(0),
        VerificationType::Integer => out.push(1),
        VerificationType::Float => out.push(2),
        VerificationType::Double => out.push(3),
        VerificationType::Long => out.push(4),
        VerificationType::Null => out.push(5),
        VerificationType::UninitializedThis => out.push(6),
        VerificationType::Reference(name) => {
            out.push(7);
            u2(out, cp.class(name));
        }
        VerificationType::Uninitialized(index) => {
            out.push(8);
            u2(out, offsets[*index] as u16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_debug_label_between_instructions_is_rejected() {
        let writer = ClassWriter::new("C", "java/lang/Object");
        let body = Body {
            access: 0x0008,
            name: "f",
            descriptor: "()V",
            code: &[0x11, 0, 1, 0x57, 0xb1], // sipush 1; pop; return
            exceptions: &[],
            labels: vec![1],
        };
        assert_eq!(
            writer.compute_frames(&body).err(),
            Some(Decline::UnsupportedControlFlow)
        );
    }

    #[test]
    fn a_dead_range_is_cut_out_of_the_ranges_it_touches() {
        let exceptions = [
            (0, 4, 20, 0),   // ends inside: shortened
            (6, 12, 20, 0),  // starts inside: shortened
            (2, 16, 20, 3),  // around: split
            (4, 8, 20, 0),   // inside: removed
            (12, 14, 20, 0), // disjoint: kept
        ];
        assert_eq!(
            remove_range(&exceptions, &(3..9)),
            vec![
                (0, 3, 20, 0),
                (9, 12, 20, 0),
                (2, 3, 20, 3),
                (9, 16, 20, 3),
                (12, 14, 20, 0),
            ]
        );
    }

    #[test]
    fn frames_compress_against_the_previous_frame_like_asm() {
        let int = VerificationType::Integer;
        let frames = [
            // append one local, 3 bytes in
            ComputedFrame {
                index: 1,
                locals: vec![int.clone(), int.clone()],
                stack: vec![],
            },
            // same locals, one stack item
            ComputedFrame {
                index: 2,
                locals: vec![int.clone(), int.clone()],
                stack: vec![int.clone()],
            },
            // chop back to the entry
            ComputedFrame {
                index: 3,
                locals: vec![int.clone()],
                stack: vec![],
            },
            // a different local: full
            ComputedFrame {
                index: 4,
                locals: vec![VerificationType::Float],
                stack: vec![],
            },
        ];
        let offsets = [0, 3, 5, 9, 70, 71];
        let mut cp = ConstPool::default();
        let table = encode(&frames, std::slice::from_ref(&int), &offsets, &mut cp);
        assert_eq!(
            table,
            vec![
                0,
                4, //
                252,
                0,
                3,
                1, // append_frame(1), delta 3, int
                64 + 1,
                1, // same_locals_1_stack_item, delta 1, int
                250,
                0,
                3, // chop_frame(1), delta 3
                255,
                0,
                60,
                0,
                1,
                2,
                0,
                0, // full_frame, delta 60, [float], []
            ]
        );
    }
}

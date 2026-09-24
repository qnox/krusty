//! Audit a written class's `StackMapTable`s against the frames kotlinc's writer would compute for the
//! same instructions.
//!
//! Every frame kotlinc writes is ASM's `COMPUTE_FRAMES` over the method's final instructions (see
//! [`FrameComputation`]). Running that computation over a class file and comparing the result with
//! the table the class carries answers two questions:
//!
//! - over kotlinc's own classes, whether the computation reproduces kotlinc's frames (it should,
//!   exactly, before krusty relies on it);
//! - over krusty's classes, where the frames krusty records while emitting differ from what the
//!   same instructions imply.
//!
//! The audit reads only the class file. Label positions a class file does not record (a label no
//! line, local, range or jump refers to) cannot be recovered, so a handler merge can in principle
//! see coarser blocks than the writer did.

use crate::jvm::classfile::bytecode_analysis::{
    entry_frame, ComputedFrame, Decline, FrameComputation, Handler, PoolView, TypedHandler,
    VerificationType,
};
use crate::jvm::classfile::VerifType;
use crate::jvm::classreader::{parse_class, read_method_code, MethodCode, C};
use crate::jvm::inline::{disassemble, insn_offsets_at};

/// One method's audit.
#[derive(Clone, Debug)]
pub struct MethodAudit {
    pub name: String,
    pub descriptor: String,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The class carries exactly the computed frames (`frames` of them; zero for a method with no
    /// table and no frame computed).
    Identical { frames: usize },
    /// The first difference: what kind it is, and it rendered for a reader.
    Different { kind: Difference, first: String },
    /// No frames could be computed, or the stored table could not be read.
    Declined { reason: String },
}

/// What the first difference between a stored and a computed table is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Difference {
    /// The class frames code nothing reaches, which kotlinc's writer would have replaced with
    /// `nop`s and an `athrow` (kotlinc's own dead-code pass removes it first).
    UnreachableCode,
    /// A stored frame where nothing jumps.
    FrameWithoutJump,
    /// A jump target the stored table does not frame.
    MissingFrame,
    /// The operand stack differs.
    Stack,
    /// A local the stored frame drops to `top` holds a value on every path.
    LocalDropped,
    /// A local holds a different type (a declared type where the stored value is narrower or
    /// wider, for example).
    LocalType,
}

impl Difference {
    pub fn describe(self) -> &'static str {
        match self {
            Difference::UnreachableCode => "unreachable code kept and framed",
            Difference::FrameWithoutJump => "frame where nothing jumps",
            Difference::MissingFrame => "jump target without a frame",
            Difference::Stack => "operand stack differs",
            Difference::LocalDropped => "local dropped to top while it still holds a value",
            Difference::LocalType => "local typed differently",
        }
    }
}

/// Audit every method with code in `bytes`. `Err` when the class itself cannot be parsed.
pub fn audit_class(bytes: &[u8]) -> ClassAudit {
    let class = parse_class(bytes).map_err(|error| format!("{error:?}"))?;
    let mut audits = Vec::new();
    for method in &class.methods {
        if method.access & (0x0400 | 0x0100) != 0 {
            continue; // abstract or native: no code
        }
        let outcome = match read_method_code(bytes, &method.name, &method.descriptor) {
            None => Outcome::Declined {
                reason: "code unreadable".to_string(),
            },
            // javac writes its own frames; only a Kotlin class's frames are kotlinc's.
            Some(code)
                if code
                    .source_file
                    .as_deref()
                    .is_some_and(|file| file.ends_with(".java")) =>
            {
                Outcome::Declined {
                    reason: "compiled from Java".to_string(),
                }
            }
            Some(code) => audit_method(&code, method.access, &method.name, &method.descriptor),
        };
        audits.push(MethodAudit {
            name: method.name.clone(),
            descriptor: method.descriptor.clone(),
            outcome,
        });
    }
    Ok(audits)
}

fn audit_method(code: &MethodCode, access: u16, name: &str, descriptor: &str) -> Outcome {
    // The name as the class file spells it: the parsed class's rendered name is not byte-exact.
    let this_class = code.defining_class.as_str();
    let declined = |reason: String| Outcome::Declined { reason };
    let Some(insns) = disassemble(&code.code) else {
        return declined("code does not disassemble".to_string());
    };
    let offsets = insn_offsets_at(&insns, 0);
    let index_of = |pc: usize| offsets.binary_search(&pc).ok();
    let pool = ClassPool(&code.source_cp);
    let mut handlers = Vec::with_capacity(code.handlers.len());
    for entry in &code.handlers {
        let (Some(start), Some(end), Some(handler)) = (
            index_of(usize::from(entry.start_pc)),
            index_of(usize::from(entry.end_pc)),
            index_of(usize::from(entry.handler_pc)),
        ) else {
            return declined("a protected range is not on an instruction boundary".to_string());
        };
        let catch_type = match entry.catch_type {
            0 => None,
            index => match pool.class_name(index) {
                Some(name) => Some(name.to_string()),
                None => return declined(format!("catch type #{index} is not a class")),
            },
        };
        handlers.push(TypedHandler {
            range: Handler {
                start,
                end,
                handler,
            },
            catch_type,
        });
    }
    let mut labels = vec![false; insns.len() + 1];
    let mut mark = |pc: usize| {
        if let Some(at) = index_of(pc) {
            labels[at] = true;
        }
    };
    for &(pc, _) in &code.lines {
        mark(usize::from(pc));
    }
    for local in &code.locals {
        mark(usize::from(local.start_pc));
        mark(usize::from(local.start_pc) + usize::from(local.length));
    }
    let computation = FrameComputation {
        insns: &insns,
        handlers: &handlers,
        labels: &labels,
        this_class,
        pool: &pool,
    };
    let computed = match computation.compute(access, name, descriptor) {
        Ok(computed) => computed.frames,
        Err(decline) => return declined(describe_decline(&decline, &offsets)),
    };
    let stored = match &code.stackmap {
        None => Vec::new(),
        Some(table) => match entry_frame(access, name, descriptor, this_class)
            .ok()
            .and_then(|entry| decode_table(table, &entry, &pool, &offsets))
        {
            Some(frames) => frames,
            None => return declined("stored StackMapTable does not decode".to_string()),
        },
    };
    compare(&stored, &computed, &offsets)
}

fn describe_decline(decline: &Decline, offsets: &[usize]) -> String {
    match decline {
        Decline::UnsupportedControlFlow => "unsupported control flow".to_string(),
        Decline::Unsteppable(index) => format!(
            "the instruction at {} underflows the stack or cannot be modelled",
            offsets[*index]
        ),
        Decline::StackHeight(index) => format!("stack heights disagree at {}", offsets[*index]),
        Decline::Descriptor => "malformed descriptor".to_string(),
    }
}

/// A stored frame at an instruction index.
struct StoredFrame {
    index: usize,
    locals: Vec<VerificationType>,
    stack: Vec<VerificationType>,
}

/// Decode a `StackMapTable` body into absolute frames (locals in frame form).
fn decode_table(
    table: &[u8],
    entry: &[VerificationType],
    pool: &ClassPool,
    offsets: &[usize],
) -> Option<Vec<StoredFrame>> {
    let mut at = 0usize;
    let u1 = |at: &mut usize| -> Option<u8> {
        let value = *table.get(*at)?;
        *at += 1;
        Some(value)
    };
    let u2 = |at: &mut usize| -> Option<u16> {
        let value = u16::from_be_bytes([*table.get(*at)?, *table.get(*at + 1)?]);
        *at += 2;
        Some(value)
    };
    let vtype = |at: &mut usize| -> Option<VerificationType> {
        Some(match u1(at)? {
            0 => VerificationType::Top,
            1 => VerificationType::Integer,
            2 => VerificationType::Float,
            3 => VerificationType::Double,
            4 => VerificationType::Long,
            5 => VerificationType::Null,
            6 => VerificationType::UninitializedThis,
            7 => VerificationType::Reference(pool.class_name(u2(at)?)?.into()),
            8 => {
                let pc = usize::from(u2(at)?);
                VerificationType::Uninitialized(offsets.binary_search(&pc).ok()?)
            }
            _ => return None,
        })
    };
    let count = u2(&mut at)?;
    let mut locals: Vec<VerificationType> = entry.to_vec();
    let mut offset: i64 = -1;
    let mut frames = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let tag = u1(&mut at)?;
        let (delta, stack) = match tag {
            0..=63 => (u16::from(tag), Vec::new()),
            64..=127 => (u16::from(tag - 64), vec![vtype(&mut at)?]),
            247 => {
                let delta = u2(&mut at)?;
                (delta, vec![vtype(&mut at)?])
            }
            248..=250 => {
                let delta = u2(&mut at)?;
                for _ in 0..(251 - tag) {
                    locals.pop()?;
                }
                (delta, Vec::new())
            }
            251 => (u2(&mut at)?, Vec::new()),
            252..=254 => {
                let delta = u2(&mut at)?;
                for _ in 0..(tag - 251) {
                    locals.push(vtype(&mut at)?);
                }
                (delta, Vec::new())
            }
            255 => {
                let delta = u2(&mut at)?;
                let n = u2(&mut at)?;
                locals = (0..n).map(|_| vtype(&mut at)).collect::<Option<_>>()?;
                let m = u2(&mut at)?;
                let stack = (0..m).map(|_| vtype(&mut at)).collect::<Option<_>>()?;
                (delta, stack)
            }
            _ => return None,
        };
        offset = if offset < 0 {
            i64::from(delta)
        } else {
            offset + i64::from(delta) + 1
        };
        let index = offsets.binary_search(&usize::try_from(offset).ok()?).ok()?;
        frames.push(StoredFrame {
            index,
            locals: locals.clone(),
            stack,
        });
    }
    Some(frames)
}

fn compare(stored: &[StoredFrame], computed: &[ComputedFrame], offsets: &[usize]) -> Outcome {
    let render = |values: &[VerificationType]| -> String {
        let parts: Vec<String> = values.iter().map(render_type).collect();
        format!("[{}]", parts.join(", "))
    };
    let unreachable = |frame: &ComputedFrame| {
        frame.locals.is_empty()
            && frame.stack == [VerificationType::Reference("java/lang/Throwable".into())]
    };
    let mut s = stored.iter().peekable();
    let mut c = computed.iter().peekable();
    loop {
        let (kind, first) = match (s.peek(), c.peek()) {
            (None, None) => {
                return Outcome::Identical {
                    frames: computed.len(),
                }
            }
            (Some(stored), Some(computed)) if stored.index == computed.index => {
                if stored.locals == computed.locals && stored.stack == computed.stack {
                    s.next();
                    c.next();
                    continue;
                }
                let kind = if unreachable(computed) {
                    Difference::UnreachableCode
                } else if stored.stack != computed.stack {
                    Difference::Stack
                } else if local_types_agree(&stored.locals, &computed.locals) {
                    Difference::LocalDropped
                } else {
                    Difference::LocalType
                };
                (
                    kind,
                    format!(
                        "at {}: stored locals {} stack {}, computed locals {} stack {}",
                        offsets[stored.index],
                        render(&stored.locals),
                        render(&stored.stack),
                        render(&computed.locals),
                        render(&computed.stack),
                    ),
                )
            }
            (Some(stored), computed) if computed.is_none_or(|c| stored.index < c.index) => (
                Difference::FrameWithoutJump,
                format!(
                    "at {}: a stored frame where none is computed (locals {} stack {})",
                    offsets[stored.index],
                    render(&stored.locals),
                    render(&stored.stack),
                ),
            ),
            (_, Some(computed)) => (
                if unreachable(computed) {
                    Difference::UnreachableCode
                } else {
                    Difference::MissingFrame
                },
                format!(
                    "at {}: a computed frame where none is stored (locals {} stack {})",
                    offsets[computed.index],
                    render(&computed.locals),
                    render(&computed.stack),
                ),
            ),
            (Some(_), None) => unreachable!("handled by the stored-only arm"),
        };
        return Outcome::Different { kind, first };
    }
}

/// Whether every local both frames type (neither `top`, nor missing) has the same type: the frames
/// then differ only in locals one side drops.
fn local_types_agree(stored: &[VerificationType], computed: &[VerificationType]) -> bool {
    stored
        .iter()
        .zip(computed)
        .all(|(a, b)| a == b || *a == VerificationType::Top || *b == VerificationType::Top)
        && computed.len() >= stored.len()
}

fn render_type(value: &VerificationType) -> String {
    match value {
        VerificationType::Top => "top".to_string(),
        VerificationType::Integer => "int".to_string(),
        VerificationType::Float => "float".to_string(),
        VerificationType::Long => "long".to_string(),
        VerificationType::Double => "double".to_string(),
        VerificationType::Null => "null".to_string(),
        VerificationType::UninitializedThis => "uninitializedThis".to_string(),
        VerificationType::Uninitialized(index) => format!("uninitialized@{index}"),
        VerificationType::Reference(name) => name.to_string(),
    }
}

/// A parsed class-file constant pool, as the frame computation reads it.
struct ClassPool<'a>(&'a [C]);

impl ClassPool<'_> {
    fn utf8(&self, index: u16) -> Option<&str> {
        match self.0.get(usize::from(index))? {
            C::Utf8(text) => Some(text),
            _ => None,
        }
    }

    fn name_and_type_descriptor(&self, index: u16) -> Option<&str> {
        match self.0.get(usize::from(index))? {
            C::NameAndType(_, descriptor) => self.utf8(*descriptor),
            _ => None,
        }
    }
}

impl PoolView for ClassPool<'_> {
    fn class_name(&self, index: u16) -> Option<&str> {
        match self.0.get(usize::from(index))? {
            C::Class(name) => self.utf8(*name),
            _ => None,
        }
    }

    fn field_descriptor(&self, index: u16) -> Option<&str> {
        match self.0.get(usize::from(index))? {
            C::Fieldref(_, nat) => self.name_and_type_descriptor(*nat),
            _ => None,
        }
    }

    fn method_descriptor(&self, index: u16) -> Option<&str> {
        match self.0.get(usize::from(index))? {
            C::Methodref(_, nat) | C::InterfaceMethodref(_, nat) | C::InvokeDynamic(_, nat) => {
                self.name_and_type_descriptor(*nat)
            }
            _ => None,
        }
    }

    fn loadable_constant(&self, index: u16) -> Option<VerifType> {
        let name = |name: &str| VerifType::ObjectName(name.to_string());
        Some(match self.0.get(usize::from(index))? {
            C::Integer(_) => VerifType::Integer,
            C::Float(_) => VerifType::Float,
            C::Long(_) => VerifType::Long,
            C::Double(_) => VerifType::Double,
            C::String(_) => name("java/lang/String"),
            C::Class(_) => name("java/lang/Class"),
            C::MethodType(_) => name("java/lang/invoke/MethodType"),
            C::MethodHandle(..) => name("java/lang/invoke/MethodHandle"),
            _ => return None,
        })
    }
}

/// One class's audit, or why the class could not be parsed.
pub type ClassAudit = Result<Vec<MethodAudit>, String>;

/// Audit every class in a jar, as `(entry name, audit)` in entry order.
pub fn audit_jar(path: &std::path::Path) -> Result<Vec<(String, ClassAudit)>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut out = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = entry.name().to_string();
        // Multi-release variants are compiled separately; the base entry is the one audited.
        if !name.ends_with(".class") || name.starts_with("META-INF/") {
            continue;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        out.push((name, audit_class(&bytes)));
    }
    Ok(out)
}

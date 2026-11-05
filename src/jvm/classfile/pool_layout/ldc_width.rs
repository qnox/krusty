//! `ldc` instructions sized for their renumbered entries.
//!
//! ASM's writer picks `ldc` for an entry whose index fits in one byte and `ldc_w` for any other, so
//! once the pool is laid out again an `ldc` whose entry moved past index 255 widens and an `ldc_w`
//! whose entry came to fit narrows. Either changes the length of its method's code, so every
//! offset after it moves: branch and switch targets, the exception table, the line and
//! local-variable tables and the stack map frames are carried to the new offsets. A `Code`
//! attribute holding anything else that names an offset is not resized.

use crate::jvm::bytecode::instruction_len;

const LDC: u8 = 0x12;
const LDC_W: u8 = 0x13;

/// A load whose instruction changes width: the offset of its operand in the class file, and the
/// entry it loads.
#[derive(Clone, Copy, Debug)]
pub(super) struct Load {
    pub(super) at: usize,
    pub(super) index: u16,
}

/// Why a class's loads were not resized.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unresized {
    Malformed,
    /// A `Code` attribute holding an attribute whose offsets are not carried.
    UnknownCodeAttribute(String),
    /// A branch, or the code itself, that no longer fits its encoding.
    OutOfRange,
}

/// `class` with each of `loads` (sorted by offset) rewritten to the width its entry needs.
pub(super) fn resized(class: &[u8], pool_end: usize, loads: &[Load]) -> Result<Vec<u8>, Unresized> {
    let names = utf8_names(class, pool_end)?;
    let mut cursor = Cursor {
        bytes: class,
        at: pool_end,
    };
    let mut out = Vec::with_capacity(class.len() + loads.len());
    cursor.skip(6)?;
    let interfaces = cursor.u2()?;
    cursor.skip(usize::from(interfaces) * 2)?;
    let fields = cursor.u2()?;
    for _ in 0..fields {
        cursor.member()?;
    }
    let methods = cursor.u2()?;
    let mut copied = 0;
    for _ in 0..methods {
        cursor.skip(6)?;
        for _ in 0..cursor.u2()? {
            let name = cursor.u2()?;
            let length_at = cursor.at;
            let length = cursor.u4()? as usize;
            let body = cursor.at..cursor.at.checked_add(length).ok_or(Unresized::Malformed)?;
            cursor.skip(length)?;
            let inside: Vec<Load> = loads
                .iter()
                .copied()
                .filter(|load| body.contains(&load.at))
                .collect();
            if inside.is_empty() {
                continue;
            }
            if names.get(&name).map(String::as_str) != Some("Code") {
                return Err(Unresized::Malformed);
            }
            let code = resized_code(&class[body.clone()], body.start, &inside, &names)?;
            out.extend_from_slice(&class[copied..length_at]);
            let length = u32::try_from(code.len()).map_err(|_| Unresized::OutOfRange)?;
            out.extend_from_slice(&length.to_be_bytes());
            out.extend_from_slice(&code);
            copied = body.end;
        }
    }
    out.extend_from_slice(&class[copied..]);
    Ok(out)
}

/// The `Utf8` entries of the pool that ends at `pool_end`, by index.
fn utf8_names(
    class: &[u8],
    pool_end: usize,
) -> Result<crate::fx_hash::HashMap<u16, String>, Unresized> {
    let mut names = crate::fx_hash::HashMap::default();
    let mut cursor = Cursor {
        bytes: class,
        at: 8,
    };
    let count = cursor.u2()?;
    let mut index = 1u16;
    while index < count {
        let tag = cursor.u1()?;
        let mut width = 1;
        match tag {
            1 => {
                let len = usize::from(cursor.u2()?);
                let start = cursor.at;
                cursor.skip(len)?;
                if let Ok(name) = std::str::from_utf8(&class[start..cursor.at]) {
                    names.insert(index, name.to_string());
                }
            }
            7 | 8 | 16 | 19 | 20 => cursor.skip(2)?,
            15 => cursor.skip(3)?,
            3 | 4 | 9..=12 | 17 | 18 => cursor.skip(4)?,
            5 | 6 => {
                cursor.skip(8)?;
                width = 2;
            }
            _ => return Err(Unresized::Malformed),
        }
        index = index.checked_add(width).ok_or(Unresized::Malformed)?;
    }
    if cursor.at != pool_end {
        return Err(Unresized::Malformed);
    }
    Ok(names)
}

/// A `Code` attribute's body with `loads` (offsets in the class; `start` is the body's) resized.
fn resized_code(
    body: &[u8],
    start: usize,
    loads: &[Load],
    names: &crate::fx_hash::HashMap<u16, String>,
) -> Result<Vec<u8>, Unresized> {
    let mut cursor = Cursor { bytes: body, at: 4 };
    let code_len = cursor.u4()? as usize;
    let code_start = cursor.at;
    cursor.skip(code_len)?;
    let code = &body[code_start..cursor.at];
    let loads: crate::fx_hash::HashMap<usize, u16> = loads
        .iter()
        .map(|load| (load.at - start - code_start - 1, load.index))
        .collect();
    let moved = Offsets::of(code, &loads)?;
    let mut out = body[..4].to_vec();
    let new_len = u32::try_from(moved.end()).map_err(|_| Unresized::OutOfRange)?;
    if new_len > u32::from(u16::MAX) {
        return Err(Unresized::OutOfRange);
    }
    out.extend_from_slice(&new_len.to_be_bytes());
    out.extend_from_slice(&moved.code(code, &loads)?);
    let handlers = cursor.u2()?;
    out.extend_from_slice(&handlers.to_be_bytes());
    for _ in 0..handlers {
        for _ in 0..3 {
            let pc = moved.at(usize::from(cursor.u2()?))?;
            out.extend_from_slice(&pc.to_be_bytes());
        }
        out.extend_from_slice(cursor.take(2)?);
    }
    let attributes = cursor.u2()?;
    out.extend_from_slice(&attributes.to_be_bytes());
    for _ in 0..attributes {
        let name = cursor.u2()?;
        let length = cursor.u4()? as usize;
        let attribute = cursor.take(length)?;
        let attribute = match names.get(&name).map(String::as_str) {
            Some("LineNumberTable") => moved.line_numbers(attribute)?,
            Some("LocalVariableTable" | "LocalVariableTypeTable") => moved.locals(attribute)?,
            Some("StackMapTable") => moved.frames(attribute)?,
            other => {
                return Err(Unresized::UnknownCodeAttribute(
                    other.unwrap_or_default().to_string(),
                ))
            }
        };
        out.extend_from_slice(&name.to_be_bytes());
        let length = u32::try_from(attribute.len()).map_err(|_| Unresized::OutOfRange)?;
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&attribute);
    }
    if cursor.at != body.len() {
        return Err(Unresized::Malformed);
    }
    Ok(out)
}

/// Where each instruction of a method's code lands once its loads are resized.
struct Offsets {
    /// The new offset of each instruction start, and of the end of the code; `None` elsewhere.
    moved: Vec<Option<u16>>,
}

impl Offsets {
    fn of(code: &[u8], loads: &crate::fx_hash::HashMap<usize, u16>) -> Result<Self, Unresized> {
        let mut moved = vec![None; code.len() + 1];
        let mut pc = 0;
        let mut at = 0usize;
        while pc < code.len() {
            moved[pc] = Some(u16::try_from(at).map_err(|_| Unresized::OutOfRange)?);
            let len = instruction_len(code, pc).ok_or(Unresized::Malformed)?;
            at += match code[pc] {
                LDC | LDC_W => loads
                    .get(&pc)
                    .map_or(len, |&index| if index > 0xff { 3 } else { 2 }),
                0xaa | 0xab => {
                    let operands = len - 1 - padding(pc);
                    1 + padding(at) + operands
                }
                _ => len,
            };
            pc += len;
        }
        moved[code.len()] = Some(u16::try_from(at).map_err(|_| Unresized::OutOfRange)?);
        Ok(Offsets { moved })
    }

    fn end(&self) -> usize {
        usize::from(self.moved.last().copied().flatten().unwrap_or(0))
    }

    /// The new offset of the instruction that started at `pc`.
    fn at(&self, pc: usize) -> Result<u16, Unresized> {
        self.moved
            .get(pc)
            .copied()
            .flatten()
            .ok_or(Unresized::Malformed)
    }

    /// The new distance from the instruction at `pc` to the one at `pc + offset`.
    fn jump(&self, pc: usize, offset: i64) -> Result<i64, Unresized> {
        let target = usize::try_from(pc as i64 + offset).map_err(|_| Unresized::Malformed)?;
        Ok(i64::from(self.at(target)?) - i64::from(self.at(pc)?))
    }

    fn code(
        &self,
        code: &[u8],
        loads: &crate::fx_hash::HashMap<usize, u16>,
    ) -> Result<Vec<u8>, Unresized> {
        let mut out = Vec::with_capacity(self.end());
        let mut pc = 0;
        while pc < code.len() {
            let len = instruction_len(code, pc).ok_or(Unresized::Malformed)?;
            let op = code[pc];
            match op {
                LDC | LDC_W if loads.contains_key(&pc) => {
                    let index = loads[&pc];
                    match u8::try_from(index) {
                        Ok(narrow) => out.extend_from_slice(&[LDC, narrow]),
                        Err(_) => {
                            out.push(LDC_W);
                            out.extend_from_slice(&index.to_be_bytes());
                        }
                    }
                }
                0x99..=0xa8 | 0xc6 | 0xc7 => {
                    let offset = i16::from_be_bytes([code[pc + 1], code[pc + 2]]);
                    let offset = i16::try_from(self.jump(pc, i64::from(offset))?)
                        .map_err(|_| Unresized::OutOfRange)?;
                    out.push(op);
                    out.extend_from_slice(&offset.to_be_bytes());
                }
                0xc8 | 0xc9 => {
                    out.push(op);
                    out.extend_from_slice(&self.wide_jump(code, pc, pc + 1)?);
                }
                0xaa | 0xab => {
                    out.push(op);
                    out.resize(out.len() + padding(out.len() - 1), 0);
                    let operands = pc + 1 + padding(pc);
                    out.extend_from_slice(&self.wide_jump(code, pc, operands)?);
                    if op == 0xaa {
                        out.extend_from_slice(&code[operands + 4..operands + 12]);
                        let mut at = operands + 12;
                        while at < pc + len {
                            out.extend_from_slice(&self.wide_jump(code, pc, at)?);
                            at += 4;
                        }
                    } else {
                        out.extend_from_slice(&code[operands + 4..operands + 8]);
                        let mut at = operands + 8;
                        while at < pc + len {
                            out.extend_from_slice(&code[at..at + 4]);
                            out.extend_from_slice(&self.wide_jump(code, pc, at + 4)?);
                            at += 8;
                        }
                    }
                }
                _ => out.extend_from_slice(&code[pc..pc + len]),
            }
            pc += len;
        }
        Ok(out)
    }

    /// The four-byte jump at `at`, of the instruction at `pc`, carried to the new offsets.
    fn wide_jump(&self, code: &[u8], pc: usize, at: usize) -> Result<[u8; 4], Unresized> {
        let bytes = code.get(at..at + 4).ok_or(Unresized::Malformed)?;
        let offset = i32::from_be_bytes(bytes.try_into().expect("four bytes"));
        let offset =
            i32::try_from(self.jump(pc, i64::from(offset))?).map_err(|_| Unresized::OutOfRange)?;
        Ok(offset.to_be_bytes())
    }

    fn line_numbers(&self, attribute: &[u8]) -> Result<Vec<u8>, Unresized> {
        let mut cursor = Cursor {
            bytes: attribute,
            at: 0,
        };
        let count = cursor.u2()?;
        let mut out = count.to_be_bytes().to_vec();
        for _ in 0..count {
            let pc = self.at(usize::from(cursor.u2()?))?;
            out.extend_from_slice(&pc.to_be_bytes());
            out.extend_from_slice(cursor.take(2)?);
        }
        Ok(out)
    }

    fn locals(&self, attribute: &[u8]) -> Result<Vec<u8>, Unresized> {
        let mut cursor = Cursor {
            bytes: attribute,
            at: 0,
        };
        let count = cursor.u2()?;
        let mut out = count.to_be_bytes().to_vec();
        for _ in 0..count {
            let start = usize::from(cursor.u2()?);
            let end = start + usize::from(cursor.u2()?);
            let (start, end) = (self.at(start)?, self.at(end)?);
            out.extend_from_slice(&start.to_be_bytes());
            out.extend_from_slice(&(end - start).to_be_bytes());
            out.extend_from_slice(cursor.take(6)?);
        }
        Ok(out)
    }

    /// The stack map with each frame at its instruction's new offset, in the compressed form ASM
    /// picks for the new distance from the frame before.
    fn frames(&self, attribute: &[u8]) -> Result<Vec<u8>, Unresized> {
        let mut cursor = Cursor {
            bytes: attribute,
            at: 0,
        };
        let count = cursor.u2()?;
        let mut out = count.to_be_bytes().to_vec();
        let mut previous: Option<(usize, u16)> = None;
        for _ in 0..count {
            let kind = cursor.u1()?;
            let delta = match kind {
                0..=63 => usize::from(kind),
                64..=127 => usize::from(kind - 64),
                247..=255 => usize::from(cursor.u2()?),
                _ => return Err(Unresized::Malformed),
            };
            let pc = previous.map_or(delta, |(pc, _)| pc + delta + 1);
            let at = self.at(pc)?;
            let new_delta = match previous {
                None => at,
                Some((_, before)) => at.checked_sub(before + 1).ok_or(Unresized::Malformed)?,
            };
            previous = Some((pc, at));
            match kind {
                0..=63 | 251 => match u8::try_from(new_delta) {
                    Ok(delta) if delta < 64 => out.push(delta),
                    _ => {
                        out.push(251);
                        out.extend_from_slice(&new_delta.to_be_bytes());
                    }
                },
                64..=127 | 247 => {
                    match u8::try_from(new_delta) {
                        Ok(delta) if delta < 64 => out.push(64 + delta),
                        _ => {
                            out.push(247);
                            out.extend_from_slice(&new_delta.to_be_bytes());
                        }
                    }
                    self.verification_types(&mut cursor, 1, &mut out)?;
                }
                248..=250 => {
                    out.push(kind);
                    out.extend_from_slice(&new_delta.to_be_bytes());
                }
                252..=254 => {
                    out.push(kind);
                    out.extend_from_slice(&new_delta.to_be_bytes());
                    self.verification_types(&mut cursor, usize::from(kind - 251), &mut out)?;
                }
                255 => {
                    out.push(kind);
                    out.extend_from_slice(&new_delta.to_be_bytes());
                    for _ in 0..2 {
                        let types = cursor.u2()?;
                        out.extend_from_slice(&types.to_be_bytes());
                        self.verification_types(&mut cursor, usize::from(types), &mut out)?;
                    }
                }
                _ => return Err(Unresized::Malformed),
            }
        }
        Ok(out)
    }

    /// Copy `count` verification types, carrying an uninitialized type's `new` to its offset.
    fn verification_types(
        &self,
        cursor: &mut Cursor,
        count: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), Unresized> {
        for _ in 0..count {
            let tag = cursor.u1()?;
            out.push(tag);
            match tag {
                7 => out.extend_from_slice(cursor.take(2)?),
                8 => {
                    let pc = self.at(usize::from(cursor.u2()?))?;
                    out.extend_from_slice(&pc.to_be_bytes());
                }
                0..=6 => {}
                _ => return Err(Unresized::Malformed),
            }
        }
        Ok(())
    }
}

/// The padding after a switch opcode at `pc` that aligns its operands to four bytes.
fn padding(pc: usize) -> usize {
    (4 - (pc + 1) % 4) % 4
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], Unresized> {
        let end = self.at.checked_add(len).ok_or(Unresized::Malformed)?;
        let bytes = self.bytes.get(self.at..end).ok_or(Unresized::Malformed)?;
        self.at = end;
        Ok(bytes)
    }

    fn skip(&mut self, len: usize) -> Result<(), Unresized> {
        self.take(len).map(|_| ())
    }

    fn u1(&mut self) -> Result<u8, Unresized> {
        Ok(self.take(1)?[0])
    }

    fn u2(&mut self) -> Result<u16, Unresized> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u4(&mut self) -> Result<u32, Unresized> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes(bytes.try_into().expect("four bytes")))
    }

    /// A field or method whose attributes are left as they are.
    fn member(&mut self) -> Result<(), Unresized> {
        self.skip(6)?;
        for _ in 0..self.u2()? {
            self.skip(2)?;
            let length = self.u4()? as usize;
            self.skip(length)?;
        }
        Ok(())
    }
}

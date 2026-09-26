//! Every constant-pool index a serialized class file holds, and what holds it.
//!
//! A class is read once, front to back: its pool entries with the indices each one names, then
//! every index slot of the rest of the class, attributed either to a method's `Code` (by the part
//! of it ASM interns in turn) or to the class itself. A class holding an attribute this reader does
//! not know is not read, since an index inside it would go unseen.

use crate::jvm::bytecode::instruction_len;

/// A pool entry: where its bytes are, and the entries it names, in the order ASM interns them.
pub(super) struct Entry {
    pub(super) range: std::ops::Range<usize>,
    /// Each named entry, with its offset from the entry's first byte.
    pub(super) components: Vec<(usize, u16)>,
    /// A `long` or `double`, which takes two indices.
    pub(super) wide: bool,
}

/// The part of a method's `Code` holding an index, in the order ASM interns them: the exception
/// table's catch types, the instructions' operands, the local-variable tables, then the frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    Catch,
    Operand,
    Local,
    Frame,
}

/// What holds an index slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Holder {
    /// The class, a field, a method's header or attributes, or an attribute's name.
    Class,
    /// The `Code` of the method at this position in the class's method table.
    Code(usize, Part),
}

/// An index slot outside the pool.
pub(super) struct Slot {
    /// Offset of the slot in the class file.
    pub(super) at: usize,
    /// A one-byte `ldc` operand, rather than a two-byte index.
    pub(super) narrow: bool,
    pub(super) index: u16,
    pub(super) holder: Holder,
}

/// A class file's pool and index slots.
pub(super) struct ClassSlots {
    /// The pool by index; `None` at index 0 and at the second index of a wide entry.
    pub(super) entries: Vec<Option<Entry>>,
    /// Offset of the byte after the pool.
    pub(super) pool_end: usize,
    pub(super) slots: Vec<Slot>,
}

/// Why a class was not read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unread {
    Truncated,
    UnknownPoolTag(u8),
    UnknownAttribute(String),
    /// An attribute whose contents do not fill its declared length.
    AttributeLength(String),
    UnknownInstruction(usize),
    UnknownElementTag(u8),
    UnknownTarget(u8),
    UnknownFrame(u8),
}

/// Where an attribute sits, which decides what it may be and who holds its indices.
#[derive(Clone, Copy)]
enum Scope {
    Class,
    Member,
    Method(usize),
    Code(usize),
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    entries: Vec<Option<Entry>>,
    slots: Vec<Slot>,
}

pub(super) fn read(bytes: &[u8]) -> Result<ClassSlots, Unread> {
    let mut reader = Reader {
        bytes,
        at: 8,
        entries: Vec::new(),
        slots: Vec::new(),
    };
    reader.pool()?;
    let pool_end = reader.at;
    reader.class()?;
    if reader.at != bytes.len() {
        return Err(Unread::Truncated);
    }
    Ok(ClassSlots {
        entries: reader.entries,
        pool_end,
        slots: reader.slots,
    })
}

impl Reader<'_> {
    fn u1(&mut self) -> Result<u8, Unread> {
        let value = *self.bytes.get(self.at).ok_or(Unread::Truncated)?;
        self.at += 1;
        Ok(value)
    }

    fn u2(&mut self) -> Result<u16, Unread> {
        Ok(u16::from_be_bytes([self.u1()?, self.u1()?]))
    }

    fn u4(&mut self) -> Result<u32, Unread> {
        Ok(u32::from(self.u2()?) << 16 | u32::from(self.u2()?))
    }

    fn skip(&mut self, len: usize) -> Result<(), Unread> {
        let end = self.at.checked_add(len).ok_or(Unread::Truncated)?;
        if end > self.bytes.len() {
            return Err(Unread::Truncated);
        }
        self.at = end;
        Ok(())
    }

    /// A two-byte index held by `holder`; `0` (no entry) is not a slot.
    fn index(&mut self, holder: Holder) -> Result<u16, Unread> {
        let at = self.at;
        let index = self.u2()?;
        if index != 0 {
            self.slots.push(Slot {
                at,
                narrow: false,
                index,
                holder,
            });
        }
        Ok(index)
    }

    fn indices(&mut self, holder: Holder) -> Result<(), Unread> {
        for _ in 0..self.u2()? {
            self.index(holder)?;
        }
        Ok(())
    }

    fn pool(&mut self) -> Result<(), Unread> {
        let count = self.u2()?;
        self.entries.push(None);
        while self.entries.len() < usize::from(count) {
            let start = self.at;
            let tag = self.u1()?;
            let (components, wide): (&[usize], bool) = match tag {
                1 => {
                    let len = self.u2()?;
                    self.skip(usize::from(len))?;
                    (&[], false)
                }
                3 | 4 => (&[], false),
                5 | 6 => (&[], true),
                7 | 8 | 16 | 19 | 20 => (&[1], false),
                9..=12 => (&[1, 3], false),
                15 => (&[2], false),
                17 | 18 => (&[3], false),
                _ => return Err(Unread::UnknownPoolTag(tag)),
            };
            let len = match tag {
                1 => self.at - start,
                3 | 4 | 9..=12 | 17 | 18 => 5,
                5 | 6 => 9,
                7 | 8 | 16 | 19 | 20 => 3,
                _ => 4,
            };
            self.at = start;
            self.skip(len)?;
            let components = components
                .iter()
                .map(|&offset| {
                    let at = start + offset;
                    (
                        offset,
                        u16::from_be_bytes([self.bytes[at], self.bytes[at + 1]]),
                    )
                })
                .collect();
            self.entries.push(Some(Entry {
                range: start..self.at,
                components,
                wide,
            }));
            if wide {
                self.entries.push(None);
            }
        }
        Ok(())
    }

    fn utf8(&self, index: u16) -> Option<&str> {
        let entry = self.entries.get(usize::from(index))?.as_ref()?;
        let bytes = self.bytes.get(entry.range.clone())?;
        (bytes.first() == Some(&1))
            .then(|| std::str::from_utf8(&bytes[3..]).ok())
            .flatten()
    }

    fn class(&mut self) -> Result<(), Unread> {
        self.skip(2)?;
        self.index(Holder::Class)?;
        self.index(Holder::Class)?;
        self.indices(Holder::Class)?;
        for _ in 0..self.u2()? {
            self.member(Scope::Member)?;
        }
        for method in 0..usize::from(self.u2()?) {
            self.member(Scope::Method(method))?;
        }
        self.attributes(Scope::Class)
    }

    fn member(&mut self, scope: Scope) -> Result<(), Unread> {
        self.skip(2)?;
        self.index(Holder::Class)?;
        self.index(Holder::Class)?;
        self.attributes(scope)
    }

    fn attributes(&mut self, scope: Scope) -> Result<(), Unread> {
        for _ in 0..self.u2()? {
            let name_index = self.index(Holder::Class)?;
            let len = self.u4()? as usize;
            let end = self.at.checked_add(len).ok_or(Unread::Truncated)?;
            let name = self.utf8(name_index).unwrap_or_default().to_string();
            self.attribute(&name, scope, len)?;
            if self.at != end {
                return Err(Unread::AttributeLength(name));
            }
        }
        Ok(())
    }

    fn attribute(&mut self, name: &str, scope: Scope, len: usize) -> Result<(), Unread> {
        let class = Holder::Class;
        match (name, scope) {
            ("Code", Scope::Method(method)) => self.code(method)?,
            ("StackMapTable", Scope::Code(method)) => self.frames(method)?,
            ("LocalVariableTable" | "LocalVariableTypeTable", Scope::Code(method)) => {
                for _ in 0..self.u2()? {
                    self.skip(4)?;
                    self.index(Holder::Code(method, Part::Local))?;
                    self.index(Holder::Code(method, Part::Local))?;
                    self.skip(2)?;
                }
            }
            ("LineNumberTable", Scope::Code(_))
            | ("SourceDebugExtension", Scope::Class)
            | ("Deprecated" | "Synthetic", _) => self.skip(len)?,
            ("ConstantValue" | "Signature", Scope::Member | Scope::Method(_))
            | ("Signature" | "SourceFile" | "NestHost", Scope::Class) => {
                self.index(class)?;
            }
            ("Exceptions", Scope::Method(_))
            | ("NestMembers" | "PermittedSubclasses", Scope::Class) => self.indices(class)?,
            ("InnerClasses", Scope::Class) => {
                for _ in 0..self.u2()? {
                    self.index(class)?;
                    self.index(class)?;
                    self.index(class)?;
                    self.skip(2)?;
                }
            }
            ("EnclosingMethod", Scope::Class) => {
                self.index(class)?;
                self.index(class)?;
            }
            ("BootstrapMethods", Scope::Class) => {
                for _ in 0..self.u2()? {
                    self.index(class)?;
                    self.indices(class)?;
                }
            }
            ("MethodParameters", Scope::Method(_)) => {
                for _ in 0..self.u1()? {
                    self.index(class)?;
                    self.skip(2)?;
                }
            }
            ("RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations", _) => {
                self.annotations()?;
            }
            (
                "RuntimeVisibleParameterAnnotations" | "RuntimeInvisibleParameterAnnotations",
                Scope::Method(_),
            ) => {
                for _ in 0..self.u1()? {
                    self.annotations()?;
                }
            }
            ("AnnotationDefault", Scope::Method(_)) => self.element()?,
            ("RuntimeVisibleTypeAnnotations" | "RuntimeInvisibleTypeAnnotations", _) => {
                for _ in 0..self.u2()? {
                    self.type_annotation()?;
                }
            }
            ("Record", Scope::Class) => {
                for _ in 0..self.u2()? {
                    self.index(class)?;
                    self.index(class)?;
                    self.attributes(Scope::Member)?;
                }
            }
            _ => return Err(Unread::UnknownAttribute(name.to_string())),
        }
        Ok(())
    }

    fn code(&mut self, method: usize) -> Result<(), Unread> {
        self.skip(4)?;
        let len = self.u4()? as usize;
        let start = self.at;
        self.skip(len)?;
        let code = &self.bytes[start..self.at];
        let mut pc = 0;
        let mut operands = Vec::new();
        while pc < code.len() {
            let op = code[pc];
            match op {
                0x12 => operands.push((pc + 1, true)),
                0x13 | 0x14 | 0xb2..=0xbb | 0xbd | 0xc0 | 0xc1 | 0xc5 => {
                    operands.push((pc + 1, false));
                }
                _ => {}
            }
            pc += instruction_len(code, pc).ok_or(Unread::UnknownInstruction(pc))?;
        }
        for (offset, narrow) in operands {
            let at = start + offset;
            let index = if narrow {
                u16::from(code[offset])
            } else {
                u16::from_be_bytes([code[offset], code[offset + 1]])
            };
            self.slots.push(Slot {
                at,
                narrow,
                index,
                holder: Holder::Code(method, Part::Operand),
            });
        }
        for _ in 0..self.u2()? {
            self.skip(6)?;
            self.index(Holder::Code(method, Part::Catch))?;
        }
        self.attributes(Scope::Code(method))
    }

    fn frames(&mut self, method: usize) -> Result<(), Unread> {
        for _ in 0..self.u2()? {
            let kind = self.u1()?;
            let (skip, types) = match kind {
                0..=63 => (0, 0),
                64..=127 => (0, 1),
                247 => (2, 1),
                248..=251 => (2, 0),
                252..=254 => (2, usize::from(kind - 251)),
                255 => {
                    self.skip(2)?;
                    let locals = self.u2()?;
                    self.verification_types(method, usize::from(locals))?;
                    let stack = self.u2()?;
                    (0, usize::from(stack))
                }
                _ => return Err(Unread::UnknownFrame(kind)),
            };
            self.skip(skip)?;
            self.verification_types(method, types)?;
        }
        Ok(())
    }

    fn verification_types(&mut self, method: usize, count: usize) -> Result<(), Unread> {
        for _ in 0..count {
            match self.u1()? {
                7 => {
                    self.index(Holder::Code(method, Part::Frame))?;
                }
                8 => self.skip(2)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn annotations(&mut self) -> Result<(), Unread> {
        for _ in 0..self.u2()? {
            self.annotation()?;
        }
        Ok(())
    }

    fn annotation(&mut self) -> Result<(), Unread> {
        self.index(Holder::Class)?;
        for _ in 0..self.u2()? {
            self.index(Holder::Class)?;
            self.element()?;
        }
        Ok(())
    }

    fn element(&mut self) -> Result<(), Unread> {
        match self.u1()? {
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' | b'c' => {
                self.index(Holder::Class)?;
            }
            b'e' => {
                self.index(Holder::Class)?;
                self.index(Holder::Class)?;
            }
            b'@' => self.annotation()?,
            b'[' => {
                for _ in 0..self.u2()? {
                    self.element()?;
                }
            }
            tag => return Err(Unread::UnknownElementTag(tag)),
        }
        Ok(())
    }

    fn type_annotation(&mut self) -> Result<(), Unread> {
        let target = self.u1()?;
        match target {
            0x00 | 0x01 | 0x16 => self.skip(1)?,
            0x10 | 0x17 | 0x42..=0x46 => self.skip(2)?,
            0x11 | 0x12 => self.skip(2)?,
            0x13..=0x15 => {}
            0x40 | 0x41 => {
                let len = self.u2()?;
                self.skip(usize::from(len) * 6)?;
            }
            0x47..=0x4b => self.skip(3)?,
            _ => return Err(Unread::UnknownTarget(target)),
        }
        let path = self.u1()?;
        self.skip(usize::from(path) * 2)?;
        self.annotation()
    }
}

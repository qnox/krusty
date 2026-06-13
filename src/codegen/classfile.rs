//! A hand-written JVM class-file writer (the format is well-specified; no external crate).
//! Targets major version 52 (Java 8) to match kotlinc's default JVM target. Straight-line methods
//! need no `StackMapTable`; branch frames are added in Phase 4 (see `emit.rs`).

use std::collections::HashMap;

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_FINAL: u16 = 0x0010;
pub const ACC_SUPER: u16 = 0x0020;

const MAJOR_JAVA8: u16 = 52;

#[derive(PartialEq, Eq, Hash, Clone)]
enum Const {
    Utf8(String),
    Integer(i32),
    Long(i64),
    Double(u64), // bit pattern (f64 isn't Hash/Eq)
    Class(u16),
    String(u16),
    NameAndType(u16, u16),
    Methodref(u16, u16),
    Fieldref(u16, u16),
}

#[derive(Default)]
struct ConstPool {
    entries: Vec<Const>, // index 0 unused conceptually; we store 1-based via len()
    dedup: HashMap<Const, u16>,
}

impl ConstPool {
    /// Number of slots used (long/double take 2). Pool count in the file = this + 1.
    fn slot_count(&self) -> u16 {
        let mut n = 0u16;
        for c in &self.entries {
            n += match c {
                Const::Long(_) | Const::Double(_) => 2,
                _ => 1,
            };
        }
        n
    }

    fn intern(&mut self, c: Const) -> u16 {
        if let Some(&i) = self.dedup.get(&c) {
            return i;
        }
        let idx = self.slot_count() + 1; // 1-based
        self.entries.push(c.clone());
        self.dedup.insert(c, idx);
        idx
    }

    fn utf8(&mut self, s: &str) -> u16 {
        self.intern(Const::Utf8(s.to_string()))
    }
    fn class(&mut self, internal_name: &str) -> u16 {
        let n = self.utf8(internal_name);
        self.intern(Const::Class(n))
    }
    fn string(&mut self, s: &str) -> u16 {
        let n = self.utf8(s);
        self.intern(Const::String(n))
    }
    fn integer(&mut self, v: i32) -> u16 {
        self.intern(Const::Integer(v))
    }
    fn long(&mut self, v: i64) -> u16 {
        self.intern(Const::Long(v))
    }
    fn double(&mut self, v: f64) -> u16 {
        self.intern(Const::Double(v.to_bits()))
    }
    fn name_and_type(&mut self, name: &str, desc: &str) -> u16 {
        let n = self.utf8(name);
        let d = self.utf8(desc);
        self.intern(Const::NameAndType(n, d))
    }
    fn methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let c = self.class(class);
        let nt = self.name_and_type(name, desc);
        self.intern(Const::Methodref(c, nt))
    }
    fn fieldref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let c = self.class(class);
        let nt = self.name_and_type(name, desc);
        self.intern(Const::Fieldref(c, nt))
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        u2(out, self.slot_count() + 1);
        for c in &self.entries {
            match c {
                Const::Utf8(s) => {
                    out.push(1);
                    let b = s.as_bytes();
                    u2(out, b.len() as u16);
                    out.extend_from_slice(b);
                }
                Const::Integer(v) => {
                    out.push(3);
                    u4(out, *v as u32);
                }
                Const::Long(v) => {
                    out.push(5);
                    u4(out, (*v >> 32) as u32);
                    u4(out, *v as u32);
                }
                Const::Double(bits) => {
                    out.push(6);
                    u4(out, (*bits >> 32) as u32);
                    u4(out, *bits as u32);
                }
                Const::Class(n) => {
                    out.push(7);
                    u2(out, *n);
                }
                Const::String(n) => {
                    out.push(8);
                    u2(out, *n);
                }
                Const::Fieldref(c, nt) => {
                    out.push(9);
                    u2(out, *c);
                    u2(out, *nt);
                }
                Const::Methodref(c, nt) => {
                    out.push(10);
                    u2(out, *c);
                    u2(out, *nt);
                }
                Const::NameAndType(n, d) => {
                    out.push(12);
                    u2(out, *n);
                    u2(out, *d);
                }
            }
        }
    }
}

struct MethodInfo {
    access: u16,
    name: u16,
    desc: u16,
    max_stack: u16,
    max_locals: u16,
    code: Vec<u8>,
}

pub struct ClassWriter {
    cp: ConstPool,
    access: u16,
    this_class: u16,
    super_class: u16,
    methods: Vec<MethodInfo>,
}

impl ClassWriter {
    pub fn new(internal_name: &str, super_internal: &str) -> ClassWriter {
        let mut cp = ConstPool::default();
        let this_class = cp.class(internal_name);
        let super_class = cp.class(super_internal);
        ClassWriter {
            cp,
            access: ACC_PUBLIC | ACC_FINAL | ACC_SUPER,
            this_class,
            super_class,
            methods: Vec::new(),
        }
    }

    /// Intern helpers exposed for the emitter (Phase 4) to reference pool entries while building code.
    pub fn methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        self.cp.methodref(class, name, desc)
    }
    pub fn fieldref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        self.cp.fieldref(class, name, desc)
    }
    pub fn const_string(&mut self, s: &str) -> u16 {
        self.cp.string(s)
    }
    pub fn const_int(&mut self, v: i32) -> u16 {
        self.cp.integer(v)
    }
    pub fn const_long(&mut self, v: i64) -> u16 {
        self.cp.long(v)
    }
    pub fn const_double(&mut self, v: f64) -> u16 {
        self.cp.double(v)
    }

    pub fn add_method(&mut self, access: u16, name: &str, desc: &str, code: &CodeBuilder) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        self.methods.push(MethodInfo {
            access,
            name: n,
            desc: d,
            max_stack: code.max_stack,
            max_locals: code.max_locals,
            code: code.bytes.clone(),
        });
    }

    pub fn finish(mut self) -> Vec<u8> {
        let code_attr_name = self.cp.utf8("Code");
        let mut out = Vec::new();
        u4(&mut out, 0xCAFEBABE);
        u2(&mut out, 0); // minor
        u2(&mut out, MAJOR_JAVA8);
        self.cp.serialize(&mut out);
        u2(&mut out, self.access);
        u2(&mut out, self.this_class);
        u2(&mut out, self.super_class);
        u2(&mut out, 0); // interfaces
        u2(&mut out, 0); // fields
        u2(&mut out, self.methods.len() as u16);
        for m in &self.methods {
            u2(&mut out, m.access);
            u2(&mut out, m.name);
            u2(&mut out, m.desc);
            u2(&mut out, 1); // attributes: Code
            // Code attribute
            u2(&mut out, code_attr_name);
            let code_len = m.code.len();
            let attr_len = 2 + 2 + 4 + code_len + 2 + 2; // max_stack+max_locals+code_len+code+exc_table_len+attrs
            u4(&mut out, attr_len as u32);
            u2(&mut out, m.max_stack);
            u2(&mut out, m.max_locals);
            u4(&mut out, code_len as u32);
            out.extend_from_slice(&m.code);
            u2(&mut out, 0); // exception_table_length
            u2(&mut out, 0); // code attributes (StackMapTable added in Phase 4)
        }
        u2(&mut out, 0); // class attributes (SourceFile/@Metadata added later)
        out
    }
}

fn u2(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn u4(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

// ---- CodeBuilder: opcode emission with automatic max_stack/max_locals tracking ----------------

pub struct CodeBuilder {
    pub bytes: Vec<u8>,
    pub max_stack: u16,
    pub max_locals: u16,
    cur_stack: i32,
}

impl CodeBuilder {
    pub fn new(arg_locals: u16) -> CodeBuilder {
        CodeBuilder { bytes: Vec::new(), max_stack: 0, max_locals: arg_locals, cur_stack: 0 }
    }

    /// Ensure the local-variable table is at least `n` slots.
    pub fn ensure_locals(&mut self, n: u16) {
        if n > self.max_locals {
            self.max_locals = n;
        }
    }

    fn adjust(&mut self, delta: i32) {
        self.cur_stack += delta;
        if self.cur_stack < 0 {
            self.cur_stack = 0; // defensive; a real bug would surface in the verifier
        }
        if self.cur_stack as u16 > self.max_stack {
            self.max_stack = self.cur_stack as u16;
        }
    }

    fn op(&mut self, byte: u8, stack_delta: i32) {
        self.bytes.push(byte);
        self.adjust(stack_delta);
    }
    fn op_u1(&mut self, byte: u8, arg: u8, stack_delta: i32) {
        self.bytes.push(byte);
        self.bytes.push(arg);
        self.adjust(stack_delta);
    }
    fn op_u2(&mut self, byte: u8, arg: u16, stack_delta: i32) {
        self.bytes.push(byte);
        self.bytes.extend_from_slice(&arg.to_be_bytes());
        self.adjust(stack_delta);
    }

    // loads (push) — `wide` slots (long/double) push 2 but JVM stack words; we count words.
    pub fn iload(&mut self, idx: u16) {
        self.load(0x15, idx, 1);
    }
    pub fn lload(&mut self, idx: u16) {
        self.load(0x16, idx, 2);
    }
    pub fn dload(&mut self, idx: u16) {
        self.load(0x18, idx, 2);
    }
    pub fn aload(&mut self, idx: u16) {
        self.load(0x19, idx, 1);
    }
    fn load(&mut self, base: u8, idx: u16, words: i32) {
        // generic form with u1 index (v0: <256 locals); wide form deferred
        self.op_u1(base, idx as u8, words);
    }

    pub fn istore(&mut self, idx: u16) {
        self.store(0x36, idx, 1);
    }
    pub fn lstore(&mut self, idx: u16) {
        self.store(0x37, idx, 2);
    }
    pub fn dstore(&mut self, idx: u16) {
        self.store(0x39, idx, 2);
    }
    pub fn astore(&mut self, idx: u16) {
        self.store(0x3a, idx, 1);
    }
    fn store(&mut self, base: u8, idx: u16, words: i32) {
        self.op_u1(base, idx as u8, -words);
        self.ensure_locals(idx + words as u16);
    }

    // int constants
    pub fn push_int(&mut self, v: i32, cw: &mut ClassWriter) {
        match v {
            -1..=5 => self.op((0x03i16 + v as i16) as u8, 1), // iconst_m1..iconst_5 = 0x02..0x08
            -128..=127 => self.op_u1(0x10, v as u8, 1),                // bipush
            -32768..=32767 => self.op_u2(0x11, v as u16, 1),           // sipush
            _ => {
                let i = cw.const_int(v);
                self.ldc(i);
            }
        }
    }
    pub fn push_long(&mut self, v: i64, cw: &mut ClassWriter) {
        if v == 0 {
            self.op(0x09, 2); // lconst_0
        } else if v == 1 {
            self.op(0x0a, 2); // lconst_1
        } else {
            let i = cw.const_long(v);
            self.op_u2(0x14, i, 2); // ldc2_w
        }
    }
    pub fn push_double(&mut self, v: f64, cw: &mut ClassWriter) {
        let i = cw.const_double(v);
        self.op_u2(0x14, i, 2); // ldc2_w
    }
    pub fn push_string(&mut self, s: &str, cw: &mut ClassWriter) {
        let i = cw.const_string(s);
        self.ldc(i);
    }
    fn ldc(&mut self, idx: u16) {
        if idx <= 255 {
            self.op_u1(0x12, idx as u8, 1); // ldc
        } else {
            self.op_u2(0x13, idx, 1); // ldc_w
        }
    }

    // arithmetic (pop 2 push 1 => -1 for int/ref words; long/double pop 4 push 2 => -2)
    pub fn iadd(&mut self) { self.op(0x60, -1); }
    pub fn isub(&mut self) { self.op(0x64, -1); }
    pub fn imul(&mut self) { self.op(0x68, -1); }
    pub fn idiv(&mut self) { self.op(0x6c, -1); }
    pub fn irem(&mut self) { self.op(0x70, -1); }
    pub fn ineg(&mut self) { self.op(0x74, 0); }
    pub fn ladd(&mut self) { self.op(0x61, -2); }
    pub fn lsub(&mut self) { self.op(0x65, -2); }
    pub fn lmul(&mut self) { self.op(0x69, -2); }
    pub fn ldiv(&mut self) { self.op(0x6d, -2); }
    pub fn lrem(&mut self) { self.op(0x71, -2); }
    pub fn lneg(&mut self) { self.op(0x75, 0); }
    pub fn dadd(&mut self) { self.op(0x63, -2); }
    pub fn dsub(&mut self) { self.op(0x67, -2); }
    pub fn dmul(&mut self) { self.op(0x6b, -2); }
    pub fn ddiv(&mut self) { self.op(0x6f, -2); }
    pub fn drem(&mut self) { self.op(0x73, -2); }
    pub fn dneg(&mut self) { self.op(0x77, 0); }

    // conversions
    pub fn i2l(&mut self) { self.op(0x85, 1); }
    pub fn i2d(&mut self) { self.op(0x87, 1); }
    pub fn l2d(&mut self) { self.op(0x8a, 0); }

    // returns
    pub fn ireturn(&mut self) { self.op(0xac, -1); }
    pub fn lreturn(&mut self) { self.op(0xad, -2); }
    pub fn dreturn(&mut self) { self.op(0xaf, -2); }
    pub fn areturn(&mut self) { self.op(0xb0, -1); }
    pub fn ret_void(&mut self) { self.op(0xb1, 0); }

    // calls / fields. `arg_words`/`ret_words` describe the stack effect from the descriptor.
    pub fn invokestatic(&mut self, methodref: u16, arg_words: i32, ret_words: i32) {
        self.op_u2(0xb8, methodref, ret_words - arg_words);
    }
    pub fn invokevirtual(&mut self, methodref: u16, arg_words: i32, ret_words: i32) {
        // pops receiver + args, pushes return
        self.op_u2(0xb6, methodref, ret_words - arg_words - 1);
    }
    pub fn getstatic(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb2, fieldref, words);
    }
    pub fn pop(&mut self) { self.op(0x57, -1); }
    pub fn pop2(&mut self) { self.op(0x58, -2); }
    pub fn dup(&mut self) { self.op(0x59, 1); }
    pub fn ixor(&mut self) { self.op(0x82, -1); }

    /// `new <class>` (push uninitialized ref).
    pub fn new_obj(&mut self, class_index: u16) {
        self.op_u2(0xbb, class_index, 1);
    }
    pub fn invokespecial(&mut self, methodref: u16, arg_words: i32, ret_words: i32) {
        self.op_u2(0xb7, methodref, ret_words - arg_words - 1);
    }

    /// Emit a numeric widening conversion from `from` to `to` (Int<Long<Double). No-op if equal.
    pub fn widen(&mut self, from: crate::types::Ty, to: crate::types::Ty) {
        use crate::types::Ty::*;
        match (from, to) {
            (Int, Long) => self.i2l(),
            (Int, Double) => self.i2d(),
            (Long, Double) => self.l2d(),
            _ => {}
        }
    }
}

/// Constant-pool index of a `Class` entry, exposed for `new`.
impl ClassWriter {
    pub fn class_ref(&mut self, internal: &str) -> u16 {
        self.cp.class(internal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_version() {
        let cw = ClassWriter::new("FooKt", "java/lang/Object");
        let bytes = cw.finish();
        assert_eq!(&bytes[0..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), MAJOR_JAVA8);
    }

    #[test]
    fn add_method_builds() {
        let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
        let mut code = CodeBuilder::new(2); // (II) => 2 locals
        code.iload(0);
        code.iload(1);
        code.iadd();
        code.ireturn();
        assert_eq!(code.max_stack, 2);
        assert_eq!(code.max_locals, 2);
        cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, "add", "(II)I", &code);
        let bytes = cw.finish();
        // methods_count is the u16 right after fields_count(0); just sanity-check non-trivial size.
        assert!(bytes.len() > 40);
    }

    #[test]
    fn constant_pool_dedups() {
        let mut cp = ConstPool::default();
        let a = cp.utf8("X");
        let b = cp.utf8("X");
        assert_eq!(a, b);
    }

    #[test]
    fn long_takes_two_slots() {
        let mut cp = ConstPool::default();
        let _l = cp.long(5);
        let after = cp.utf8("next");
        // long consumed 2 slots (indices 1,2), so next utf8 is index 3
        assert_eq!(after, 3);
    }

    #[test]
    fn stack_tracking_for_constants() {
        let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.push_int(1000, &mut cw); // sipush (+1)
        code.push_int(7, &mut cw); // iconst-ish (+1) => stack 2
        code.iadd(); // -1 => 1
        code.ireturn();
        assert_eq!(code.max_stack, 2);
    }
}

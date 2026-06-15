//! A hand-written JVM class-file writer (the format is well-specified; no external crate).
//! Targets major version 50 (Java 6). Methods that create lambda objects (new $lambda$N) emit a
//! StackMapTable attribute so the type-checking verifier on Java 25+ accepts them.

use std::collections::HashMap;

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_FINAL: u16 = 0x0010;
pub const ACC_SUPER: u16 = 0x0020;
pub const ACC_INTERFACE: u16 = 0x0200;
pub const ACC_ABSTRACT: u16 = 0x0400;

// Major 52 = Java 8, matching kotlinc's default JVM target.
const MAJOR_JAVA8: u16 = 52;

/// JVM verification type for StackMapTable entries (JVMS §4.7.4).
#[derive(Clone, PartialEq)]
pub enum VerifType {
    Top,
    Integer,
    Float,
    Long,
    Double,
    Null,
    Object(u16), // constant-pool index of a Class entry
}

fn write_verif_type(vt: &VerifType, out: &mut Vec<u8>) {
    match vt {
        VerifType::Top => out.push(0),
        VerifType::Integer => out.push(1),
        VerifType::Float => out.push(2),
        VerifType::Double => out.push(3),
        VerifType::Long => out.push(4),
        VerifType::Null => out.push(5),
        VerifType::Object(idx) => { out.push(7); u2(out, *idx); }
    }
}

#[derive(PartialEq, Eq, Hash, Clone)]
enum Const {
    Utf8(String),
    Integer(i32),
    Float(u32),  // bit pattern (f32 isn't Hash/Eq)
    Long(i64),
    Double(u64), // bit pattern (f64 isn't Hash/Eq)
    Class(u16),
    String(u16),
    NameAndType(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
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
    fn float(&mut self, v: f32) -> u16 {
        self.intern(Const::Float(v.to_bits()))
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
    fn interface_methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let c = self.class(class);
        let nt = self.name_and_type(name, desc);
        self.intern(Const::InterfaceMethodref(c, nt))
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
                    let b = crate::metadata::encoding::modified_utf8(s);
                    u2(out, b.len() as u16);
                    out.extend_from_slice(&b);
                }
                Const::Integer(v) => {
                    out.push(3);
                    u4(out, *v as u32);
                }
                Const::Float(bits) => {
                    out.push(4);
                    u4(out, *bits);
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
                Const::InterfaceMethodref(c, nt) => {
                    out.push(11);
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
    /// `None` for an abstract method (no `Code` attribute).
    code: Option<Vec<u8>>,
    /// `Code` exception table: `(start_pc, end_pc, handler_pc, catch_type)` — `catch_type` is a
    /// constant-pool class index, or 0 for a catch-all.
    exceptions: Vec<(u16, u16, u16, u16)>,
    /// Pre-built StackMapTable attribute body (after name+length fields). `None` if no frames.
    stackmap: Option<Vec<u8>>,
}

struct FieldInfo {
    access: u16,
    name: u16,
    desc: u16,
}

pub struct ClassWriter {
    cp: ConstPool,
    access: u16,
    this_class: u16,
    super_class: u16,
    interfaces: Vec<u16>,
    fields: Vec<FieldInfo>,
    methods: Vec<MethodInfo>,
    class_attributes: Vec<(u16, Vec<u8>)>, // (name_index, raw bytes)
    pub internal_name: String,
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
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            class_attributes: Vec::new(),
            internal_name: internal_name.to_string(),
        }
    }

    /// Override the class access flags (e.g. `ACC_PUBLIC | ACC_INTERFACE | ACC_ABSTRACT`).
    pub fn set_access(&mut self, access: u16) {
        self.access = access;
    }

    /// Add an implemented interface / extended interface by internal name.
    pub fn add_interface(&mut self, internal: &str) {
        let c = self.cp.class(internal);
        self.interfaces.push(c);
    }

    /// Declare an abstract method (no `Code` attribute) — for interfaces.
    pub fn add_abstract_method(&mut self, access: u16, name: &str, desc: &str) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        self.methods.push(MethodInfo { access: access | ACC_ABSTRACT, name: n, desc: d, max_stack: 0, max_locals: 0, code: None, exceptions: Vec::new(), stackmap: None });
    }

    /// Declare a field (e.g. a backing field for a Kotlin property).
    pub fn add_field(&mut self, access: u16, name: &str, desc: &str) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        self.fields.push(FieldInfo { access, name: n, desc: d });
    }

    /// Attach a `@kotlin.Metadata` annotation (RuntimeVisibleAnnotations) describing the file facade.
    /// `d1`/`d2` are the encoded protobuf payload + string table.
    pub fn set_kotlin_metadata(&mut self, k: i32, mv: &[i32], xi: i32, d1: &[String], d2: &[String]) {
        let anno_type = self.cp.utf8("Lkotlin/Metadata;");
        let n_mv = self.cp.utf8("mv");
        let n_k = self.cp.utf8("k");
        let n_xi = self.cp.utf8("xi");
        let n_d1 = self.cp.utf8("d1");
        let n_d2 = self.cp.utf8("d2");

        let mut body = Vec::new();
        u2(&mut body, 1); // num_annotations
        u2(&mut body, anno_type);
        u2(&mut body, 5); // element_value_pairs: mv, k, xi, d1, d2
        u2(&mut body, n_mv);
        self.ev_int_array(&mut body, mv);
        u2(&mut body, n_k);
        self.ev_int(&mut body, k);
        u2(&mut body, n_xi);
        self.ev_int(&mut body, xi);
        u2(&mut body, n_d1);
        self.ev_str_array(&mut body, d1);
        u2(&mut body, n_d2);
        self.ev_str_array(&mut body, d2);

        let name = self.cp.utf8("RuntimeVisibleAnnotations");
        self.class_attributes.push((name, body));
    }

    fn ev_int(&mut self, out: &mut Vec<u8>, v: i32) {
        out.push(b'I');
        let idx = self.cp.integer(v);
        u2(out, idx);
    }
    fn ev_str(&mut self, out: &mut Vec<u8>, s: &str) {
        out.push(b's');
        let idx = self.cp.utf8(s);
        u2(out, idx);
    }
    fn ev_int_array(&mut self, out: &mut Vec<u8>, vs: &[i32]) {
        out.push(b'[');
        u2(out, vs.len() as u16);
        for &v in vs {
            self.ev_int(out, v);
        }
    }
    fn ev_str_array(&mut self, out: &mut Vec<u8>, ss: &[String]) {
        out.push(b'[');
        u2(out, ss.len() as u16);
        for s in ss {
            self.ev_str(out, s);
        }
    }

    /// Intern helpers exposed for the emitter (Phase 4) to reference pool entries while building code.
    pub fn methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        self.cp.methodref(class, name, desc)
    }
    pub fn interface_methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        self.cp.interface_methodref(class, name, desc)
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
    pub fn const_float(&mut self, v: f32) -> u16 {
        self.cp.float(v)
    }
    pub fn const_double(&mut self, v: f64) -> u16 {
        self.cp.double(v)
    }

    pub fn add_method(&mut self, access: u16, name: &str, desc: &str, code: &CodeBuilder) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        let stackmap = code.build_stackmap();
        self.methods.push(MethodInfo {
            access,
            name: n,
            desc: d,
            max_stack: code.max_stack,
            max_locals: code.max_locals,
            code: Some(code.bytes.clone()),
            exceptions: code.resolved_exceptions(),
            stackmap,
        });
    }

    pub fn finish(mut self) -> Vec<u8> {
        let code_attr_name = self.cp.utf8("Code");
        let stackmap_attr_name = self.cp.utf8("StackMapTable");
        let mut out = Vec::new();
        u4(&mut out, 0xCAFEBABE);
        u2(&mut out, 0); // minor
        u2(&mut out, MAJOR_JAVA8);
        self.cp.serialize(&mut out);
        u2(&mut out, self.access);
        u2(&mut out, self.this_class);
        u2(&mut out, self.super_class);
        u2(&mut out, self.interfaces.len() as u16);
        for &i in &self.interfaces {
            u2(&mut out, i);
        }
        u2(&mut out, self.fields.len() as u16);
        for f in &self.fields {
            u2(&mut out, f.access);
            u2(&mut out, f.name);
            u2(&mut out, f.desc);
            u2(&mut out, 0); // field attributes
        }
        u2(&mut out, self.methods.len() as u16);
        for m in &self.methods {
            u2(&mut out, m.access);
            u2(&mut out, m.name);
            u2(&mut out, m.desc);
            match &m.code {
                None => u2(&mut out, 0), // abstract method: no attributes
                Some(code) => {
                    u2(&mut out, 1); // attributes: Code
                    u2(&mut out, code_attr_name);
                    let code_len = code.len();
                    let sm_overhead = match &m.stackmap {
                        None => 0,
                        Some(sm) => 2 + 4 + sm.len(), // name_idx + length + body
                    };
                    let num_code_attrs: u16 = if m.stackmap.is_some() { 1 } else { 0 };
                    // Code attr body: max_stack(2) + max_locals(2) + code_len(4) + code + exception_count(2) + exceptions + code_attrs_count(2) + [stackmap]
                    let attr_len = 2 + 2 + 4 + code_len + 2 + m.exceptions.len() * 8 + 2 + sm_overhead;
                    u4(&mut out, attr_len as u32);
                    u2(&mut out, m.max_stack);
                    u2(&mut out, m.max_locals);
                    u4(&mut out, code_len as u32);
                    out.extend_from_slice(code);
                    u2(&mut out, m.exceptions.len() as u16); // exception_table_length
                    for &(start, end, handler, catch_type) in &m.exceptions {
                        u2(&mut out, start);
                        u2(&mut out, end);
                        u2(&mut out, handler);
                        u2(&mut out, catch_type);
                    }
                    u2(&mut out, num_code_attrs);
                    if let Some(sm) = &m.stackmap {
                        u2(&mut out, stackmap_attr_name);
                        u4(&mut out, sm.len() as u32);
                        out.extend_from_slice(sm);
                    }
                }
            }
        }
        u2(&mut out, self.class_attributes.len() as u16);
        for (name, bytes) in &self.class_attributes {
            u2(&mut out, *name);
            u4(&mut out, bytes.len() as u32);
            out.extend_from_slice(bytes);
        }
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Label(u32);

pub struct CodeBuilder {
    pub bytes: Vec<u8>,
    pub max_stack: u16,
    pub max_locals: u16,
    cur_stack: i32,
    labels: Vec<usize>,        // label id -> bound byte offset (usize::MAX until bound)
    fixups: Vec<(usize, u32)>, // (operand position, label id) to patch in link()
    /// Exception-table entries by label: `(start, end, handler, catch_type)`, resolved in `link()`.
    exceptions: Vec<(Label, Label, Label, u16)>,
    /// Whether this method creates a lambda object (new $ClassName$lambda$N). When true, we must
    /// emit a StackMapTable so the Java 25 type-checking verifier accepts the class.
    pub needs_stackmap: bool,
    /// Frames to include in the StackMapTable: (label_id, locals, stack).
    /// Added via `add_frame_if_new`; first registration for a given label wins.
    frames: Vec<(u32, Vec<VerifType>, Vec<VerifType>)>,
}

impl CodeBuilder {
    pub fn new(arg_locals: u16) -> CodeBuilder {
        CodeBuilder {
            bytes: Vec::new(),
            max_stack: 0,
            max_locals: arg_locals,
            cur_stack: 0,
            labels: Vec::new(),
            fixups: Vec::new(),
            exceptions: Vec::new(),
            needs_stackmap: false,
            frames: Vec::new(),
        }
    }

    /// Mark that this method creates a lambda object. Causes a StackMapTable to be emitted.
    pub fn set_needs_stackmap(&mut self) {
        self.needs_stackmap = true;
    }

    /// Record the frame at `label` (given locals + stack) if not already recorded.
    /// First registration wins — early callers capture the "outer" scope before inner vars appear.
    /// `stack` is the operand-stack verification types at this label (empty in most cases).
    pub fn add_frame_if_new(&mut self, label: Label, locals: Vec<VerifType>, stack: Vec<VerifType>) {
        let lid = label.0;
        if !self.frames.iter().any(|(id, _, _)| *id == lid) {
            self.frames.push((lid, locals, stack));
        }
    }

    /// Build the StackMapTable attribute body. Returns `None` when no frames are needed.
    /// Emits a `full_frame` entry for every registered label, sorted by bytecode offset.
    pub fn build_stackmap(&self) -> Option<Vec<u8>> {
        if self.frames.is_empty() {
            return None;
        }
        // Resolve label ids to bytecode offsets and sort by offset.
        let code_len = self.bytes.len();
        let mut entries: Vec<(u32, &Vec<VerifType>, &Vec<VerifType>)> = self.frames.iter()
            .map(|(lid, locals, stack)| (self.labels[*lid as usize] as u32, locals, stack))
            // Drop frames whose offset is outside the bytecode (e.g. an `end` label bound one past
            // the last `ireturn`/`athrow` when every branch of a `when` diverges). The JVM verifier
            // rejects StackMapTable entries with out-of-range offsets.
            .filter(|(off, _, _)| (*off as usize) < code_len)
            .collect();
        entries.sort_by_key(|&(off, _, _)| off);
        // Multiple labels may be bound at the same offset (e.g. `next` and `end` in an all-diverging
        // `when`). Keep only the first frame at each offset; duplicates would underflow the delta.
        entries.dedup_by_key(|(off, _, _)| *off);

        let mut body = Vec::new();
        u2(&mut body, entries.len() as u16);

        // Offset deltas: the first entry's delta = offset; subsequent = offset - prev_offset - 1.
        let mut prev: i64 = -1;
        for (offset, locals, stack) in entries {
            let delta = if prev < 0 { offset } else { offset - prev as u32 - 1 };
            prev = offset as i64;
            body.push(255u8); // full_frame
            u2(&mut body, delta as u16);
            u2(&mut body, locals.len() as u16);
            for vt in locals {
                write_verif_type(vt, &mut body);
            }
            u2(&mut body, stack.len() as u16);
            for vt in stack {
                write_verif_type(vt, &mut body);
            }
        }
        Some(body)
    }

    /// Register a `try` range `[start, end)` guarded by a handler at `handler`, catching `catch_type`
    /// (a constant-pool class index, or 0 for catch-all).
    pub fn add_exception(&mut self, start: Label, end: Label, handler: Label, catch_type: u16) {
        self.exceptions.push((start, end, handler, catch_type));
    }

    /// Resolve the exception table to byte offsets (call after all labels are bound, e.g. in `link`).
    /// Drops degenerate ranges where `start >= end` (an empty protected region — e.g. an empty `try`
    /// body — protects nothing, and an empty range is an illegal `Code` exception-table entry).
    pub fn resolved_exceptions(&self) -> Vec<(u16, u16, u16, u16)> {
        self.exceptions
            .iter()
            .map(|&(s, e, h, t)| {
                (self.labels[s.0 as usize] as u16, self.labels[e.0 as usize] as u16, self.labels[h.0 as usize] as u16, t)
            })
            .filter(|&(start, end, _, _)| start < end)
            .collect()
    }

    /// The current (linearly tracked) operand-stack height.
    pub fn stack_height(&self) -> i32 {
        self.cur_stack
    }

    /// Force the current operand-stack height (e.g. an exception handler is entered with the caught
    /// exception already on the stack). Keeps `max_stack` correct across non-linear control flow.
    pub fn set_stack(&mut self, n: u16) {
        self.cur_stack = n as i32;
        if n > self.max_stack {
            self.max_stack = n;
        }
    }

    // ---- branches & labels ----
    pub fn new_label(&mut self) -> Label {
        let id = self.labels.len() as u32;
        self.labels.push(usize::MAX);
        Label(id)
    }
    pub fn bind(&mut self, l: Label) {
        self.labels[l.0 as usize] = self.bytes.len();
    }
    fn branch(&mut self, opcode: u8, l: Label, delta: i32) {
        self.bytes.push(opcode);
        let pos = self.bytes.len();
        self.fixups.push((pos, l.0));
        self.bytes.extend_from_slice(&[0, 0]);
        self.adjust(delta);
    }
    pub fn goto(&mut self, l: Label) { self.branch(0xa7, l, 0); }
    pub fn ifeq(&mut self, l: Label) { self.branch(0x99, l, -1); }
    pub fn ifne(&mut self, l: Label) { self.branch(0x9a, l, -1); }
    pub fn if_icmpeq(&mut self, l: Label) { self.branch(0x9f, l, -2); }
    pub fn if_icmpne(&mut self, l: Label) { self.branch(0xa0, l, -2); }
    pub fn if_icmplt(&mut self, l: Label) { self.branch(0xa1, l, -2); }
    pub fn if_icmpge(&mut self, l: Label) { self.branch(0xa2, l, -2); }
    pub fn if_icmpgt(&mut self, l: Label) { self.branch(0xa3, l, -2); }
    pub fn if_icmple(&mut self, l: Label) { self.branch(0xa4, l, -2); }
    pub fn lcmp(&mut self) { self.op(0x94, -3); }
    pub fn dcmpg(&mut self) { self.op(0x98, -3); }
    pub fn ifnull(&mut self, l: Label) { self.branch(0xc6, l, -1); }
    pub fn ifnonnull(&mut self, l: Label) { self.branch(0xc7, l, -1); }
    pub fn iflt(&mut self, l: Label) { self.branch(0x9b, l, -1); }
    pub fn ifge(&mut self, l: Label) { self.branch(0x9c, l, -1); }
    pub fn ifgt(&mut self, l: Label) { self.branch(0x9d, l, -1); }
    pub fn ifle(&mut self, l: Label) { self.branch(0x9e, l, -1); }

    /// Resolve all branch offsets. Call once after the method body is built.
    pub fn link(&mut self) {
        for &(pos, lid) in &self.fixups {
            let target = self.labels[lid as usize];
            debug_assert!(target != usize::MAX, "unbound label {lid}");
            let off = target as i64 - (pos - 1) as i64; // opcode is 1 byte before operand
            let b = (off as i16).to_be_bytes();
            self.bytes[pos] = b[0];
            self.bytes[pos + 1] = b[1];
        }
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
    pub fn fload(&mut self, idx: u16) {
        self.load(0x17, idx, 1);
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
    pub fn fstore(&mut self, idx: u16) {
        self.store(0x38, idx, 1);
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
    pub fn push_float(&mut self, v: f32, cw: &mut ClassWriter) {
        let i = cw.const_float(v);
        self.ldc(i); // float is one slot
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
    pub fn fadd(&mut self) { self.op(0x62, -1); }
    pub fn fsub(&mut self) { self.op(0x66, -1); }
    pub fn fmul(&mut self) { self.op(0x6a, -1); }
    pub fn fdiv(&mut self) { self.op(0x6e, -1); }
    pub fn frem(&mut self) { self.op(0x72, -1); }
    pub fn fneg(&mut self) { self.op(0x76, 0); }
    /// `fcmpg`: pops two floats, pushes an int (-1/0/1).
    pub fn fcmpg(&mut self) { self.op(0x96, -1); }

    // conversions
    pub fn i2l(&mut self) { self.op(0x85, 1); }
    pub fn i2d(&mut self) { self.op(0x87, 1); }
    pub fn l2d(&mut self) { self.op(0x8a, 0); }
    pub fn i2f(&mut self) { self.op(0x86, 0); }
    pub fn l2f(&mut self) { self.op(0x89, -1); }
    pub fn f2d(&mut self) { self.op(0x8d, 1); }
    pub fn l2i(&mut self) { self.op(0x88, -1); }
    pub fn f2i(&mut self) { self.op(0x8b, 0); }
    pub fn f2l(&mut self) { self.op(0x8c, 1); }
    pub fn d2i(&mut self) { self.op(0x8e, -1); }
    pub fn d2l(&mut self) { self.op(0x8f, 0); }
    pub fn d2f(&mut self) { self.op(0x90, -1); }
    /// `iinc index, const` — increment a local int in place (no stack effect).
    pub fn iinc(&mut self, idx: u16, delta: i8) {
        self.bytes.push(0x84);
        self.bytes.push(idx as u8);
        self.bytes.push(delta as u8);
        self.ensure_locals(idx + 1);
    }
    pub fn i2b(&mut self) { self.op(0x91, 0); }
    pub fn i2s(&mut self) { self.op(0x93, 0); }

    // returns
    pub fn ireturn(&mut self) { self.op(0xac, -1); }
    pub fn lreturn(&mut self) { self.op(0xad, -2); }
    pub fn freturn(&mut self) { self.op(0xae, -1); }
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
    /// `invokeinterface <iface-methodref> <count> 0` — `count` = receiver + arg words.
    pub fn invokeinterface(&mut self, iref: u16, arg_words: i32, ret_words: i32) {
        self.bytes.push(0xb9);
        self.bytes.extend_from_slice(&iref.to_be_bytes());
        self.bytes.push((arg_words + 1) as u8); // count includes the receiver
        self.bytes.push(0);
        self.adjust(ret_words - arg_words - 1);
    }
    pub fn getstatic(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb2, fieldref, words);
    }
    pub fn putstatic(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb3, fieldref, -words);
    }
    /// `getfield`: pops objectref, pushes the field value (`words` wide).
    pub fn getfield(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb4, fieldref, words - 1);
    }
    /// `putfield`: pops objectref + value (`words` wide).
    pub fn putfield(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb5, fieldref, -(1 + words));
    }
    pub fn pop(&mut self) { self.op(0x57, -1); }
    pub fn pop2(&mut self) { self.op(0x58, -2); }
    pub fn dup(&mut self) { self.op(0x59, 1); }

    // ---- arrays ----
    /// `arraylength`: pops arrayref, pushes int.
    pub fn arraylength(&mut self) { self.op(0xbe, 0); }
    /// `newarray <atype>`: pops count, pushes a primitive arrayref. (boolean=4 char=5 float=6
    /// double=7 byte=8 short=9 int=10 long=11)
    pub fn newarray(&mut self, atype: u8) { self.op_u1(0xbc, atype, 0); }
    /// `anewarray <class>`: pops count, pushes a reference arrayref.
    pub fn anewarray(&mut self, class_index: u16) { self.op_u2(0xbd, class_index, 0); }
    /// Array load `Xaload`: pops arrayref + index, pushes a value `words` wide.
    pub fn array_load(&mut self, opcode: u8, words: i32) { self.op(opcode, words - 2); }
    /// Array store `Xastore`: pops arrayref + index + value (`words` wide).
    pub fn array_store(&mut self, opcode: u8, words: i32) { self.op(opcode, -(2 + words)); }
    pub fn ixor(&mut self) { self.op(0x82, -1); }
    pub fn iand(&mut self) { self.op(0x7e, -1); }
    pub fn aconst_null(&mut self) { self.op(0x01, 1); }
    pub fn athrow(&mut self) { self.op(0xbf, -1); }

    /// `instanceof <class>` (pops ref, pushes int 0/1).
    pub fn instance_of(&mut self, class_index: u16) { self.op_u2(0xc1, class_index, 0); }
    /// `checkcast <class>` (ref -> ref).
    pub fn checkcast(&mut self, class_index: u16) { self.op_u2(0xc0, class_index, 0); }
    /// `if_acmpne` — branch if two refs are not the same object.
    pub fn if_acmpne(&mut self, l: Label) { self.branch(0xa6, l, -2); }

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
            (Int, Float) => self.i2f(),
            (Long, Float) => self.l2f(),
            (Float, Double) => self.f2d(),
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

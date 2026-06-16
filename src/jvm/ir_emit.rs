//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of the backend-agnostic IR — it maps
//! Kotlin FqNames to JVM descriptors here (the IR never carries descriptors). Covers the same core
//! subset as `ir_lower`; shares the `CodeBuilder`/`ClassWriter` infrastructure with the AST emitter.

use std::collections::HashMap;

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrType};
use crate::jvm::classfile::{ClassWriter, CodeBuilder, Label, VerifType};
use crate::jvm::emit::method_descriptor;
use crate::types::Ty;

/// Emit a whole IR file as one facade class (`internal`) of `public static` methods → `.class` bytes.
pub fn emit_file(ir: &IrFile, internal: &str) -> Vec<u8> {
    let mut cw = ClassWriter::new(internal, "java/lang/Object");
    for f in &ir.functions {
        let Some(body) = f.body else { continue };
        let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
        let ret = ir_ty_to_jvm(&f.ret);
        let mut e = Emitter { ir, cw: &mut cw, internal: internal.to_string(), slots: HashMap::new(), next_slot: 0, ret };
        for (i, t) in param_tys.iter().enumerate() {
            let slot = e.next_slot;
            e.slots.insert(i as u32, (slot, *t));
            e.next_slot += slot_words(*t);
        }
        let mut code = CodeBuilder::new(e.next_slot);
        e.emit(body, &mut code);
        if ret == Ty::Unit {
            code.ret_void();
        }
        code.ensure_locals(e.next_slot);
        code.link();
        e.cw.add_method(0x0009 /* PUBLIC|STATIC */, &f.name, &method_descriptor(&param_tys, ret), &code);
    }
    cw.finish()
}

struct Emitter<'a> {
    ir: &'a IrFile,
    cw: &'a mut ClassWriter,
    internal: String,
    slots: HashMap<u32, (u16, Ty)>,
    next_slot: u16,
    ret: Ty,
}

impl<'a> Emitter<'a> {
    /// Statement position: yields no operand-stack value.
    fn emit(&mut self, e: u32, code: &mut CodeBuilder) {
        match self.ir.expr(e).clone() {
            IrExpr::Block { stmts, value } => {
                for s in stmts {
                    self.emit(s, code);
                }
                if let Some(v) = value {
                    self.emit_value(v, code);
                    discard(self.value_ty(v), code);
                }
            }
            IrExpr::Return(v) => match v {
                Some(v) => {
                    self.emit_value(v, code);
                    emit_return(self.ret, code);
                }
                None => code.ret_void(),
            },
            IrExpr::Variable { index, ty, init } => {
                let jt = ir_ty_to_jvm(&ty);
                let slot = self.next_slot;
                self.next_slot += slot_words(jt);
                self.slots.insert(index, (slot, jt));
                if let Some(i) = init {
                    self.emit_value(i, code);
                    store(jt, slot, code);
                }
            }
            IrExpr::SetValue { var, value } => {
                let (slot, jt) = self.slots[&var];
                self.emit_value(value, code);
                store(jt, slot, code);
            }
            other => {
                self.emit_value_node(&other, code);
                discard(self.value_ty(e), code);
            }
        }
    }

    /// Expression position: leaves the value on the operand stack.
    fn emit_value(&mut self, e: u32, code: &mut CodeBuilder) {
        let node = self.ir.expr(e).clone();
        self.emit_value_node(&node, code);
    }

    fn emit_value_node(&mut self, node: &IrExpr, code: &mut CodeBuilder) {
        match node {
            IrExpr::Const(c) => match c {
                IrConst::Boolean(b) => code.push_int(if *b { 1 } else { 0 }, self.cw),
                IrConst::Int(v) => code.push_int(*v, self.cw),
                IrConst::Short(v) => code.push_int(*v as i32, self.cw),
                IrConst::Byte(v) => code.push_int(*v as i32, self.cw),
                IrConst::Char(v) => code.push_int(*v as i32, self.cw),
                IrConst::Long(v) => code.push_long(*v, self.cw),
                IrConst::String(s) => code.push_string(s, self.cw),
                IrConst::Double(_) | IrConst::Float(_) | IrConst::Null => {}
            },
            IrExpr::GetValue(i) => {
                let (slot, jt) = self.slots[i];
                load(jt, slot, code);
            }
            IrExpr::Call { callee, dispatch_receiver, args } => match callee {
                Callee::Local(fid) => {
                    let f = &self.ir.functions[*fid as usize];
                    let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
                    let ret = ir_ty_to_jvm(&f.ret);
                    let name = f.name.clone();
                    let args = args.clone();
                    for &a in &args {
                        self.emit_value(a, code);
                    }
                    let arg_words: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let desc = method_descriptor(&param_tys, ret);
                    let owner = self.internal.clone();
                    let m = self.cw.methodref(&owner, &name, &desc);
                    code.invokestatic(m, arg_words, slot_words(ret) as i32);
                }
                // Stdlib intrinsic, mapped to the JVM platform here (the IR is target-neutral).
                Callee::Intrinsic(fq) => self.emit_intrinsic(fq, dispatch_receiver, args, code),
            },
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.emit_binop(*op, *lhs, *rhs, code),
            IrExpr::When { branches } => self.emit_when(branches, code),
            _ => {}
        }
    }

    /// The JVM platform's realization of a stdlib intrinsic named by Kotlin FqName.
    fn emit_intrinsic(&mut self, fq: &str, recv: &Option<u32>, args: &[u32], code: &mut CodeBuilder) {
        match fq {
            // `String.plus`: `recv + arg` → `new StringBuilder().append(recv).append(arg).toString()`.
            "kotlin/String.plus" => {
                let recv = recv.unwrap();
                let arg = args[0];
                let sb = self.cw.class_ref("java/lang/StringBuilder");
                code.new_obj(sb);
                code.dup();
                let init = self.cw.methodref("java/lang/StringBuilder", "<init>", "()V");
                code.invokespecial(init, 0, 0);
                self.append(recv, code);
                self.append(arg, code);
                let ts = self.cw.methodref("java/lang/StringBuilder", "toString", "()Ljava/lang/String;");
                code.invokevirtual(ts, 0, 1);
            }
            _ => {} // unknown intrinsic — lowering shouldn't produce it (file would have been skipped)
        }
    }

    /// Append a value to a `StringBuilder` already on the stack (leaves the builder on the stack).
    fn append(&mut self, e: u32, code: &mut CodeBuilder) {
        let ty = self.value_ty(e);
        self.emit_value(e, code);
        let desc = match ty {
            Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/StringBuilder;",
            Ty::Long => "(J)Ljava/lang/StringBuilder;",
            Ty::Boolean => "(Z)Ljava/lang/StringBuilder;",
            Ty::Char => "(C)Ljava/lang/StringBuilder;",
            Ty::Double => "(D)Ljava/lang/StringBuilder;",
            Ty::Float => "(F)Ljava/lang/StringBuilder;",
            Ty::String => "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
            _ => "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
        };
        let m = self.cw.methodref("java/lang/StringBuilder", "append", desc);
        let aw = slot_words(ty) as i32;
        code.invokevirtual(m, aw, 1);
    }

    fn emit_binop(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        match op {
            Add | Sub | Mul | Div | Rem => {
                self.emit_value(lhs, code);
                self.emit_value(rhs, code);
                if lt == Ty::Long {
                    match op { Add => code.ladd(), Sub => code.lsub(), Mul => code.lmul(), Div => code.ldiv(), Rem => code.lrem(), _ => unreachable!() }
                } else {
                    match op { Add => code.iadd(), Sub => code.isub(), Mul => code.imul(), Div => code.idiv(), Rem => code.irem(), _ => unreachable!() }
                }
            }
            // `&&`/`||`: the rhs may itself be branchy (a comparison), so the lhs result must not sit
            // on the operand stack underneath its frames — spill the lhs to a temp local first.
            And | Or => {
                self.emit_value(lhs, code);
                let tmp = self.next_slot;
                self.next_slot += 1;
                self.slots.insert(1_000_000 + tmp as u32, (tmp, Ty::Boolean));
                code.istore(tmp);
                self.emit_value(rhs, code);
                code.iload(tmp);
                if op == And { code.iand() } else { code.ior() }
            }
            Lt | Le | Gt | Ge | Eq | Ne => self.emit_compare(op, lhs, rhs, code),
        }
    }

    fn emit_compare(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        let lt = self.value_ty(lhs);
        // Reference equality (`String ==`/`!=`) → Objects.equals, no branching.
        if lt == Ty::String {
            self.emit_value(lhs, code);
            self.emit_value(rhs, code);
            let m = self.cw.methodref("java/util/Objects", "equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z");
            code.invokestatic(m, 2, 1);
            if op == IrBinOp::Ne {
                code.push_int(1, self.cw);
                code.ixor();
            }
            return;
        }
        self.emit_value(lhs, code);
        self.emit_value(rhs, code);
        if lt == Ty::Long {
            code.lcmp();
            code.push_int(0, self.cw); // compare lcmp-result against 0 with if_icmp*
        }
        let t = code.new_label();
        let end = code.new_label();
        self.frame(t, vec![], code); // at branch target the operands are consumed → empty stack
        match op {
            IrBinOp::Lt => code.if_icmplt(t),
            IrBinOp::Le => code.if_icmple(t),
            IrBinOp::Gt => code.if_icmpgt(t),
            IrBinOp::Ge => code.if_icmpge(t),
            IrBinOp::Eq => code.if_icmpeq(t),
            IrBinOp::Ne => code.if_icmpne(t),
            _ => unreachable!(),
        }
        code.push_int(0, self.cw);
        self.frame(end, vec![VerifType::Integer], code);
        code.goto(end);
        code.bind(t);
        code.push_int(1, self.cw);
        code.bind(end);
    }

    fn emit_when(&mut self, branches: &[(Option<u32>, u32)], code: &mut CodeBuilder) {
        let end = code.new_label();
        let result_stack = self.verif_stack(self.value_ty_of_when(branches));
        for (cond, body) in branches {
            match cond {
                Some(c) => {
                    self.emit_value(*c, code);
                    let next = code.new_label();
                    self.frame(next, vec![], code);
                    code.ifeq(next);
                    self.emit_value(*body, code);
                    self.frame(end, result_stack.clone(), code);
                    code.goto(end);
                    code.bind(next);
                }
                None => self.emit_value(*body, code),
            }
        }
        self.frame(end, result_stack, code);
        code.bind(end);
    }

    fn value_ty_of_when(&self, branches: &[(Option<u32>, u32)]) -> Ty {
        branches.last().map(|(_, b)| self.value_ty(*b)).unwrap_or(Ty::Unit)
    }

    fn frame(&mut self, label: Label, stack: Vec<VerifType>, code: &mut CodeBuilder) {
        let locals = self.verif_locals();
        code.add_frame_if_new(label, locals, stack);
    }

    fn verif_locals(&mut self) -> Vec<VerifType> {
        let max = self.next_slot as usize;
        let mut raw = vec![VerifType::Top; max];
        let entries: Vec<(u16, Ty)> = self.slots.values().copied().collect();
        for (slot, ty) in entries {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            out.push(raw[i].clone());
            i += if wide { 2 } else { 1 };
        }
        while out.last() == Some(&VerifType::Top) {
            out.pop();
        }
        out
    }

    fn verif_single(&mut self, ty: Ty) -> VerifType {
        match ty {
            Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => VerifType::Integer,
            Ty::Long => VerifType::Long,
            Ty::Double => VerifType::Double,
            Ty::Float => VerifType::Float,
            Ty::String => VerifType::Object(self.cw.class_ref("java/lang/String")),
            Ty::Obj(n) => VerifType::Object(self.cw.class_ref(n)),
            _ => VerifType::Top,
        }
    }

    fn verif_stack(&mut self, ty: Ty) -> Vec<VerifType> {
        match ty {
            Ty::Unit | Ty::Nothing | Ty::Error => vec![],
            _ => vec![self.verif_single(ty)],
        }
    }

    fn value_ty(&self, e: u32) -> Ty {
        match self.ir.expr(e) {
            IrExpr::Const(c) => match c {
                IrConst::Boolean(_) => Ty::Boolean,
                IrConst::Int(_) => Ty::Int,
                IrConst::Long(_) => Ty::Long,
                IrConst::String(_) => Ty::String,
                _ => Ty::Error,
            },
            IrExpr::GetValue(i) => self.slots.get(i).map(|(_, t)| *t).unwrap_or(Ty::Error),
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(fid) => ir_ty_to_jvm(&self.ir.functions[*fid as usize].ret),
                Callee::Intrinsic(fq) => intrinsic_ret(fq),
            },
            IrExpr::PrimitiveBinOp { op, lhs, .. } => match op {
                IrBinOp::Lt | IrBinOp::Le | IrBinOp::Gt | IrBinOp::Ge | IrBinOp::Eq | IrBinOp::Ne | IrBinOp::And | IrBinOp::Or => Ty::Boolean,
                _ => self.value_ty(*lhs),
            },
            IrExpr::When { branches } => self.value_ty_of_when(branches),
            _ => Ty::Error,
        }
    }
}

/// The result type of a stdlib intrinsic (shared by JVM/value-typing).
fn intrinsic_ret(fq: &str) -> Ty {
    match fq {
        "kotlin/String.plus" => Ty::String,
        _ => Ty::Error,
    }
}

/// Map a backend-agnostic `IrType` (Kotlin FqName) to a krusty `Ty` for JVM descriptors.
fn ir_ty_to_jvm(t: &IrType) -> Ty {
    match t {
        IrType::Unit => Ty::Unit,
        IrType::Nothing => Ty::Nothing,
        IrType::Class { fq_name, .. } => match fq_name.as_str() {
            "kotlin/Int" => Ty::Int,
            "kotlin/Long" => Ty::Long,
            "kotlin/Short" => Ty::Short,
            "kotlin/Byte" => Ty::Byte,
            "kotlin/Boolean" => Ty::Boolean,
            "kotlin/Char" => Ty::Char,
            "kotlin/Double" => Ty::Double,
            "kotlin/Float" => Ty::Float,
            "kotlin/String" => Ty::String,
            _ => Ty::obj(fq_name),
        },
        _ => Ty::Error,
    }
}

fn slot_words(t: Ty) -> u16 {
    match t {
        Ty::Long | Ty::Double => 2,
        Ty::Unit | Ty::Nothing => 0,
        _ => 1,
    }
}

fn load(t: Ty, slot: u16, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lload(slot),
        Ty::Double => code.dload(slot),
        Ty::Float => code.fload(slot),
        Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => code.iload(slot),
        _ => code.aload(slot),
    }
}

fn store(t: Ty, slot: u16, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lstore(slot),
        Ty::Double => code.dstore(slot),
        Ty::Float => code.fstore(slot),
        Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => code.istore(slot),
        _ => code.astore(slot),
    }
}

fn emit_return(t: Ty, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lreturn(),
        Ty::Double => code.dreturn(),
        Ty::Float => code.freturn(),
        Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => code.ireturn(),
        Ty::Unit | Ty::Nothing => code.ret_void(),
        _ => code.areturn(),
    }
}

fn discard(t: Ty, code: &mut CodeBuilder) {
    match slot_words(t) {
        2 => code.pop2(),
        1 => code.pop(),
        _ => {}
    }
}

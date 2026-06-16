//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of the backend-agnostic IR — it maps
//! Kotlin FqNames to JVM descriptors here (the IR never carries descriptors). Covers the core
//! subset (functions, simple classes); shares `CodeBuilder`/`ClassWriter` with the AST emitter.

use std::collections::HashMap;

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrType};
use crate::jvm::classfile::{ClassWriter, CodeBuilder, Label, VerifType};
use crate::jvm::emit::method_descriptor;
use crate::types::Ty;

/// Emit a whole IR file: the facade class of top-level `static` functions, plus one `.class` per
/// `IrClass`. Returns `(internal_name, bytes)` for each.
pub fn emit_all(ir: &IrFile, facade: &str) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    // Facade: the static top-level functions (those with no dispatch receiver).
    let mut cw = ClassWriter::new(facade, "java/lang/Object");
    for (i, f) in ir.functions.iter().enumerate() {
        if f.dispatch_receiver.is_some() || f.body.is_none() {
            continue;
        }
        emit_method(ir, i as u32, facade, facade, &mut cw, false);
    }
    out.push((facade.to_string(), cw.finish()));
    // Each class.
    for c in &ir.classes {
        out.push((c.fq_name.clone(), emit_class(ir, c, facade)));
    }
    out
}

/// Back-compat single-facade entry (used where a file has only functions).
pub fn emit_file(ir: &IrFile, facade: &str) -> Vec<u8> {
    let mut cw = ClassWriter::new(facade, "java/lang/Object");
    for (i, f) in ir.functions.iter().enumerate() {
        if f.dispatch_receiver.is_none() && f.body.is_some() {
            emit_method(ir, i as u32, facade, facade, &mut cw, false);
        }
    }
    cw.finish()
}

fn emit_class(ir: &IrFile, c: &crate::ir::IrClass, facade: &str) -> Vec<u8> {
    let mut cw = ClassWriter::new(&c.fq_name, "java/lang/Object");
    // Public fields (the IR slice reads them cross-class directly; kotlinc uses private + getters —
    // an ABI refinement, not a runtime difference).
    for (name, ty) in &c.fields {
        cw.add_field(0x0001 /* PUBLIC */, name, &ir_ty_to_jvm(ty).descriptor());
    }
    // Constructor: super(); store each ctor param into its field.
    let field_tys: Vec<Ty> = c.fields.iter().map(|(_, t)| ir_ty_to_jvm(t)).collect();
    let mut ctor = CodeBuilder::new(1 + field_tys.iter().map(|t| slot_words(*t)).sum::<u16>());
    ctor.aload(0);
    let obj_init = cw.methodref("java/lang/Object", "<init>", "()V");
    ctor.invokespecial(obj_init, 0, 0);
    let mut slot = 1u16;
    for ((name, _), t) in c.fields.iter().zip(&field_tys) {
        ctor.aload(0);
        load(*t, slot, &mut ctor);
        let fref = cw.fieldref(&c.fq_name, name, &t.descriptor());
        ctor.putfield(fref, slot_words(*t) as i32);
        slot += slot_words(*t);
    }
    ctor.ret_void();
    ctor.ensure_locals(slot);
    ctor.link();
    cw.add_method(0x0001, "<init>", &method_descriptor(&field_tys, Ty::Unit), &ctor);
    // Instance methods.
    for &fid in &c.methods {
        if ir.functions[fid as usize].body.is_some() {
            emit_method(ir, fid, &c.fq_name, facade, &mut cw, true);
        }
    }
    cw.finish()
}

/// Emit function `fid` as a method on `owner`. `instance` = an instance method (`this` in slot 0).
fn emit_method(ir: &IrFile, fid: u32, owner: &str, facade: &str, cw: &mut ClassWriter, instance: bool) {
    let f = &ir.functions[fid as usize];
    let body = f.body.unwrap();
    let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
    let ret = ir_ty_to_jvm(&f.ret);
    let mut e = Emitter { ir, cw, owner: owner.to_string(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 0, ret };
    if instance {
        e.slots.insert(0, (0, Ty::obj(owner)));
        e.next_slot = 1;
    }
    for (i, t) in param_tys.iter().enumerate() {
        let vi = i as u32 + if instance { 1 } else { 0 };
        let slot = e.next_slot;
        e.slots.insert(vi, (slot, *t));
        e.next_slot += slot_words(*t);
    }
    let mut code = CodeBuilder::new(e.next_slot);
    e.emit(body, &mut code);
    if ret == Ty::Unit {
        code.ret_void();
    }
    code.ensure_locals(e.next_slot);
    code.link();
    let access = if instance { 0x0001 } else { 0x0009 }; // PUBLIC | (STATIC)
    e.cw.add_method(access, &f.name, &method_descriptor(&param_tys, ret), &code);
}

struct Emitter<'a> {
    ir: &'a IrFile,
    cw: &'a mut ClassWriter,
    owner: String,
    facade: String,
    slots: HashMap<u32, (u16, Ty)>,
    next_slot: u16,
    ret: Ty,
}

impl<'a> Emitter<'a> {
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
                // Emit the initializer BEFORE allocating the slot, so the variable's slot isn't
                // claimed in StackMapTable frames recorded inside a branchy initializer (where the
                // verifier still sees it as `top`).
                let jt = ir_ty_to_jvm(&ty);
                if let Some(i) = init {
                    self.emit_value(i, code);
                    let slot = self.next_slot;
                    self.next_slot += slot_words(jt);
                    self.slots.insert(index, (slot, jt));
                    store(jt, slot, code);
                } else {
                    let slot = self.next_slot;
                    self.next_slot += slot_words(jt);
                    self.slots.insert(index, (slot, jt));
                }
            }
            IrExpr::SetValue { var, value } => {
                let (slot, jt) = self.slots[&var];
                self.emit_value(value, code);
                store(jt, slot, code);
            }
            IrExpr::While { cond, body } => {
                let start = code.new_label();
                let end = code.new_label();
                self.frame(start, vec![], code);
                code.bind(start);
                self.emit_value(cond, code);
                self.frame(end, vec![], code);
                code.ifeq(end);
                self.emit(body, code);
                self.frame(start, vec![], code);
                code.goto(start);
                self.frame(end, vec![], code);
                code.bind(end);
            }
            other => {
                self.emit_value_node(&other, code);
                discard(self.value_ty(e), code);
            }
        }
    }

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
                IrConst::Double(v) => code.push_double(*v, self.cw),
                IrConst::Float(v) => code.push_float(*v, self.cw),
                IrConst::String(s) => code.push_string(s, self.cw),
                IrConst::Null => {}
            },
            IrExpr::GetValue(i) => {
                let (slot, jt) = self.slots[i];
                load(jt, slot, code);
            }
            IrExpr::GetField { receiver, class, index } => {
                let c = &self.ir.classes[*class as usize];
                let (name, fty) = c.fields[*index as usize].clone();
                let jt = ir_ty_to_jvm(&fty);
                let owner = c.fq_name.clone();
                self.emit_value(*receiver, code);
                let fref = self.cw.fieldref(&owner, &name, &jt.descriptor());
                code.getfield(fref, slot_words(jt) as i32);
            }
            IrExpr::New { class, args } => {
                let c = &self.ir.classes[*class as usize];
                let owner = c.fq_name.clone();
                let field_tys: Vec<Ty> = c.fields.iter().map(|(_, t)| ir_ty_to_jvm(t)).collect();
                let args = args.clone();
                let ci = self.cw.class_ref(&owner);
                code.new_obj(ci);
                code.dup();
                for &a in &args {
                    self.emit_value(a, code);
                }
                let aw: i32 = field_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let m = self.cw.methodref(&owner, "<init>", &method_descriptor(&field_tys, Ty::Unit));
                code.invokespecial(m, aw, 0);
            }
            IrExpr::MethodCall { class, index, receiver, args } => {
                let c = &self.ir.classes[*class as usize];
                let fid = c.methods[*index as usize];
                let f = &self.ir.functions[fid as usize];
                let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
                let ret = ir_ty_to_jvm(&f.ret);
                let name = f.name.clone();
                let owner = c.fq_name.clone();
                let args = args.clone();
                self.emit_value(*receiver, code);
                for &a in &args {
                    self.emit_value(a, code);
                }
                let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let m = self.cw.methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                code.invokevirtual(m, aw, slot_words(ret) as i32);
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
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self.cw.methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Intrinsic(fq) => self.emit_intrinsic(fq, dispatch_receiver, args, code),
            },
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.emit_binop(*op, *lhs, *rhs, code),
            IrExpr::When { branches } => self.emit_when(branches, code),
            _ => {}
        }
    }

    fn emit_intrinsic(&mut self, fq: &str, recv: &Option<u32>, args: &[u32], code: &mut CodeBuilder) {
        match fq {
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
            // `s.length` → `String.length()`.
            "kotlin/String.length" => {
                self.emit_value(recv.unwrap(), code);
                let m = self.cw.methodref("java/lang/String", "length", "()I");
                code.invokevirtual(m, 0, 1);
            }
            // `x.toString()` → `String.valueOf(x)` (the right primitive/Object overload).
            "kotlin/Any.toString" => {
                let r = recv.unwrap();
                let ty = self.value_ty(r);
                self.emit_value(r, code);
                let desc = match ty {
                    Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/String;",
                    Ty::Long => "(J)Ljava/lang/String;",
                    Ty::Boolean => "(Z)Ljava/lang/String;",
                    Ty::Char => "(C)Ljava/lang/String;",
                    Ty::Double => "(D)Ljava/lang/String;",
                    Ty::Float => "(F)Ljava/lang/String;",
                    _ => "(Ljava/lang/Object;)Ljava/lang/String;",
                };
                let m = self.cw.methodref("java/lang/String", "valueOf", desc);
                code.invokestatic(m, slot_words(ty) as i32, 1);
            }
            _ => {}
        }
    }

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
        code.invokevirtual(m, slot_words(ty) as i32, 1);
    }

    fn emit_binop(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        match op {
            Add | Sub | Mul | Div | Rem => {
                self.emit_value(lhs, code);
                self.emit_value(rhs, code);
                match lt {
                    Ty::Long => match op { Add => code.ladd(), Sub => code.lsub(), Mul => code.lmul(), Div => code.ldiv(), Rem => code.lrem(), _ => unreachable!() },
                    Ty::Double => match op { Add => code.dadd(), Sub => code.dsub(), Mul => code.dmul(), Div => code.ddiv(), Rem => code.drem(), _ => unreachable!() },
                    Ty::Float => match op { Add => code.fadd(), Sub => code.fsub(), Mul => code.fmul(), Div => code.fdiv(), Rem => code.frem(), _ => unreachable!() },
                    _ => match op { Add => code.iadd(), Sub => code.isub(), Mul => code.imul(), Div => code.idiv(), Rem => code.irem(), _ => unreachable!() },
                }
            }
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
        // Long/Double/Float compare to a 3-way result, then test against 0 with `if_icmp*`.
        match lt {
            Ty::Long => { code.lcmp(); code.push_int(0, self.cw); }
            Ty::Double => { code.dcmpg(); code.push_int(0, self.cw); }
            Ty::Float => { code.fcmpg(); code.push_int(0, self.cw); }
            _ => {}
        }
        let t = code.new_label();
        let end = code.new_label();
        self.frame(t, vec![], code);
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
        let has_else = branches.iter().any(|(c, _)| c.is_none());
        if !has_else {
            // A `when` with no `else` is a *statement* (`Unit`): each matched branch runs for effect
            // (its value discarded), and no value reaches the operand stack.
            for (cond, body) in branches {
                if let Some(c) = cond {
                    self.emit_value(*c, code);
                    let next = code.new_label();
                    self.frame(next, vec![], code);
                    code.ifeq(next);
                    self.emit_value(*body, code);
                    discard(self.value_ty(*body), code);
                    self.frame(end, vec![], code);
                    code.goto(end);
                    code.bind(next);
                }
            }
            self.frame(end, vec![], code);
            code.bind(end);
            return;
        }
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
        // No `else` → the `when` is a Unit statement.
        if !branches.iter().any(|(c, _)| c.is_none()) {
            return Ty::Unit;
        }
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
                IrConst::Double(_) => Ty::Double,
                IrConst::Float(_) => Ty::Float,
                IrConst::Char(_) => Ty::Char,
                IrConst::String(_) => Ty::String,
                IrConst::Short(_) => Ty::Short,
                IrConst::Byte(_) => Ty::Byte,
                IrConst::Null => Ty::Error,
            },
            IrExpr::GetValue(i) => self.slots.get(i).map(|(_, t)| *t).unwrap_or(Ty::Error),
            IrExpr::GetField { class, index, .. } => ir_ty_to_jvm(&self.ir.classes[*class as usize].fields[*index as usize].1),
            IrExpr::New { class, .. } => Ty::obj(&self.ir.classes[*class as usize].fq_name),
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                ir_ty_to_jvm(&self.ir.functions[fid as usize].ret)
            }
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

fn intrinsic_ret(fq: &str) -> Ty {
    match fq {
        "kotlin/String.plus" | "kotlin/Any.toString" => Ty::String,
        "kotlin/String.length" => Ty::Int,
        _ => Ty::Error,
    }
}

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

//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of the backend-agnostic IR — it maps
//! Kotlin FqNames to JVM descriptors here (the IR never carries descriptors). Covers the core
//! subset (functions, simple classes); shares `CodeBuilder`/`ClassWriter` with the AST emitter.

use std::collections::HashMap;

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrType, IrTypeOp};
use crate::jvm::classfile::{ClassWriter, CodeBuilder, Label, VerifType};
use crate::jvm::names::method_descriptor;
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
    emit_statics(ir, facade, &mut cw);
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
    emit_statics(ir, facade, &mut cw);
    cw.finish()
}

/// Emit the facade's top-level properties as `public static` fields plus a `<clinit>` that runs
/// their initializers in declaration order.
fn emit_statics(ir: &IrFile, facade: &str, cw: &mut ClassWriter) {
    if ir.statics.is_empty() {
        return;
    }
    for s in &ir.statics {
        cw.add_field(0x0009 /* PUBLIC | STATIC */, &s.name, &ir_ty_to_jvm(&s.ty).descriptor());
    }
    let mut e = Emitter { ir, cw, owner: facade.to_string(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 0, ret: Ty::Unit };
    let mut code = CodeBuilder::new(0);
    for s in &ir.statics {
        e.emit_value(s.init, &mut code);
        let jt = ir_ty_to_jvm(&s.ty);
        let fref = e.cw.fieldref(facade, &s.name, &jt.descriptor());
        code.putstatic(fref, slot_words(jt) as i32);
    }
    code.ret_void();
    code.ensure_locals(e.next_slot);
    code.link();
    e.cw.add_method(0x0008 /* STATIC */, "<clinit>", "()V", &code);
}

fn emit_class(ir: &IrFile, c: &crate::ir::IrClass, facade: &str) -> Vec<u8> {
    let mut cw = ClassWriter::new(&c.fq_name, "java/lang/Object");
    // Public fields (the IR slice reads them cross-class directly; kotlinc uses private + getters —
    // an ABI refinement, not a runtime difference).
    for (name, ty) in &c.fields {
        cw.add_field(0x0001 /* PUBLIC */, name, &ir_ty_to_jvm(ty).descriptor());
    }
    // Constructor: super(); store each ctor *parameter* into its field; then run `init_body`
    // (body-property initializers + `init {}` blocks). Fields past `ctor_param_count` are body
    // properties — not parameters — so the descriptor covers only the leading parameter fields.
    let field_tys: Vec<Ty> = c.fields.iter().map(|(_, t)| ir_ty_to_jvm(t)).collect();
    let n_params = c.ctor_param_count as usize;
    let param_tys: Vec<Ty> = field_tys[..n_params].to_vec();
    let params_words: u16 = param_tys.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + params_words);
    ctor.aload(0);
    let obj_init = cw.methodref("java/lang/Object", "<init>", "()V");
    ctor.invokespecial(obj_init, 0, 0);
    let mut slot = 1u16;
    for ((name, _), t) in c.fields[..n_params].iter().zip(&param_tys) {
        ctor.aload(0);
        load(*t, slot, &mut ctor);
        let fref = cw.fieldref(&c.fq_name, name, &t.descriptor());
        ctor.putfield(fref, slot_words(*t) as i32);
        slot += slot_words(*t);
    }
    let mut max_slot = slot;
    if let Some(init_body) = c.init_body {
        // Emit the init block with `this` = value 0 and the ctor params as values 1..=N, reusing the
        // method emitter's value→slot machinery (the params already occupy slots 1..`slot`).
        let mut e = Emitter { ir, cw: &mut cw, owner: c.fq_name.clone(), facade: facade.to_string(), slots: HashMap::new(), next_slot: slot, ret: Ty::Unit };
        e.slots.insert(0, (0, Ty::obj(&c.fq_name)));
        let mut s = 1u16;
        for (vi, t) in param_tys.iter().enumerate() {
            e.slots.insert(vi as u32 + 1, (s, *t));
            s += slot_words(*t);
        }
        e.emit(init_body, &mut ctor);
        max_slot = e.next_slot;
    }
    ctor.ret_void();
    ctor.ensure_locals(max_slot);
    ctor.link();
    cw.add_method(0x0001, "<init>", &method_descriptor(&param_tys, Ty::Unit), &ctor);
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
                // Scope block-locals: restore the slot *map* after the block (keeping next_slot
                // monotonic) so a local declared here doesn't leak into a later merge-point frame
                // (its slot must read as `Top` once out of scope — else a sibling branch that never
                // initialized it fails verification).
                let saved = self.slots.clone();
                let mut dead = false;
                for s in stmts {
                    self.emit(s, code);
                    if self.diverges(s) { dead = true; break; } // rest of the block is unreachable
                }
                if !dead {
                    if let Some(v) = value {
                        self.emit_value(v, code);
                        discard(self.value_ty(v), code);
                    }
                }
                self.slots = saved;
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
            IrExpr::SetField { receiver, class, index, value } => {
                let c = &self.ir.classes[class as usize];
                let (name, fty) = c.fields[index as usize].clone();
                let jt = ir_ty_to_jvm(&fty);
                let owner = c.fq_name.clone();
                self.emit_value(receiver, code);
                self.emit_value(value, code);
                let fref = self.cw.fieldref(&owner, &name, &jt.descriptor());
                code.putfield(fref, slot_words(jt) as i32);
            }
            IrExpr::SetStatic { index, value } => {
                let s = &self.ir.statics[index as usize];
                let jt = ir_ty_to_jvm(&s.ty);
                let name = s.name.clone();
                let facade = self.facade.clone();
                self.emit_value(value, code);
                let fref = self.cw.fieldref(&facade, &name, &jt.descriptor());
                code.putstatic(fref, slot_words(jt) as i32);
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
                IrConst::Null => code.aconst_null(),
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
            IrExpr::GetStatic(i) => {
                let s = &self.ir.statics[*i as usize];
                let jt = ir_ty_to_jvm(&s.ty);
                let name = s.name.clone();
                let facade = self.facade.clone();
                let fref = self.cw.fieldref(&facade, &name, &jt.descriptor());
                code.getstatic(fref, slot_words(jt) as i32);
            }
            IrExpr::New { class, args } => {
                let c = &self.ir.classes[*class as usize];
                let owner = c.fq_name.clone();
                // The constructor takes only the parameter fields; body properties are set inside it.
                let field_tys: Vec<Ty> = c.fields[..c.ctor_param_count as usize].iter().map(|(_, t)| ir_ty_to_jvm(t)).collect();
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
                Callee::External(fq) => self.emit_intrinsic(fq, dispatch_receiver, args, code),
            },
            IrExpr::TypeOp { op, arg, type_operand } => {
                let internal = ref_internal(ir_ty_to_jvm(type_operand));
                self.emit_value(*arg, code);
                match op {
                    IrTypeOp::InstanceOf => {
                        let ci = self.cw.class_ref(&internal);
                        code.instance_of(ci);
                    }
                    IrTypeOp::NotInstanceOf => {
                        let ci = self.cw.class_ref(&internal);
                        code.instance_of(ci);
                        code.push_int(1, self.cw);
                        code.ixor();
                    }
                    IrTypeOp::Cast => {
                        let ci = self.cw.class_ref(&internal);
                        code.checkcast(ci);
                    }
                    // Box a primitive into a reference target, or unbox a wrapper into a primitive.
                    IrTypeOp::ImplicitCoercion => {
                        let at = self.value_ty(*arg);
                        let target = ir_ty_to_jvm(type_operand);
                        if at.is_primitive() && target.is_reference() {
                            self.box_prim(at, code);
                        } else if at.is_reference() && target.is_primitive() {
                            self.unbox_to(target, code);
                        }
                    }
                    IrTypeOp::SafeCast => {}
                }
            }
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.emit_binop(*op, *lhs, *rhs, code),
            IrExpr::When { branches } => self.emit_when(branches, code),
            // Block in value position: run its statements for effect, leave the trailing value on the
            // stack. Scope block-locals (restore the slot map) so they don't leak into outer frames.
            IrExpr::Block { stmts, value } => {
                let saved = self.slots.clone();
                let mut dead = false;
                for s in stmts {
                    self.emit(*s, code);
                    if self.diverges(*s) { dead = true; break; }
                }
                if !dead {
                    if let Some(v) = value {
                        self.emit_value(*v, code);
                    }
                }
                self.slots = saved;
            }
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
            // `s[i]` → `String.charAt(i)`.
            "kotlin/String.get" => {
                self.emit_value(recv.unwrap(), code);
                self.emit_value(args[0], code);
                let m = self.cw.methodref("java/lang/String", "charAt", "(I)C");
                code.invokevirtual(m, 1, 1);
            }
            // Array operations: the JVM platform realizes them with native array instructions; the
            // element type comes from the receiver's IR type (`kotlin/Array.get/set/size`) or from
            // the per-element constructor name (`kotlin/IntArray.<init>`).
            "kotlin/Array.get" => {
                let arr = recv.unwrap();
                let elem = self.array_elem(arr);
                self.emit_value(arr, code);
                self.emit_value(args[0], code);
                let (op, w) = array_load_op(elem);
                code.array_load(op, w);
            }
            "kotlin/Array.set" => {
                let arr = recv.unwrap();
                let elem = self.array_elem(arr);
                self.emit_value(arr, code);
                self.emit_value(args[0], code);
                self.emit_value(args[1], code);
                let (op, w) = array_store_op(elem);
                code.array_store(op, w);
            }
            "kotlin/Array.size" => {
                self.emit_value(recv.unwrap(), code);
                code.arraylength();
            }
            _ if fq.ends_with("Array.<init>") => {
                self.emit_value(args[0], code);
                let elem = prim_array_atype(fq);
                code.newarray(elem);
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
        // Kotlin `==`/`!=` on reference operands is structural (`a?.equals(b)`), realized by the
        // null-safe `Objects.equals`. Primitives keep the `if_icmp*`/3-way-compare path below.
        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) && lt.is_reference() && self.value_ty(rhs).is_reference() {
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
        // A `when` with no `else`, or one whose value is `Unit`, is a statement: branch values are
        // discarded and nothing reaches the operand stack at `end`.
        let is_stmt = !has_else || self.value_ty_of_when(branches) == Ty::Unit;
        let result_stack = if is_stmt { vec![] } else { self.verif_stack(self.value_ty_of_when(branches)) };
        // `end` is reachable if any branch falls through to it (i.e. doesn't return/throw). A
        // no-`else` statement always has the implicit no-match fallthrough.
        let mut end_reachable = !has_else;
        for (cond, body) in branches {
            match cond {
                Some(c) => {
                    self.emit_value(*c, code);
                    let next = code.new_label();
                    self.frame(next, vec![], code);
                    code.ifeq(next);
                    self.emit_value(*body, code);
                    if is_stmt { discard(self.value_ty(*body), code); }
                    if !self.diverges(*body) {
                        // Only a falling-through branch jumps to (and needs a frame at) `end`.
                        self.frame(end, result_stack.clone(), code);
                        code.goto(end);
                        end_reachable = true;
                    }
                    code.bind(next);
                }
                None => {
                    self.emit_value(*body, code);
                    if is_stmt { discard(self.value_ty(*body), code); }
                    if !self.diverges(*body) { end_reachable = true; }
                    // The else is last — it falls through to `end` (no goto needed).
                }
            }
        }
        // Frame `end` only when it's actually reachable; if every branch diverges, `end` is dead
        // (no jump targets it) and a frame there would be "Expecting a stack map frame".
        if end_reachable {
            self.frame(end, result_stack, code);
        }
        code.bind(end);
    }

    /// Whether emitting `e` as a value always transfers control away (returns/throws), so control
    /// never falls through past it. Used to suppress dead `goto`s and unreachable merge frames.
    fn diverges(&self, e: u32) -> bool {
        match self.ir.expr(e) {
            IrExpr::Return(_) => true,
            IrExpr::Block { stmts, value } => match value {
                Some(v) => self.diverges(*v),
                None => stmts.last().map_or(false, |s| self.diverges(*s)),
            },
            IrExpr::When { branches } => {
                branches.iter().any(|(c, _)| c.is_none())
                    && branches.iter().all(|(_, b)| self.diverges(*b))
            }
            _ => false,
        }
    }

    /// Box a primitive value already on the stack to its wrapper (`Integer.valueOf`, …).
    fn box_prim(&mut self, t: Ty, code: &mut CodeBuilder) {
        let (cls, desc) = match t {
            Ty::Int => ("java/lang/Integer", "(I)Ljava/lang/Integer;"),
            Ty::Long => ("java/lang/Long", "(J)Ljava/lang/Long;"),
            Ty::Double => ("java/lang/Double", "(D)Ljava/lang/Double;"),
            Ty::Float => ("java/lang/Float", "(F)Ljava/lang/Float;"),
            Ty::Boolean => ("java/lang/Boolean", "(Z)Ljava/lang/Boolean;"),
            Ty::Char => ("java/lang/Character", "(C)Ljava/lang/Character;"),
            Ty::Byte => ("java/lang/Byte", "(B)Ljava/lang/Byte;"),
            Ty::Short => ("java/lang/Short", "(S)Ljava/lang/Short;"),
            _ => return,
        };
        let m = self.cw.methodref(cls, "valueOf", desc);
        code.invokestatic(m, slot_words(t) as i32, 1);
    }

    /// Unbox a wrapper on the stack to the primitive `t` (`checkcast` + `intValue`, …).
    fn unbox_to(&mut self, t: Ty, code: &mut CodeBuilder) {
        let (cls, meth, desc) = match t {
            Ty::Int => ("java/lang/Integer", "intValue", "()I"),
            Ty::Long => ("java/lang/Long", "longValue", "()J"),
            Ty::Double => ("java/lang/Double", "doubleValue", "()D"),
            Ty::Float => ("java/lang/Float", "floatValue", "()F"),
            Ty::Boolean => ("java/lang/Boolean", "booleanValue", "()Z"),
            Ty::Char => ("java/lang/Character", "charValue", "()C"),
            Ty::Byte => ("java/lang/Byte", "byteValue", "()B"),
            Ty::Short => ("java/lang/Short", "shortValue", "()S"),
            _ => return,
        };
        let ci = self.cw.class_ref(cls);
        code.checkcast(ci);
        let m = self.cw.methodref(cls, meth, desc);
        code.invokevirtual(m, 0, slot_words(t) as i32);
    }

    /// The element `Ty` of an array-typed IR expression.
    fn array_elem(&self, e: u32) -> Ty {
        self.value_ty(e).array_elem().unwrap_or(Ty::Error)
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
            // An array's verification type is an `Object` whose class name is its descriptor (`[I`).
            Ty::Array(_) => VerifType::Object(self.cw.class_ref(&ty.descriptor())),
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
                IrConst::Null => Ty::Null,
            },
            IrExpr::GetValue(i) => self.slots.get(i).map(|(_, t)| *t).unwrap_or(Ty::Error),
            IrExpr::GetField { class, index, .. } => ir_ty_to_jvm(&self.ir.classes[*class as usize].fields[*index as usize].1),
            IrExpr::GetStatic(i) => ir_ty_to_jvm(&self.ir.statics[*i as usize].ty),
            IrExpr::New { class, .. } => Ty::obj(&self.ir.classes[*class as usize].fq_name),
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                ir_ty_to_jvm(&self.ir.functions[fid as usize].ret)
            }
            IrExpr::Call { callee, dispatch_receiver, .. } => match callee {
                Callee::Local(fid) => ir_ty_to_jvm(&self.ir.functions[*fid as usize].ret),
                // Array `get` returns the receiver's element; an array `<init>` returns the array type.
                Callee::External(fq) if fq == "kotlin/Array.get" => dispatch_receiver.map(|r| self.array_elem(r)).unwrap_or(Ty::Error),
                Callee::External(fq) if fq.ends_with("Array.<init>") => Ty::array(prim_array_elem_ty(fq)),
                Callee::External(fq) => intrinsic_ret(fq),
            },
            IrExpr::PrimitiveBinOp { op, lhs, .. } => match op {
                IrBinOp::Lt | IrBinOp::Le | IrBinOp::Gt | IrBinOp::Ge | IrBinOp::Eq | IrBinOp::Ne | IrBinOp::And | IrBinOp::Or => Ty::Boolean,
                _ => self.value_ty(*lhs),
            },
            IrExpr::When { branches } => self.value_ty_of_when(branches),
            IrExpr::Block { value, .. } => value.map(|v| self.value_ty(v)).unwrap_or(Ty::Unit),
            IrExpr::TypeOp { op, type_operand, .. } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                _ => ir_ty_to_jvm(type_operand),
            },
            _ => Ty::Error,
        }
    }
}

/// JVM internal name for a reference `Ty`, for `instanceof`/`checkcast`.
fn ref_internal(t: Ty) -> String {
    match t {
        Ty::String => "java/lang/String".to_string(),
        Ty::Obj(n) => n.to_string(),
        Ty::Array(_) => t.descriptor(),
        _ => "java/lang/Object".to_string(),
    }
}

fn intrinsic_ret(fq: &str) -> Ty {
    match fq {
        "kotlin/String.plus" | "kotlin/Any.toString" => Ty::String,
        "kotlin/String.length" | "kotlin/Array.size" => Ty::Int,
        "kotlin/String.get" => Ty::Char,
        "kotlin/Array.set" => Ty::Unit,
        _ => Ty::Error,
    }
}

/// `newarray` atype for a `kotlin/<Prim>Array.<init>` intrinsic.
fn prim_array_atype(fq: &str) -> u8 {
    match prim_array_elem_ty(fq) {
        Ty::Boolean => 4,
        Ty::Char => 5,
        Ty::Float => 6,
        Ty::Double => 7,
        Ty::Byte => 8,
        Ty::Short => 9,
        Ty::Int => 10,
        Ty::Long => 11,
        _ => 10,
    }
}

/// Element `Ty` for a `kotlin/<Prim>Array.<init>` intrinsic FqName.
fn prim_array_elem_ty(fq: &str) -> Ty {
    let cls = fq.trim_start_matches("kotlin/").trim_end_matches(".<init>");
    match cls {
        "IntArray" => Ty::Int,
        "LongArray" => Ty::Long,
        "DoubleArray" => Ty::Double,
        "FloatArray" => Ty::Float,
        "BooleanArray" => Ty::Boolean,
        "CharArray" => Ty::Char,
        "ByteArray" => Ty::Byte,
        "ShortArray" => Ty::Short,
        _ => Ty::Int,
    }
}

/// `(opcode, value-words)` for an array element load (`Xaload`).
fn array_load_op(elem: Ty) -> (u8, i32) {
    match elem {
        Ty::Int => (0x2e, 1), Ty::Long => (0x2f, 2), Ty::Float => (0x30, 1), Ty::Double => (0x31, 2),
        Ty::Boolean | Ty::Byte => (0x33, 1), Ty::Char => (0x34, 1), Ty::Short => (0x35, 1),
        _ => (0x32, 1), // aaload
    }
}

/// `(opcode, value-words)` for an array element store (`Xastore`).
fn array_store_op(elem: Ty) -> (u8, i32) {
    match elem {
        Ty::Int => (0x4f, 1), Ty::Long => (0x50, 2), Ty::Float => (0x51, 1), Ty::Double => (0x52, 2),
        Ty::Boolean | Ty::Byte => (0x54, 1), Ty::Char => (0x55, 1), Ty::Short => (0x56, 1),
        _ => (0x53, 1), // aastore
    }
}

fn ir_ty_to_jvm(t: &IrType) -> Ty {
    match t {
        IrType::Unit => Ty::Unit,
        IrType::Nothing => Ty::Nothing,
        IrType::Class { fq_name, type_args, .. } => match fq_name.as_str() {
            "kotlin/Int" => Ty::Int,
            "kotlin/Long" => Ty::Long,
            "kotlin/Short" => Ty::Short,
            "kotlin/Byte" => Ty::Byte,
            "kotlin/Boolean" => Ty::Boolean,
            "kotlin/Char" => Ty::Char,
            "kotlin/Double" => Ty::Double,
            "kotlin/Float" => Ty::Float,
            "kotlin/String" => Ty::String,
            // Arrays are regular class types the JVM backend lowers to JVM array types here.
            "kotlin/IntArray" => Ty::array(Ty::Int),
            "kotlin/LongArray" => Ty::array(Ty::Long),
            "kotlin/DoubleArray" => Ty::array(Ty::Double),
            "kotlin/FloatArray" => Ty::array(Ty::Float),
            "kotlin/BooleanArray" => Ty::array(Ty::Boolean),
            "kotlin/CharArray" => Ty::array(Ty::Char),
            "kotlin/ByteArray" => Ty::array(Ty::Byte),
            "kotlin/ShortArray" => Ty::array(Ty::Short),
            "kotlin/Array" => Ty::array(type_args.first().map(ir_ty_to_jvm).unwrap_or(Ty::obj("java/lang/Object"))),
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

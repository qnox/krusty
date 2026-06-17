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
    if !c.enum_entries.is_empty() {
        return emit_enum_class(ir, c, facade);
    }
    if c.is_interface {
        return emit_interface_class(ir, c);
    }
    let mut cw = ClassWriter::new(&c.fq_name, &c.superclass);
    // Access: an extended or abstract class must not be `final`; a class with an abstract method
    // (body `None`) is `ACC_ABSTRACT`.
    let extended = ir.classes.iter().any(|o| o.superclass == c.fq_name);
    let has_abstract = c.methods.iter().any(|&fid| ir.functions[fid as usize].body.is_none());
    let mut access = 0x0001 | 0x0020; // PUBLIC | SUPER
    if !extended && !has_abstract { access |= 0x0010; } // FINAL
    if has_abstract { access |= 0x0400; } // ABSTRACT
    cw.set_access(access);
    for itf in &c.interfaces {
        cw.add_interface(itf);
    }
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
    // The superclass constructor's parameter types (empty for `java/lang/Object`).
    let super_param_tys: Vec<Ty> = if c.superclass == "java/lang/Object" {
        Vec::new()
    } else {
        ir.classes.iter().find(|sc| sc.fq_name == c.superclass)
            .map(|sc| sc.fields[..sc.ctor_param_count as usize].iter().map(|(_, t)| ir_ty_to_jvm(t)).collect())
            .unwrap_or_default()
    };
    let max_slot;
    {
        let mut e = Emitter { ir, cw: &mut cw, owner: c.fq_name.clone(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 1 + params_words, ret: Ty::Unit };
        e.slots.insert(0, (0, Ty::obj(&c.fq_name)));
        let mut s = 1u16;
        for (vi, t) in param_tys.iter().enumerate() {
            e.slots.insert(vi as u32 + 1, (s, *t));
            s += slot_words(*t);
        }
        // `super(args)` — `this` is loaded first, so spill any branchy arg to temps before it.
        let super_args = c.super_args.clone();
        if super_args.iter().any(|&a| e.records_frame(a)) {
            let temps = e.spill_to_temps(&super_args, &mut ctor);
            ctor.aload(0);
            for &(slot, t, _) in &temps { load(t, slot, &mut ctor); }
            for &(_, _, key) in &temps { e.slots.remove(&key); }
        } else {
            ctor.aload(0);
            for &a in &super_args { e.emit_value(a, &mut ctor); }
        }
        let aw: i32 = super_param_tys.iter().map(|t| slot_words(*t) as i32).sum();
        let super_init = e.cw.methodref(&c.superclass, "<init>", &method_descriptor(&super_param_tys, Ty::Unit));
        ctor.invokespecial(super_init, aw, 0);
        // Store this class's own primary-constructor parameter fields.
        let mut slot = 1u16;
        for ((name, _), t) in c.fields[..n_params].iter().zip(&param_tys) {
            ctor.aload(0);
            load(*t, slot, &mut ctor);
            let fref = e.cw.fieldref(&c.fq_name, name, &t.descriptor());
            ctor.putfield(fref, slot_words(*t) as i32);
            slot += slot_words(*t);
        }
        if let Some(init_body) = c.init_body {
            e.emit(init_body, &mut ctor);
        }
        max_slot = e.next_slot;
    }
    ctor.ret_void();
    ctor.ensure_locals(max_slot);
    ctor.link();
    // An `object`'s constructor is private; a normal class's is public.
    let ctor_access = if c.is_object { 0x0002 } else { 0x0001 };
    cw.add_method(ctor_access, "<init>", &method_descriptor(&param_tys, Ty::Unit), &ctor);
    // A singleton `object`: a `public static final INSTANCE` built in `<clinit>`.
    if c.is_object {
        let self_desc = format!("L{};", c.fq_name);
        cw.add_field(0x0019, "INSTANCE", &self_desc); // PUBLIC | STATIC | FINAL
        let mut clinit = CodeBuilder::new(0);
        let ci = cw.class_ref(&c.fq_name);
        clinit.new_obj(ci);
        clinit.dup();
        let init = cw.methodref(&c.fq_name, "<init>", "()V");
        clinit.invokespecial(init, 0, 0);
        let fref = cw.fieldref(&c.fq_name, "INSTANCE", &self_desc);
        clinit.putstatic(fref, 1);
        clinit.ret_void();
        clinit.ensure_locals(0);
        clinit.link();
        cw.add_method(0x0008, "<clinit>", "()V", &clinit);
    }
    // Instance methods (concrete emitted; abstract declared with `ACC_ABSTRACT`, no Code).
    for &fid in &c.methods {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            emit_method(ir, fid, &c.fq_name, facade, &mut cw, true);
        } else {
            let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
            let ret = ir_ty_to_jvm(&f.ret);
            cw.add_abstract_method(0x0001 | 0x0400, &f.name, &method_descriptor(&param_tys, ret));
        }
    }
    emit_bridges(c, &mut cw);
    cw.finish()
}

/// Emit `ACC_BRIDGE|ACC_SYNTHETIC` methods: each has the supertype's erased descriptor, adapts its
/// arguments (checkcast / unbox / numeric convert), delegates to the concrete override, and adapts
/// the return value back (box / numeric convert). Bridges are straight-line — no frames.
fn emit_bridges(c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    for b in &c.bridges {
        let ep: Vec<Ty> = b.erased_params.iter().map(ir_ty_to_jvm).collect();
        let cp: Vec<Ty> = b.concrete_params.iter().map(ir_ty_to_jvm).collect();
        let er = ir_ty_to_jvm(&b.erased_ret);
        let cr = ir_ty_to_jvm(&b.concrete_ret);
        let pw: u16 = ep.iter().map(|t| slot_words(*t)).sum();
        let mut code = CodeBuilder::new(1 + pw);
        code.aload(0);
        let mut slot = 1u16;
        for (et, ct) in ep.iter().zip(&cp) {
            load(*et, slot, &mut code);
            slot += slot_words(*et);
            if et != ct {
                if et.is_reference() && ct.is_reference() {
                    let ci = cw.class_ref(&ref_internal(*ct));
                    code.checkcast(ci);
                } else if et.is_reference() && ct.is_primitive() {
                    unbox_prim(cw, &mut code, *ct);
                } else if et.is_primitive() && ct.is_primitive() {
                    emit_num_conv(*et, *ct, &mut code);
                }
            }
        }
        let argw: i32 = cp.iter().map(|t| slot_words(*t) as i32).sum();
        let m = cw.methodref(&c.fq_name, &b.name, &method_descriptor(&cp, cr));
        code.invokevirtual(m, argw, slot_words(cr) as i32);
        if cr != er {
            if er.is_reference() && cr.is_primitive() {
                box_prim_free(cw, &mut code, cr);
            } else if er.is_primitive() && cr.is_primitive() {
                emit_num_conv(cr, er, &mut code);
            } // reference→reference: concrete return is a subtype of erased — no cast needed
        }
        emit_return(er, &mut code);
        code.ensure_locals(1 + pw);
        code.link();
        cw.add_method(0x0001 | 0x0040 | 0x1000, &b.name, &method_descriptor(&ep, er), &code);
    }
}

/// Box a primitive on the stack to its wrapper (free-function form for the bridge emitter).
fn box_prim_free(cw: &mut ClassWriter, code: &mut CodeBuilder, t: Ty) {
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
    let m = cw.methodref(cls, "valueOf", desc);
    code.invokestatic(m, slot_words(t) as i32, 1);
}

/// Unbox a wrapper on the stack to the primitive `t` (free-function form for the bridge emitter).
fn unbox_prim(cw: &mut ClassWriter, code: &mut CodeBuilder, t: Ty) {
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
    let ci = cw.class_ref(cls);
    code.checkcast(ci);
    let m = cw.methodref(cls, meth, desc);
    code.invokevirtual(m, 0, slot_words(t) as i32);
}

/// Emit an `interface`: `ACC_PUBLIC|ACC_INTERFACE|ACC_ABSTRACT`, extends `java/lang/Object`, with one
/// `public abstract` method per declared (abstract) method and no fields/constructor.
fn emit_interface_class(ir: &IrFile, c: &crate::ir::IrClass) -> Vec<u8> {
    let mut cw = ClassWriter::new(&c.fq_name, "java/lang/Object");
    cw.set_access(0x0001 | 0x0200 | 0x0400); // PUBLIC | INTERFACE | ABSTRACT
    for itf in &c.interfaces {
        cw.add_interface(itf);
    }
    for &fid in &c.methods {
        let f = &ir.functions[fid as usize];
        let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
        let ret = ir_ty_to_jvm(&f.ret);
        cw.add_abstract_method(0x0001 | 0x0400, &f.name, &method_descriptor(&param_tys, ret)); // PUBLIC | ABSTRACT
    }
    cw.finish()
}

/// Emit an `enum class`: extends `java/lang/Enum`, a private `(String name, int ordinal, …)` ctor →
/// `super(name, ordinal)`, a `public static final` field per entry plus a `$VALUES` array, a
/// `<clinit>` that constructs the entries and fills `$VALUES`, and synthetic `values()`/`valueOf`.
fn emit_enum_class(ir: &IrFile, c: &crate::ir::IrClass, facade: &str) -> Vec<u8> {
    const ACC_ENUM: u16 = 0x4000;
    const ACC_SYNTHETIC: u16 = 0x1000;
    let fq = c.fq_name.clone();
    let self_desc = format!("L{fq};");
    let arr_desc = format!("[{self_desc}");
    let mut cw = ClassWriter::new(&fq, "java/lang/Enum");
    cw.set_access(0x0001 | 0x0010 | 0x0020 | ACC_ENUM); // PUBLIC | FINAL | SUPER | ENUM

    let field_tys: Vec<Ty> = c.fields.iter().map(|(_, t)| ir_ty_to_jvm(t)).collect();
    let n_params = c.ctor_param_count as usize;
    let user_tys: Vec<Ty> = field_tys[..n_params].to_vec();
    // User (primary-constructor) fields — public so the IR's direct cross-class reads work.
    for ((name, _), t) in c.fields[..n_params].iter().zip(&user_tys) {
        cw.add_field(0x0001, name, &t.descriptor());
    }
    // One static-final constant per entry, plus the private `$VALUES` array.
    for (entry, _) in &c.enum_entries {
        cw.add_field(0x0001 | 0x0008 | 0x0010 | ACC_ENUM, entry, &self_desc);
    }
    cw.add_field(0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC, "$VALUES", &arr_desc);

    // Private constructor: `(Ljava/lang/String;I<user>)V` → `super(name, ordinal)` + store user fields.
    let ctor_params: Vec<Ty> = std::iter::once(Ty::String).chain(std::iter::once(Ty::Int)).chain(user_tys.iter().copied()).collect();
    let ctor_desc = method_descriptor(&ctor_params, Ty::Unit);
    let ctor_words: u16 = ctor_params.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + ctor_words);
    ctor.aload(0);
    ctor.aload(1);
    load(Ty::Int, 2, &mut ctor);
    let super_init = cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
    ctor.invokespecial(super_init, 2, 0);
    let mut slot = 3u16;
    for ((name, _), t) in c.fields[..n_params].iter().zip(&user_tys) {
        ctor.aload(0);
        load(*t, slot, &mut ctor);
        let fref = cw.fieldref(&fq, name, &t.descriptor());
        ctor.putfield(fref, slot_words(*t) as i32);
        slot += slot_words(*t);
    }
    ctor.ret_void();
    ctor.ensure_locals(1 + ctor_words);
    ctor.link();
    cw.add_method(0x0002 | ACC_SYNTHETIC, "<init>", &ctor_desc, &ctor);

    // <clinit>: construct each entry, then build `$VALUES`.
    let ctor_argw: i32 = ctor_params.iter().map(|t| slot_words(*t) as i32).sum();
    {
        let mut e = Emitter { ir, cw: &mut cw, owner: fq.clone(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 0, ret: Ty::Unit };
        let mut clinit = CodeBuilder::new(0);
        for (i, (entry, args)) in c.enum_entries.iter().enumerate() {
            // A branchy entry arg (`X(1 == 1)`) must run on a clean stack — spill all args to temps
            // first, then construct (mirrors the `New` node's spill).
            let spill = args.iter().any(|&a| e.records_frame(a));
            let temps = if spill { e.spill_to_temps(args, &mut clinit) } else { Vec::new() };
            let cls = e.cw.class_ref(&fq);
            clinit.new_obj(cls);
            clinit.dup();
            clinit.push_string(entry, e.cw);
            clinit.push_int(i as i32, e.cw);
            if spill {
                for &(slot, t, _) in &temps { load(t, slot, &mut clinit); }
                for &(_, _, key) in &temps { e.slots.remove(&key); }
            } else {
                for &a in args {
                    e.emit_value(a, &mut clinit);
                }
            }
            let ctor_ref = e.cw.methodref(&fq, "<init>", &ctor_desc);
            clinit.invokespecial(ctor_ref, ctor_argw, 0);
            let fref = e.cw.fieldref(&fq, entry, &self_desc);
            clinit.putstatic(fref, 1);
        }
        clinit.push_int(c.enum_entries.len() as i32, e.cw);
        let acls = e.cw.class_ref(&fq);
        clinit.anewarray(acls);
        for (i, (entry, _)) in c.enum_entries.iter().enumerate() {
            clinit.dup();
            clinit.push_int(i as i32, e.cw);
            let fref = e.cw.fieldref(&fq, entry, &self_desc);
            clinit.getstatic(fref, 1);
            clinit.array_store(0x53, 1); // aastore
        }
        let valref = e.cw.fieldref(&fq, "$VALUES", &arr_desc);
        clinit.putstatic(valref, 1);
        clinit.ret_void();
        clinit.ensure_locals(e.next_slot.max(4));
        clinit.link();
        e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
    }

    // values(): `$VALUES.clone()` cast back to the array type.
    let mut vals = CodeBuilder::new(0);
    let valref = cw.fieldref(&fq, "$VALUES", &arr_desc);
    vals.getstatic(valref, 1);
    let clone_m = cw.methodref(&arr_desc, "clone", "()Ljava/lang/Object;");
    vals.invokevirtual(clone_m, 0, 1);
    let arr_cls = cw.class_ref(&arr_desc);
    vals.checkcast(arr_cls);
    vals.areturn();
    vals.ensure_locals(0);
    vals.link();
    cw.add_method(0x0009, "values", &format!("(){arr_desc}"), &vals);

    // valueOf(String): `Enum.valueOf(E.class, name)` cast to E.
    let mut vof = CodeBuilder::new(1);
    vof.ldc_class(&fq, &mut cw);
    vof.aload(0);
    let veo = cw.methodref("java/lang/Enum", "valueOf", "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;");
    vof.invokestatic(veo, 2, 1);
    let cc = cw.class_ref(&fq);
    vof.checkcast(cc);
    vof.areturn();
    vof.ensure_locals(1);
    vof.link();
    cw.add_method(0x0009, "valueOf", &format!("(Ljava/lang/String;){self_desc}"), &vof);

    for &fid in &c.methods {
        if ir.functions[fid as usize].body.is_some() {
            emit_method(ir, fid, &fq, facade, &mut cw, true);
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
                let aw: i32 = field_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let desc = method_descriptor(&field_tys, Ty::Unit);
                if args.iter().any(|&a| self.records_frame(a)) {
                    // A branchy argument can't run with `[new, dup]` on the stack — its merge frame
                    // would omit them. Evaluate all args into temps first (clean stack), then build.
                    let temps = self.spill_to_temps(&args, code);
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for &(slot, t, _) in &temps { load(t, slot, code); }
                    for &(_, _, key) in &temps { self.slots.remove(&key); }
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    code.invokespecial(m, aw, 0);
                } else {
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for &a in &args {
                        self.emit_value(a, code);
                    }
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    code.invokespecial(m, aw, 0);
                }
            }
            IrExpr::MethodCall { class, index, receiver, args } => {
                let c = &self.ir.classes[*class as usize];
                let fid = c.methods[*index as usize];
                let f = &self.ir.functions[fid as usize];
                let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
                let ret = ir_ty_to_jvm(&f.ret);
                let name = f.name.clone();
                let owner = c.fq_name.clone();
                let is_iface = c.is_interface;
                let mut ops = vec![*receiver];
                ops.extend(args.iter().copied());
                self.emit_operands(&ops, code);
                let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let desc = method_descriptor(&param_tys, ret);
                if is_iface {
                    // Dispatch through an interface — `invokeinterface I.m`.
                    let m = self.cw.interface_methodref(&owner, &name, &desc);
                    code.invokeinterface(m, aw, slot_words(ret) as i32);
                } else {
                    let m = self.cw.methodref(&owner, &name, &desc);
                    code.invokevirtual(m, aw, slot_words(ret) as i32);
                }
            }
            IrExpr::Call { callee, dispatch_receiver, args } => match callee {
                Callee::Local(fid) => {
                    let f = &self.ir.functions[*fid as usize];
                    let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
                    let ret = ir_ty_to_jvm(&f.ret);
                    let name = f.name.clone();
                    let args = args.clone();
                    self.emit_operands(&args, code);
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
                    // Box a primitive into a reference target, unbox a wrapper into a primitive, or
                    // widen/narrow between primitive numeric types (`Int`→`Long`, `Double`→`Int`, …).
                    IrTypeOp::ImplicitCoercion => {
                        let at = self.value_ty(*arg);
                        let target = ir_ty_to_jvm(type_operand);
                        if at.is_primitive() && target.is_reference() {
                            self.box_prim(at, code);
                        } else if at.is_reference() && target.is_primitive() {
                            self.unbox_to(target, code);
                        } else if at.is_primitive() && target.is_primitive() && at != target {
                            emit_num_conv(at, target, code);
                        }
                    }
                    IrTypeOp::SafeCast => {}
                }
            }
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.emit_binop(*op, *lhs, *rhs, code),
            IrExpr::EnumEntry { class, index } => {
                let c = &self.ir.classes[*class as usize];
                let (entry, _) = c.enum_entries[*index as usize].clone();
                let desc = format!("L{};", c.fq_name);
                let f = self.cw.fieldref(&c.fq_name.clone(), &entry, &desc);
                code.getstatic(f, 1);
            }
            IrExpr::ObjectInstance { class } => {
                let fq = self.ir.classes[*class as usize].fq_name.clone();
                let f = self.cw.fieldref(&fq, "INSTANCE", &format!("L{fq};"));
                code.getstatic(f, 1);
            }
            IrExpr::EnumValues { class } => {
                let fq = self.ir.classes[*class as usize].fq_name.clone();
                let m = self.cw.methodref(&fq, "values", &format!("()[L{fq};"));
                code.invokestatic(m, 0, 1);
            }
            IrExpr::EnumValueOf { class, arg } => {
                let fq = self.ir.classes[*class as usize].fq_name.clone();
                self.emit_value(*arg, code);
                let m = self.cw.methodref(&fq, "valueOf", &format!("(Ljava/lang/String;)L{fq};"));
                code.invokestatic(m, 1, 1);
            }
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
            IrExpr::Lambda { impl_fn, arity, captures } => {
                // Non-capturing lambdas only (lowering bails otherwise).
                debug_assert!(captures.is_empty());
                let f = &self.ir.functions[*impl_fn as usize];
                let impl_name = f.name.clone();
                let impl_params: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
                let impl_ret = ir_ty_to_jvm(&f.ret);
                let iface = format!("kotlin/jvm/functions/Function{arity}");
                let impl_desc = jvm_descriptor(&impl_params, impl_ret);
                // samMethodType: the erased `(Object,…)Object`; instantiatedMethodType: boxed actuals.
                let sam_desc = sam_descriptor(*arity);
                let inst_params: Vec<String> = impl_params.iter().map(|t| boxed_descriptor(*t)).collect();
                let inst_desc = format!("({}){}", inst_params.concat(), boxed_descriptor(impl_ret));
                let facade = self.facade.clone();
                let meta = self.cw.method_handle_static(
                    "java/lang/invoke/LambdaMetafactory", "metafactory", LMF_METAFACTORY_DESC);
                let sam_mt = self.cw.method_type(&sam_desc);
                let impl_mh = self.cw.method_handle_static(&facade, &impl_name, &impl_desc);
                let inst_mt = self.cw.method_type(&inst_desc);
                let bsm = self.cw.add_bootstrap(meta, vec![sam_mt, impl_mh, inst_mt]);
                let indy = self.cw.invoke_dynamic(bsm, "invoke", &format!("()L{iface};"));
                code.invokedynamic(indy, 0, 1);
            }
            IrExpr::InvokeFunction { func, args, ret } => {
                let n = args.len();
                self.emit_value(*func, code);
                for &arg in args {
                    self.emit_value(arg, code);
                    let at = self.value_ty(arg);
                    self.box_prim(at, code); // box a primitive arg to its wrapper (an Object)
                }
                let iface = format!("kotlin/jvm/functions/Function{n}");
                let m = self.cw.interface_methodref(&iface, "invoke", &sam_descriptor(n as u8));
                code.invokeinterface(m, n as i32, 1);
                // The interface returns `Object`; cast/unbox to the function's declared return type.
                let rt = ir_ty_to_jvm(ret);
                match rt {
                    Ty::Int | Ty::Long | Ty::Double | Ty::Float | Ty::Boolean | Ty::Char
                    | Ty::Byte | Ty::Short => self.unbox_to(rt, code),
                    Ty::Unit | Ty::Nothing => code.pop(),
                    Ty::String => { let ci = self.cw.class_ref("java/lang/String"); code.checkcast(ci); }
                    Ty::Obj(internal) => { let ci = self.cw.class_ref(internal); code.checkcast(ci); }
                    Ty::Array(_) => { let ci = self.cw.class_ref(&rt.descriptor()); code.checkcast(ci); }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn emit_intrinsic(&mut self, fq: &str, recv: &Option<u32>, args: &[u32], code: &mut CodeBuilder) {
        match fq {
            // Static numeric helpers used by synthesized data-class equals/hashCode.
            "java/lang/Double.hashCode" | "java/lang/Long.hashCode" | "java/lang/Float.hashCode"
            | "java/lang/Boolean.hashCode" | "java/util/Objects.hashCode" => {
                self.emit_value(args[0], code);
                let (cls, d) = match fq {
                    "java/lang/Double.hashCode" => ("java/lang/Double", "(D)I"),
                    "java/lang/Long.hashCode" => ("java/lang/Long", "(J)I"),
                    "java/lang/Float.hashCode" => ("java/lang/Float", "(F)I"),
                    "java/lang/Boolean.hashCode" => ("java/lang/Boolean", "(Z)I"),
                    _ => ("java/util/Objects", "(Ljava/lang/Object;)I"),
                };
                let aw = slot_words(self.value_ty(args[0])) as i32;
                let m = self.cw.methodref(cls, "hashCode", d);
                code.invokestatic(m, aw, 1);
            }
            "java/lang/Double.compare" | "java/lang/Float.compare" => {
                self.emit_value(args[0], code);
                self.emit_value(args[1], code);
                let (cls, d, aw) = if fq == "java/lang/Double.compare" { ("java/lang/Double", "(DD)I", 4) } else { ("java/lang/Float", "(FF)I", 2) };
                let m = self.cw.methodref(cls, "compare", d);
                code.invokestatic(m, aw, 1);
            }
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
            // `e.ordinal` / `e.name` on an enum value → `Enum.ordinal()I` / `Enum.name()String`.
            "java/lang/Enum.ordinal" => {
                self.emit_value(recv.unwrap(), code);
                let m = self.cw.methodref("java/lang/Enum", "ordinal", "()I");
                code.invokevirtual(m, 0, 1);
            }
            "java/lang/Enum.name" => {
                self.emit_value(recv.unwrap(), code);
                let m = self.cw.methodref("java/lang/Enum", "name", "()Ljava/lang/String;");
                code.invokevirtual(m, 0, 1);
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

    /// Whether emitting `e` as a value records a StackMapTable frame (a primitive comparison, a
    /// `when`, or a `while` — anywhere in its subtree). Such an expression can't be emitted while
    /// other operands sit on the stack (its merge frames would omit them); callers spill first.
    fn records_frame(&self, e: u32) -> bool {
        use IrBinOp::*;
        match self.ir.expr(e) {
            IrExpr::When { .. } | IrExpr::While { .. } => true,
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
                (matches!(op, Lt | Le | Gt | Ge | Eq | Ne) && self.value_ty(*lhs).is_primitive())
                    || self.records_frame(*lhs) || self.records_frame(*rhs)
            }
            IrExpr::Call { dispatch_receiver, args, .. } =>
                dispatch_receiver.map_or(false, |r| self.records_frame(r)) || args.iter().any(|&a| self.records_frame(a)),
            IrExpr::MethodCall { receiver, args, .. } =>
                self.records_frame(*receiver) || args.iter().any(|&a| self.records_frame(a)),
            IrExpr::New { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::GetField { receiver, .. } => self.records_frame(*receiver),
            IrExpr::SetField { receiver, value, .. } => self.records_frame(*receiver) || self.records_frame(*value),
            IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => self.records_frame(*value),
            IrExpr::TypeOp { arg, .. } | IrExpr::EnumValueOf { arg, .. } => self.records_frame(*arg),
            IrExpr::Return(v) => v.map_or(false, |x| self.records_frame(x)),
            IrExpr::Variable { init, .. } => init.map_or(false, |i| self.records_frame(i)),
            IrExpr::Block { stmts, value } =>
                stmts.iter().any(|&s| self.records_frame(s)) || value.map_or(false, |v| self.records_frame(v)),
            _ => false, // Const, GetValue, GetStatic, EnumEntry, EnumValues — no frames
        }
    }

    /// Push `ops` onto the stack in order. If any op after the first records a frame (so an earlier
    /// op would be live on the stack across that frame), evaluate all ops into temps first, then load
    /// them — keeping the stack empty while each frame-recording op runs.
    fn emit_operands(&mut self, ops: &[u32], code: &mut CodeBuilder) {
        if ops.iter().skip(1).any(|&o| self.records_frame(o)) {
            let temps = self.spill_to_temps(ops, code);
            for &(slot, t, _) in &temps { load(t, slot, code); }
            for &(_, _, key) in &temps { self.slots.remove(&key); }
        } else {
            for &o in ops { self.emit_value(o, code); }
        }
    }

    /// Evaluate each of `ops` into a fresh temp slot, in order. Each temp is registered in `self.slots`
    /// (so a *later* op's frames see the earlier temps as live, not `Top`); the caller loads them and
    /// then removes them (they're dead once loaded). Returns `(slot, ty, slots-key)` per op.
    fn spill_to_temps(&mut self, ops: &[u32], code: &mut CodeBuilder) -> Vec<(u16, Ty, u32)> {
        let mut temps = Vec::new();
        for &o in ops {
            self.emit_value(o, code);
            let t = self.value_ty(o);
            let slot = self.next_slot;
            self.next_slot += slot_words(t);
            store(t, slot, code);
            let key = 2_000_000 + slot as u32;
            self.slots.insert(key, (slot, t));
            temps.push((slot, t, key));
        }
        temps
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
                // Evaluate lhs, hold it in a temp while rhs is emitted (rhs may record frames that
                // must see the temp as live), then combine. The temp is dead afterwards, so remove it
                // from the slot map so it doesn't leak into later merge frames (next_slot stays
                // monotonic — no reuse). Without this, a `false`/`else` path that never assigned the
                // temp reaches a merge whose frame claims it's defined → VerifyError.
                self.emit_value(lhs, code);
                let tmp = self.next_slot;
                self.next_slot += 1;
                let key = 1_000_000 + tmp as u32;
                self.slots.insert(key, (tmp, Ty::Boolean));
                code.istore(tmp);
                self.emit_value(rhs, code);
                code.iload(tmp);
                if op == And { code.iand() } else { code.ior() }
                self.slots.remove(&key);
            }
            Lt | Le | Gt | Ge | Eq | Ne => self.emit_compare(op, lhs, rhs, code),
        }
    }

    fn emit_compare(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        let lt = self.value_ty(lhs);
        // Kotlin `==`/`!=` on reference operands is structural (`a?.equals(b)`), realized by the
        // null-safe `Objects.equals`. Primitives keep the `if_icmp*`/3-way-compare path below.
        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) && lt.is_reference() && self.value_ty(rhs).is_reference() {
            // Spill if rhs is branchy (`x == when{…}`) so lhs isn't live across its merge frames.
            self.emit_operands(&[lhs, rhs], code);
            let m = self.cw.methodref("java/util/Objects", "equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z");
            code.invokestatic(m, 2, 1);
            if op == IrBinOp::Ne {
                code.push_int(1, self.cw);
                code.ixor();
            }
            return;
        }
        self.emit_operands(&[lhs, rhs], code);
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
        let last = branches.last().map(|(_, b)| self.value_ty(*b)).unwrap_or(Ty::Unit);
        // A `null`/`Nothing` last branch (e.g. the no-receiver arm of a safe-call `a?.b`) carries no
        // concrete type and would verify-type the merge stack as `top`. Use a concrete branch type so
        // the merge frame is a reference — `null` is assignable to any reference.
        if matches!(last, Ty::Null | Ty::Nothing | Ty::Error) {
            for (_, b) in branches {
                let t = self.value_ty(*b);
                if !matches!(t, Ty::Null | Ty::Nothing | Ty::Error) {
                    return t;
                }
            }
        }
        last
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
            IrExpr::EnumEntry { class, .. } | IrExpr::EnumValueOf { class, .. } | IrExpr::ObjectInstance { class } => Ty::obj(&self.ir.classes[*class as usize].fq_name),
            IrExpr::EnumValues { class } => Ty::array(Ty::obj(&self.ir.classes[*class as usize].fq_name)),
            IrExpr::Block { value, .. } => value.map(|v| self.value_ty(v)).unwrap_or(Ty::Unit),
            IrExpr::TypeOp { op, type_operand, .. } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                _ => ir_ty_to_jvm(type_operand),
            },
            IrExpr::Lambda { arity, .. } => Ty::obj(&format!("kotlin/jvm/functions/Function{arity}")),
            IrExpr::InvokeFunction { ret, .. } => ir_ty_to_jvm(ret),
            _ => Ty::Error,
        }
    }
}

/// The `LambdaMetafactory.metafactory` bootstrap-method descriptor (the standard non-altmetafactory form).
const LMF_METAFACTORY_DESC: &str = "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;\
Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;\
Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;";

/// A JVM method descriptor `(p1p2…)R` from parameter/return `Ty`s.
fn jvm_descriptor(params: &[Ty], ret: Ty) -> String {
    let mut s = String::from("(");
    for p in params {
        s.push_str(&p.descriptor());
    }
    s.push(')');
    s.push_str(&ret.descriptor());
    s
}

/// The erased SAM descriptor `(Ljava/lang/Object;…)Ljava/lang/Object;` for `FunctionN.invoke`.
fn sam_descriptor(arity: u8) -> String {
    let mut s = String::from("(");
    for _ in 0..arity {
        s.push_str("Ljava/lang/Object;");
    }
    s.push_str(")Ljava/lang/Object;");
    s
}

/// The boxed (wrapper) descriptor for a `Ty` — primitives map to their wrapper, references unchanged.
fn boxed_descriptor(t: Ty) -> String {
    match t {
        Ty::Int => "Ljava/lang/Integer;",
        Ty::Long => "Ljava/lang/Long;",
        Ty::Double => "Ljava/lang/Double;",
        Ty::Float => "Ljava/lang/Float;",
        Ty::Boolean => "Ljava/lang/Boolean;",
        Ty::Char => "Ljava/lang/Character;",
        Ty::Byte => "Ljava/lang/Byte;",
        Ty::Short => "Ljava/lang/Short;",
        _ => return t.descriptor(),
    }
    .to_string()
}

/// JVM internal name for a reference `Ty`, for `instanceof`/`checkcast`.
/// Convert the numeric primitive on top of the stack from `from` to `to` (JVM `i2l`/`i2d`/…).
/// Byte/Short/Char live in the `int` stack category; widening goes via that category, and a
/// Byte/Short/Char target is narrowed from `int` last.
fn emit_num_conv(from: Ty, to: Ty, code: &mut CodeBuilder) {
    use Ty::*;
    if from == to { return; }
    let wide = |t: Ty| match t { Byte | Short | Char | Int => Int, o => o };
    match (wide(from), wide(to)) {
        (Int, Long) => code.i2l(), (Int, Float) => code.i2f(), (Int, Double) => code.i2d(),
        (Long, Int) => code.l2i(), (Long, Float) => code.l2f(), (Long, Double) => code.l2d(),
        (Float, Int) => code.f2i(), (Float, Long) => code.f2l(), (Float, Double) => code.f2d(),
        (Double, Int) => code.d2i(), (Double, Long) => code.d2l(), (Double, Float) => code.d2f(),
        _ => {} // same wide category (e.g. Byte→Int): the value is already correct on the stack
    }
    match to { Byte => code.i2b(), Short => code.i2s(), Char => code.i2c(), _ => {} }
}

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
        "kotlin/String.length" | "kotlin/Array.size" | "java/lang/Enum.ordinal" => Ty::Int,
        "kotlin/String.get" => Ty::Char,
        "kotlin/Array.set" => Ty::Unit,
        "java/lang/Enum.name" => Ty::String,
        f if f.ends_with(".hashCode") || f.ends_with(".compare") => Ty::Int,
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

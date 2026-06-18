//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of the backend-agnostic IR — it maps
//! Kotlin FqNames to JVM descriptors here (the IR never carries descriptors). Covers the core
//! subset (functions, simple classes); shares `CodeBuilder`/`ClassWriter` with the AST emitter.

use std::collections::HashMap;

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrType, IrTypeOp};
use crate::jvm::classfile::{ClassWriter, CodeBuilder, Label, VerifType};
use crate::jvm::inline::MethodBodies;
use crate::jvm::names::method_descriptor;
use crate::types::Ty;

/// Emit a whole IR file: the facade class of top-level `static` functions, plus one `.class` per
/// `IrClass`. Returns `(internal_name, bytes)` for each, or `None` when the IR uses a construct the
/// JVM backend can't represent (so every emission path skips it rather than miscompiling).
pub fn emit_all(ir: &IrFile, facade: &str, bodies: &dyn MethodBodies) -> Option<Vec<(String, Vec<u8>)>> {
    if !jvm_can_emit(ir) {
        return None;
    }
    let mut out = Vec::new();
    // Facade: the static top-level functions (those with no dispatch receiver).
    let mut cw = ClassWriter::new(facade, "java/lang/Object");
    for (i, f) in ir.functions.iter().enumerate() {
        if f.dispatch_receiver.is_some() || f.body.is_none() {
            continue;
        }
        emit_method(ir, i as u32, facade, facade, &mut cw, false, bodies);
    }
    emit_statics(ir, facade, &mut cw, bodies);
    out.push((facade.to_string(), cw.finish()));
    // Each class.
    for c in &ir.classes {
        out.push((c.fq_name.clone(), emit_class(ir, c, facade, bodies)));
    }
    Some(out)
}

/// Whether the JVM backend can represent this IR. The JVM stdlib provides fixed-arity
/// `kotlin/jvm/functions/Function0..22`; a function type or lambda of higher arity needs a different
/// vararg representation krusty doesn't emit, so such a file is skipped — never miscompiled. This is a
/// JVM constraint (the language allows any arity), so it lives in the JVM emitter, not common lowering.
fn jvm_can_emit(ir: &IrFile) -> bool {
    fn ty_ok(t: &IrType) -> bool {
        match t {
            IrType::Function { params, ret } => params.len() <= 22 && params.iter().all(ty_ok) && ty_ok(ret),
            IrType::Class { type_args, .. } => type_args.iter().all(ty_ok),
            _ => true,
        }
    }
    if ir.functions.iter().any(|f| !ty_ok(&f.ret) || !f.params.iter().all(ty_ok)) {
        return false;
    }
    if ir.statics.iter().any(|s| !ty_ok(&s.ty)) {
        return false;
    }
    ir.exprs.iter().all(|e| match e {
        IrExpr::Lambda { arity, .. } => *arity <= 22,
        IrExpr::Variable { ty, .. } => ty_ok(ty),
        _ => true,
    })
}

/// Back-compat single-facade entry (used where a file has only functions).
pub fn emit_file(ir: &IrFile, facade: &str, bodies: &dyn MethodBodies) -> Vec<u8> {
    let mut cw = ClassWriter::new(facade, "java/lang/Object");
    for (i, f) in ir.functions.iter().enumerate() {
        if f.dispatch_receiver.is_none() && f.body.is_some() {
            emit_method(ir, i as u32, facade, facade, &mut cw, false, bodies);
        }
    }
    emit_statics(ir, facade, &mut cw, bodies);
    cw.finish()
}

/// Emit the facade's top-level properties as `public static` fields plus a `<clinit>` that runs
/// their initializers in declaration order.
/// Convert the inliner's `VType` (a relocated frame verification type) to the class-writer's
/// `VerifType`. `Uninitialized` types shouldn't reach here (`splice_branchy` bails on them).
fn vtype_to_verif(v: &crate::jvm::inline::VType) -> VerifType {
    use crate::jvm::inline::VType;
    match v {
        VType::Top => VerifType::Top,
        VType::Int => VerifType::Integer,
        VType::Float => VerifType::Float,
        VType::Long => VerifType::Long,
        VType::Double => VerifType::Double,
        VType::Null => VerifType::Null,
        VType::Object(idx) => VerifType::Object(*idx),
        VType::UninitThis | VType::Uninit(_) => VerifType::Top,
    }
}

fn emit_statics(ir: &IrFile, facade: &str, cw: &mut ClassWriter, bodies: &dyn MethodBodies) {
    if ir.statics.is_empty() {
        return;
    }
    for s in &ir.statics {
        cw.add_field(0x0009 /* PUBLIC | STATIC */, &s.name, &ir_ty_to_jvm(&s.ty).descriptor());
    }
    let mut e = Emitter { ir, cw, bodies, owner: facade.to_string(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 0, ret: Ty::Unit, loop_stack: Vec::new(), inlining: false };
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

fn emit_class(ir: &IrFile, c: &crate::ir::IrClass, facade: &str, bodies: &dyn MethodBodies) -> Vec<u8> {
    if !c.enum_entries.is_empty() {
        return emit_enum_class(ir, c, facade, bodies);
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
    // Backing fields are private; access goes through the synthesized `getX()`/`setX()` accessors
    // (kotlinc does the same) — for both normal classes and objects.
    let base_field_acc: u16 = 0x0002;
    for (i, (name, ty)) in c.fields.iter().enumerate() {
        // A `val` backing field is `final`.
        let acc = base_field_acc | if c.field_final.get(i).copied().unwrap_or(false) { 0x0010 } else { 0 };
        cw.add_field(acc, name, &ir_ty_to_jvm(ty).descriptor());
    }
    // Constructor: super(); store each ctor *parameter* into its field; then run `init_body`
    // (body-property initializers + `init {}` blocks). Fields past `ctor_param_count` are body
    // properties — not parameters — so the descriptor covers only the leading parameter fields.
    let field_tys: Vec<Ty> = c.fields.iter().map(|(_, t)| ir_ty_to_jvm(t)).collect();
    let n_params = c.ctor_param_count as usize;
    let param_tys: Vec<Ty> = field_tys[..n_params].to_vec();
    let params_words: u16 = param_tys.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + params_words);
    // The superclass constructor's parameter types (empty for the erased top type — the front end
    // names it `kotlin/Any`, which this backend maps to `java/lang/Object`).
    let super_param_tys: Vec<Ty> = if crate::jvm::jvm_class_map::to_jvm_internal(&c.superclass) == "java/lang/Object" {
        Vec::new()
    } else {
        ir.classes.iter().find(|sc| sc.fq_name == c.superclass)
            .map(|sc| sc.fields[..sc.ctor_param_count as usize].iter().map(|(_, t)| ir_ty_to_jvm(t)).collect())
            .unwrap_or_default()
    };
    let max_slot;
    let mut init_diverges = false;
    {
        let mut e = Emitter { ir, cw: &mut cw, bodies, owner: c.fq_name.clone(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 1 + params_words, ret: Ty::Unit, loop_stack: Vec::new(), inlining: false };
        e.slots.insert(0, (0, Ty::obj(&c.fq_name)));
        let mut s = 1u16;
        for (vi, t) in param_tys.iter().enumerate() {
            e.slots.insert(vi as u32 + 1, (s, *t));
            s += slot_words(*t);
        }
        // kotlinc guards each non-null reference constructor parameter with checkNotNullParameter at
        // the very start of `<init>` — before the super() call.
        let ctor_checks = c.ctor_param_checks.clone();
        for (i, check) in ctor_checks.iter().enumerate() {
            if let Some(name) = check {
                if let Some(&(slot, _)) = e.slots.get(&(i as u32 + 1)) {
                    ctor.aload(slot);
                    ctor.push_string(name, e.cw);
                    let m = e.cw.methodref("kotlin/jvm/internal/Intrinsics", "checkNotNullParameter", "(Ljava/lang/Object;Ljava/lang/String;)V");
                    ctor.invokestatic(m, 2, 0);
                }
            }
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
            init_diverges = e.diverges(init_body);
        }
        max_slot = e.next_slot;
    }
    // A diverging `init` (e.g. `init { throw … }`) leaves no fall-through — the trailing `return`
    // would be dead code after `athrow` (which the verifier rejects without a frame).
    if !init_diverges {
        ctor.ret_void();
    }
    ctor.ensure_locals(max_slot);
    ctor.link();
    // An `object`'s constructor is private; a `C$Companion`'s is package-private (so the outer class's
    // `<clinit>` can call it without nestmate attributes); a normal class's is public.
    let ctor_access = if c.is_object { 0x0002 } else if c.is_companion { 0x0000 } else { 0x0001 };
    cw.add_method(ctor_access, "<init>", &method_descriptor(&param_tys, Ty::Unit), &ctor);

    // Secondary constructors: each `<init>(p)` calls `this(delegate_args)` (the primary `<init>`) then
    // runs its body. `this` is slot 0, parameters follow.
    for sc in &c.secondary_ctors {
        let sc_param_tys: Vec<Ty> = sc.params.iter().map(ir_ty_to_jvm).collect();
        let sc_words: u16 = sc_param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut sctor = CodeBuilder::new(1 + sc_words);
        let sec_max;
        let mut sec_diverges = false;
        {
            let mut e = Emitter { ir, cw: &mut cw, bodies, owner: c.fq_name.clone(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 1 + sc_words, ret: Ty::Unit, loop_stack: Vec::new(), inlining: false };
            e.slots.insert(0, (0, Ty::obj(&c.fq_name)));
            let mut s = 1u16;
            for (vi, t) in sc_param_tys.iter().enumerate() {
                e.slots.insert(vi as u32 + 1, (s, *t));
                s += slot_words(*t);
            }
            // `this(delegate_args)` — `invokespecial` the primary `<init>` (this is loaded first, so
            // spill any branchy delegate argument to a temp before it).
            let dargs = sc.delegate_args.clone();
            if dargs.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&dargs, &mut sctor);
                sctor.aload(0);
                for &(slot, t, _) in &temps { load(t, slot, &mut sctor); }
                for &(_, _, key) in &temps { e.slots.remove(&key); }
            } else {
                sctor.aload(0);
                for &a in &dargs { e.emit_value(a, &mut sctor); }
            }
            let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let prim_init = e.cw.methodref(&c.fq_name, "<init>", &method_descriptor(&param_tys, Ty::Unit));
            sctor.invokespecial(prim_init, aw, 0);
            if let Some(body) = sc.body {
                e.emit(body, &mut sctor);
                sec_diverges = e.diverges(body);
            }
            sec_max = e.next_slot;
        }
        if !sec_diverges {
            sctor.ret_void();
        }
        sctor.ensure_locals(sec_max);
        sctor.link();
        cw.add_method(0x0001, "<init>", &method_descriptor(&sc_param_tys, Ty::Unit), &sctor);
    }
    // A class with a `companion object`: a `public static final Companion` field of the companion
    // type, constructed in this class's `<clinit>`.
    if let Some(comp_fq) = &c.companion_class {
        let comp_desc = format!("L{comp_fq};");
        cw.add_field(0x0019, "Companion", &comp_desc); // PUBLIC | STATIC | FINAL
        let mut clinit = CodeBuilder::new(0);
        let ci = cw.class_ref(comp_fq);
        clinit.new_obj(ci);
        clinit.dup();
        let init = cw.methodref(comp_fq, "<init>", "()V");
        clinit.invokespecial(init, 0, 0);
        let fref = cw.fieldref(&c.fq_name, "Companion", &comp_desc);
        clinit.putstatic(fref, 1);
        clinit.ret_void();
        clinit.ensure_locals(0);
        clinit.link();
        cw.add_method(0x0008, "<clinit>", "()V", &clinit);
    }
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
            emit_method(ir, fid, &c.fq_name, facade, &mut cw, true, bodies);
        } else {
            let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
            let ret = ir_ty_to_jvm(&f.ret);
            cw.add_abstract_method(0x0001 | 0x0400, &f.name, &method_descriptor(&param_tys, ret));
        }
        // A method with default-valued parameters gets a `<name>$default(self, params…, mask, marker)`
        // synthetic stub (the JVM realization of default arguments).
        if let Some(defaults) = ir.fn_param_defaults.get(&fid) {
            emit_default_stub(ir, fid, &c.fq_name, facade, &mut cw, defaults, bodies);
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
fn emit_enum_class(ir: &IrFile, c: &crate::ir::IrClass, facade: &str, bodies: &dyn MethodBodies) -> Vec<u8> {
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
        let mut e = Emitter { ir, cw: &mut cw, bodies, owner: fq.clone(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 0, ret: Ty::Unit, loop_stack: Vec::new(), inlining: false };
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
            emit_method(ir, fid, &fq, facade, &mut cw, true, bodies);
        }
    }
    cw.finish()
}

/// Emit function `fid` as a method on `owner`. `instance` = an instance method (`this` in slot 0).
fn emit_method(ir: &IrFile, fid: u32, owner: &str, facade: &str, cw: &mut ClassWriter, instance: bool, bodies: &dyn MethodBodies) {
    let f = &ir.functions[fid as usize];
    let body = f.body.unwrap();
    let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
    let ret = ir_ty_to_jvm(&f.ret);
    let mut e = Emitter { ir, cw, bodies, owner: owner.to_string(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 0, ret, loop_stack: Vec::new(), inlining: false };
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
    // kotlinc guards each non-null reference parameter of a visible function with
    // `Intrinsics.checkNotNullParameter(param, "name")` at method entry — emit the same.
    let param_checks = f.param_checks.clone();
    for (i, check) in param_checks.iter().enumerate() {
        if let Some(name) = check {
            let vi = i as u32 + if instance { 1 } else { 0 };
            if let Some(&(slot, _)) = e.slots.get(&vi) {
                code.aload(slot);
                code.push_string(name, e.cw);
                let m = e.cw.methodref("kotlin/jvm/internal/Intrinsics", "checkNotNullParameter", "(Ljava/lang/Object;Ljava/lang/String;)V");
                code.invokestatic(m, 2, 0);
            }
        }
    }
    e.emit(body, &mut code);
    // The implicit `return` for a `Unit` function is dead code when the body already diverges
    // (`fun foo() { throw … }`): an unreachable `return` after `athrow` has no stack-map frame and
    // the verifier rejects it. Skip it exactly when the body can't fall through.
    if ret == Ty::Unit && !e.diverges(body) {
        code.ret_void();
    }
    code.ensure_locals(e.next_slot);
    code.link();
    // Top-level/`static` functions are always `final` (kotlinc emits `public static final`). An
    // instance method of a *final* class (nothing extends it) is also `final` and can never be
    // overridden, so marking it is safe; in an open/extended class we conservatively leave it
    // non-`final` (a method-level `open`/`override` model would refine this).
    let access = if instance {
        let final_class = !ir.classes.iter().any(|o| o.superclass == owner);
        0x0001 | if final_class { 0x0010 } else { 0 }
    } else {
        0x0019 // PUBLIC | STATIC | FINAL
    };
    e.cw.add_method(access, &f.name, &method_descriptor(&param_tys, ret), &code);
}

/// Emit the JVM `<name>$default(self, params…, mask: int, marker: Object)` synthetic stub for an
/// instance method with default-valued parameters: for each defaulted param, `if ((mask & (1<<i)) != 0)
/// param = <default>;` then tail-call the real method. The default-value exprs reference `self` as value
/// 0. This is the JVM realization of default arguments — the `param_defaults` *meaning* is in the IR.
fn emit_default_stub(ir: &IrFile, fid: u32, owner: &str, facade: &str, cw: &mut ClassWriter, defaults: &[Option<u32>], bodies: &dyn MethodBodies) {
    let f = &ir.functions[fid as usize];
    let method_name = f.name.clone();
    let real_params: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
    let ret = ir_ty_to_jvm(&f.ret);
    let n = real_params.len();
    let owner_ty = Ty::obj(owner);

    let mut e = Emitter { ir, cw, bodies, owner: owner.to_string(), facade: facade.to_string(), slots: HashMap::new(), next_slot: 0, ret, loop_stack: Vec::new(), inlining: false };
    // value 0 = self; values 1..=n = the real params; then mask + marker (not value-indexed).
    e.slots.insert(0, (0, owner_ty));
    let mut slot = 1u16;
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in real_params.iter().enumerate() {
        e.slots.insert((i + 1) as u32, (slot, *t));
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let mask_slot = slot;
    e.slots.insert(9_000_001, (mask_slot, Ty::Int)); // register so frames type these slots
    slot += 1;
    e.slots.insert(9_000_002, (slot, Ty::obj("java/lang/Object")));
    slot += 1;
    e.next_slot = slot;

    let mut code = CodeBuilder::new(slot);
    for (i, def) in defaults.iter().enumerate().take(n) {
        if let Some(def_expr) = def {
            let (pslot, pty) = param_slots[i];
            code.iload(mask_slot);
            code.push_int(1 << i, e.cw);
            code.iand();
            let skip = code.new_label();
            e.frame(skip, vec![], &mut code);
            code.ifeq(skip);
            e.emit_value(*def_expr, &mut code);
            store(pty, pslot, &mut code);
            code.bind(skip);
        }
    }
    code.aload(0);
    for &(pslot, pty) in &param_slots {
        load(pty, pslot, &mut code);
    }
    let aw: i32 = real_params.iter().map(|t| slot_words(*t) as i32).sum();
    let m = e.cw.methodref(owner, &method_name, &method_descriptor(&real_params, ret));
    code.invokevirtual(m, aw, slot_words(ret) as i32);
    emit_return(ret, &mut code);
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = vec![owner_ty];
    stub_params.extend(real_params.iter().copied());
    stub_params.push(Ty::Int);
    stub_params.push(Ty::obj("java/lang/Object"));
    let desc = method_descriptor(&stub_params, ret);
    e.cw.add_method(0x1009 /* PUBLIC | STATIC | SYNTHETIC */, &format!("{method_name}$default"), &desc, &code);
}

struct Emitter<'a> {
    ir: &'a IrFile,
    cw: &'a mut ClassWriter,
    /// The narrow bytecode provider — lets the emitter read a cross-module `inline fun`'s compiled
    /// body (`bodies.body`) to splice it at the call site (the bytecode inliner).
    bodies: &'a dyn MethodBodies,
    owner: String,
    facade: String,
    slots: HashMap<u32, (u16, Ty)>,
    next_slot: u16,
    ret: Ty,
    /// Stack of enclosing loops' `(continue_label, break_label)` — `break`/`continue` target the top.
    loop_stack: Vec<(Label, Label)>,
    /// True while emitting an inlined lambda body (route-(b) lambda splice): a `Return` leaves its value
    /// on the operand stack and falls through (the spliced stdlib body continues) instead of `*return`.
    inlining: bool,
}

/// Parse a method descriptor's parameter types (in order) to `Ty`s.
fn parse_descriptor_params(desc: &str) -> Option<Vec<Ty>> {
    let inner = desc.strip_prefix('(')?.split(')').next()?;
    let b = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while b.get(i) == Some(&b'[') {
            i += 1;
        }
        match b.get(i)? {
            b'L' => {
                while b.get(i) != Some(&b';') {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
        out.push(crate::jvm::jvm_libraries::desc_to_ty(&inner[start..i]));
    }
    Some(out)
}

impl<'a> Emitter<'a> {
    /// Emit lambda impl function `impl_fn`'s body INLINE: bind its parameter value-indices `0..` to the
    /// given JVM slots (the lambda's typed arguments), then emit its body with `inlining` set so its
    /// `Return` leaves the result on the stack (instead of `*return`). Used by route-(b) lambda splice.
    fn emit_fn_body_inline(&mut self, impl_fn: u32, param_slots: &[(u16, Ty)], code: &mut CodeBuilder) {
        let body = self.ir.functions[impl_fn as usize].body.expect("lambda impl has a body");
        let saved_slots = std::mem::take(&mut self.slots);
        let saved_inlining = self.inlining;
        self.inlining = true;
        for (i, &(slot, ty)) in param_slots.iter().enumerate() {
            self.slots.insert(i as u32, (slot, ty));
        }
        self.emit(body, code);
        self.inlining = saved_inlining;
        self.slots = saved_slots;
    }

    /// Route (b): inline a cross-module `inline fun` whose body calls a lambda *parameter*, splicing the
    /// caller's lambda body at the `FunctionN.invoke` site. v1: a branchless single-invoke body
    /// (`let`/`also`/`run`/`apply`/…), one lambda argument, no captures. Returns `true` if inlined.
    fn try_inline_lambda_call(
        &mut self,
        descriptor: &str,
        args: &[u32],
        lam_idx: usize,
        lam_expr: u32,
        body: &crate::jvm::classreader::MethodCode,
        base: u16,
        code: &mut CodeBuilder,
    ) -> bool {
        let IrExpr::Lambda { impl_fn, arity, captures, .. } = self.ir.expr(lam_expr).clone() else {
            return false;
        };
        if !captures.is_empty() {
            return false; // v1: only non-capturing lambdas
        }
        let arity = arity as usize;
        let Some(params) = parse_descriptor_params(descriptor) else { return false };
        if lam_idx >= params.len() {
            return false;
        }
        // Slot offset of the lambda parameter within the (shifted) body frame.
        let lambda_off: u16 = params[..lam_idx].iter().map(|t| slot_words(*t)).sum();
        let Some((before, after)) = crate::jvm::inline::branchless_lambda_segments(body, base, lambda_off, self.cw) else {
            return false;
        };
        // Reserve the spliced body's own local range (`base..base+max_locals`, e.g. kotlinc's `$i$f`
        // inline-marker store) so the fresh lambda-param slots sit above it and `max_locals` covers all.
        self.next_slot = self.next_slot.max(base + body.max_locals);
        // The lambda impl's parameters are `[captures…, lambda_params…]`.
        let impl_f = &self.ir.functions[impl_fn as usize];
        let Some(n_cap) = impl_f.params.len().checked_sub(arity) else { return false };
        if n_cap != captures.len() {
            return false;
        }
        let cap_tys: Vec<Ty> = impl_f.params[..n_cap].iter().map(ir_ty_to_jvm).collect();
        let lam_tys: Vec<Ty> = impl_f.params[n_cap..].iter().map(ir_ty_to_jvm).collect();
        let impl_ret = ir_ty_to_jvm(&impl_f.ret);
        // Single-exit body only: `{ effects…; Return(Some(rv)) }` with no early/non-local return — the
        // `inlining` flag makes every `Return` leave its value and fall through, so multiple returns or a
        // return nested in control flow would corrupt the stack. (Covers value AND Unit lambdas; the
        // preceding effect statements must not themselves return/branch.)
        let body_ok = matches!(impl_f.body, Some(b) if matches!(self.ir.expr(b), IrExpr::Block { stmts, value: None }
            if stmts.last().map_or(false, |&l| matches!(self.ir.expr(l), IrExpr::Return(Some(_))))
                && stmts[..stmts.len() - 1].iter().all(|&s| !matches!(self.ir.expr(s),
                    IrExpr::Return(_) | IrExpr::When { .. } | IrExpr::While { .. } | IrExpr::Try { .. }))));
        if !body_ok {
            return false;
        }
        // Capture parameters bind to the caller's *actual* slots, so a mutable capture written by the
        // lambda body propagates to the enclosing variable (`var s; recv.let { s += it }`).
        let mut cap_slots: Vec<(u16, Ty)> = Vec::with_capacity(n_cap);
        for (i, &cap) in captures.iter().enumerate() {
            let IrExpr::GetValue(v) = self.ir.expr(cap) else { return false };
            let Some(&(slot, _)) = self.slots.get(v) else { return false };
            cap_slots.push((slot, cap_tys[i]));
        }

        // Prologue: emit each NON-lambda argument and store it into its parameter slot (the lambda param
        // has no value — its loads were elided from the body segments).
        let mut off: u16 = 0;
        for (i, pty) in params.iter().enumerate() {
            let jt = *pty;
            if i != lam_idx {
                self.emit_value(args[i], code);
                store(jt, base + off, code);
            }
            off += slot_words(jt);
        }
        // `before`: the relocated body up to the invoke (lambda-object loads elided) — leaves the lambda's
        // (boxed) arguments on the stack.
        self.append_segment(&before, body.max_stack, code, arity as i32);
        // Unbox each argument to the lambda's typed parameter and store it (top = last argument). The
        // full parameter→slot map is the captures (bound to caller slots) followed by these.
        let mut param_slots: Vec<(u16, Ty)> = cap_slots;
        param_slots.extend(std::iter::repeat((0, Ty::Error)).take(arity));
        for j in (0..arity).rev() {
            let jt = lam_tys[j];
            if jt.is_primitive() {
                unbox_prim(self.cw, code, jt);
            }
            let slot = self.next_slot;
            self.next_slot += slot_words(jt);
            store(jt, slot, code);
            param_slots[n_cap + j] = (slot, jt);
        }
        code.set_stack(0);
        // The lambda body, inlined (its captures resolve to the caller's frame; mutable capture works).
        self.emit_fn_body_inline(impl_fn, &param_slots, code);
        // Box the typed result back to `Object` — the body's `after` continues from the invoke's `Object`.
        if impl_ret.is_primitive() {
            box_prim_free(self.cw, code, impl_ret);
        }
        // `after`: the relocated body past the invoke (the trailing return dropped) — yields the value.
        self.append_segment(&after, body.max_stack, code, slot_words(ty_from_descriptor_ret(descriptor)) as i32);
        true
    }

    /// Append a pre-relocated, branchless instruction segment (from `branchless_lambda_segments`) as raw
    /// bytes, reserving `body_stack` of headroom and setting the resulting stack height to `result_slots`.
    fn append_segment(&mut self, seg: &[crate::jvm::inline::Insn], body_stack: u16, code: &mut CodeBuilder, result_slots: i32) {
        let bytes = crate::jvm::inline::assemble(seg);
        let base_stack = code.stack_height();
        code.set_stack((base_stack as u16).saturating_add(body_stack)); // reserve peak headroom
        code.bytes.extend_from_slice(&bytes);
        if code.max_locals < self.next_slot {
            code.max_locals = self.next_slot;
        }
        code.set_stack((base_stack + result_slots).max(0) as u16);
    }

    /// Attempt to splice a cross-module `inline fun`'s compiled body at the call site (the bytecode
    /// inliner; the callee body comes from [`MethodBodies::body`]). Returns `true` if spliced; `false`
    /// ⇒ the caller emits an ordinary `invokestatic`, so an un-spliceable inline call is never
    /// miscompiled. The splice itself (StackMapTable relocation for branchy bodies + lambda-argument
    /// splicing) lands in the next phase — until then this always falls back.
    fn try_inline_static(&mut self, owner: &str, name: &str, descriptor: &str, args: &[u32], code: &mut CodeBuilder) -> bool {
        let Some(body) = self.bodies.body(owner, name, descriptor) else {
            return false;
        };
        // Splice the body's locals above BOTH the slot allocator's next free slot and the code's
        // high-water mark, so the spliced temporaries can never collide with a caller local (live or
        // reserved-but-unstored).
        let base = self.next_slot.max(code.max_locals);
        // Route (b): a literal lambda argument → inline its body at the body's `FunctionN.invoke` site.
        if let Some((lam_idx, lam_expr)) = args.iter().enumerate()
            .find_map(|(i, &a)| matches!(self.ir.expr(a), IrExpr::Lambda { .. }).then_some((i, a)))
        {
            return self.try_inline_lambda_call(descriptor, args, lam_idx, lam_expr, &body, base, code);
        }
        // A function-typed parameter whose argument isn't a literal lambda (a passed `Function`) isn't
        // spliceable — fall back to a normal call.
        if descriptor.contains("Lkotlin/jvm/functions/Function") {
            return false;
        }
        let ret_words = slot_words(ty_from_descriptor_ret(descriptor)) as i32;
        let top_local = base + body.max_locals;
        // Branchless single-exit body: append the spliced bytes, no frames needed.
        if let Some(insns) = crate::jvm::inline::splice_branchless(&body, descriptor, base, self.cw) {
            self.emit_operands(args, code);
            let arg_words: i32 = args.iter().map(|&a| slot_words(self.value_ty(a)) as i32).sum();
            let bytes = crate::jvm::inline::assemble(&insns);
            code.splice_inline(&bytes, body.max_stack, top_local, arg_words, ret_words);
            return true;
        }
        // Branchy body: relocate the callee's StackMapTable frames into the caller. Requires an empty
        // operand-stack baseline (so frames need no operand-stack prefix); a sub-expression inline call
        // (non-empty stack) falls back to a normal call.
        if code.stack_height() != 0 {
            return false;
        }
        let Some(bs) = crate::jvm::inline::splice_branchy(&body, descriptor, base, self.cw) else {
            return false;
        };
        self.emit_operands(args, code);
        let arg_words: i32 = args.iter().map(|&a| slot_words(self.value_ty(a)) as i32).sum();
        let splice_start = code.bytes.len();
        let prefix = self.verif_locals_upto(base);
        for (rel, body_locals, stack) in &bs.frames {
            let mut locals = prefix.clone();
            locals.extend(body_locals.iter().map(vtype_to_verif));
            let st: Vec<VerifType> = stack.iter().map(vtype_to_verif).collect();
            let l = code.new_label();
            code.bind_at(l, splice_start + rel);
            code.add_frame_if_new(l, locals, st);
        }
        code.set_needs_stackmap();
        code.splice_inline(&bs.bytes, body.max_stack, top_local, arg_words, ret_words);
        // Join frame: the redirected returns land at the continuation right after the spliced body.
        // Bind it at the *live* position (now `code.bytes.len()`) so it can never fall at `code.len()`
        // and be dropped — caller locals only (body locals are dead), with the return value on stack.
        let join = code.new_label();
        code.bind(join);
        let join_stack: Vec<VerifType> = bs.join_stack.iter().map(vtype_to_verif).collect();
        code.add_frame_if_new(join, prefix, join_stack);
        true
    }

    /// Caller-local verification types for slots `0..upto` (collapsing `long`/`double` to one entry),
    /// NOT trimming trailing `Top` — the prefix a spliced branchy body's frames are concatenated onto
    /// (the body's own locals occupy slots `upto..`).
    fn verif_locals_upto(&mut self, upto: u16) -> Vec<VerifType> {
        let mut raw = vec![VerifType::Top; upto as usize];
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
        out
    }

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
                    // Inside an inlined lambda body the result must stay on the stack (the spliced stdlib
                    // body continues) rather than returning from the enclosing method.
                    if !self.inlining {
                        emit_return(self.ret, code);
                    }
                }
                None => {
                    if !self.inlining {
                        code.ret_void();
                    }
                }
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
            IrExpr::While { cond, body, update, post_test } => {
                let start = code.new_label();
                let cont = code.new_label();
                let end = code.new_label();
                self.frame(start, vec![], code);
                code.bind(start);
                // A pre-test loop checks the condition before the body; a `do…while` skips this and
                // tests at the bottom (`cont`), so the body always runs once.
                if !post_test {
                    self.emit_value(cond, code);
                    self.frame(end, vec![], code);
                    code.ifeq(end);
                }
                // `continue` targets `cont` (run the update / bottom test); `break` targets `end`.
                self.loop_stack.push((cont, end));
                self.emit(body, code);
                // The body block restored the slot map, so framing `cont`/`start` here captures the
                // loop's outer locals — a `continue` jumping in from a deeper scope stays compatible.
                self.frame(cont, vec![], code);
                code.bind(cont);
                // The update is part of the loop, so it keeps the `break`/`continue` scope active — the
                // non-overflowing counted loop puts its `if (i == end) break` here (before the increment)
                // so a `continue` lands on it too, instead of skipping straight to the wrapping `i++`.
                if let Some(u) = update {
                    self.emit(u, code);
                }
                self.loop_stack.pop();
                if post_test {
                    // `do…while`: loop back while the condition holds, then fall through to `end`.
                    self.emit_value(cond, code);
                    self.frame(start, vec![], code);
                    code.ifne(start);
                } else {
                    self.frame(start, vec![], code);
                    code.goto(start);
                }
                self.frame(end, vec![], code);
                code.bind(end);
            }
            IrExpr::Break => {
                let (_, end) = *self.loop_stack.last().expect("break outside loop");
                code.goto(end);
            }
            IrExpr::Continue => {
                let (cont, _) = *self.loop_stack.last().expect("continue outside loop");
                code.goto(cont);
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
            // `break`/`continue` are `Nothing`-typed: in value position (e.g. `x ?: break`) they diverge
            // — emit the jump and push nothing; the consuming branch is dead past this point.
            IrExpr::Break => {
                let (_, end) = *self.loop_stack.last().expect("break outside loop");
                code.goto(end);
                return;
            }
            IrExpr::Continue => {
                let (cont, _) = *self.loop_stack.last().expect("continue outside loop");
                code.goto(cont);
                return;
            }
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
            IrExpr::New { class, args, ctor_params } => {
                let c = &self.ir.classes[*class as usize];
                let owner = c.fq_name.clone();
                // The constructor takes only the parameter fields (primary), or a secondary
                // constructor's explicit parameter types; body properties are set inside it.
                let field_tys: Vec<Ty> = match ctor_params {
                    Some(ps) => ps.iter().map(ir_ty_to_jvm).collect(),
                    None => c.fields[..c.ctor_param_count as usize].iter().map(|(_, t)| ir_ty_to_jvm(t)).collect(),
                };
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
                if args.iter().any(|a| a.is_none()) {
                    // Some arguments are omitted — invoke the `<name>$default(self, params…, mask, marker)`
                    // stub: receiver, each provided arg (or a zero placeholder for an omitted one with its
                    // mask bit set), the mask, then a null marker.
                    let args = args.clone();
                    self.emit_value(*receiver, code);
                    let mut mask = 0i32;
                    for (i, arg) in args.iter().enumerate() {
                        match arg {
                            Some(a) => self.emit_value(*a, code),
                            None => { push_zero(param_tys[i], code, self.cw); mask |= 1 << i; }
                        }
                    }
                    code.push_int(mask, self.cw);
                    code.aconst_null();
                    let mut stub_params = vec![Ty::obj(&owner)];
                    stub_params.extend(param_tys.iter().copied());
                    stub_params.push(Ty::Int);
                    stub_params.push(Ty::obj("java/lang/Object"));
                    let aw: i32 = stub_params.iter().map(|t| slot_words(*t) as i32).sum();
                    let m = self.cw.methodref(&owner, &format!("{name}$default"), &method_descriptor(&stub_params, ret));
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                    return;
                }
                let mut ops = vec![*receiver];
                ops.extend(args.iter().map(|a| a.unwrap()));
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
                Callee::Static { owner, name, descriptor, inline } => {
                    let (owner, name, descriptor, inline) = (owner.clone(), name.clone(), descriptor.clone(), *inline);
                    let args = args.clone();
                    // A cross-module `inline fun`: try to splice its compiled body here (the bytecode
                    // inliner). On any unsupported shape `try_inline_static` returns false and we emit the
                    // ordinary `invokestatic` — so an un-spliceable inline call is never miscompiled.
                    if inline && self.try_inline_static(&owner, &name, &descriptor, &args, code) {
                        return;
                    }
                    self.emit_operands(&args, code);
                    let aw: i32 = args.iter().map(|&a| slot_words(self.value_ty(a)) as i32).sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    let m = self.cw.methodref(&owner, &name, &descriptor);
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Virtual { owner, name, descriptor, interface } => {
                    let (owner, name, descriptor, interface) = (owner.clone(), name.clone(), descriptor.clone(), *interface);
                    let recv = dispatch_receiver.expect("virtual call needs a receiver");
                    let args = args.clone();
                    let mut ops = vec![recv];
                    ops.extend(args.iter().copied());
                    self.emit_operands(&ops, code);
                    let aw: i32 = args.iter().map(|&a| slot_words(self.value_ty(a)) as i32).sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    if interface {
                        let m = self.cw.interface_methodref(&owner, &name, &descriptor);
                        code.invokeinterface(m, aw, slot_words(ret) as i32);
                    } else {
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        code.invokevirtual(m, aw, slot_words(ret) as i32);
                    }
                }
            },
            IrExpr::TypeOp { op, arg, type_operand } => {
                // A primitive target of `instanceof`/`checkcast` (`x is Int`) tests the boxed wrapper.
                let jvm_ty = ir_ty_to_jvm(type_operand);
                let internal = if jvm_ty.is_primitive() {
                    crate::jvm::jvm_class_map::wrapper_internal(jvm_ty).map(|s| s.to_string()).unwrap_or_else(|| ref_internal(jvm_ty))
                } else {
                    ref_internal(jvm_ty)
                };
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
                    IrTypeOp::CastNonNull => {
                        // Null-check (throws on null) then checkcast — matching kotlinc's `as T`.
                        let kotlin_name = match type_operand {
                            IrType::Class { fq_name, .. } => fq_name.replace('/', "."),
                            _ => "kotlin.Any".to_string(),
                        };
                        code.dup();
                        code.push_string(&format!("null cannot be cast to non-null type {kotlin_name}"), self.cw);
                        let m = self.cw.methodref("kotlin/jvm/internal/Intrinsics", "checkNotNull", "(Ljava/lang/Object;Ljava/lang/String;)V");
                        code.invokestatic(m, 2, 0);
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
            IrExpr::StaticInstance { owner, ty, field } => {
                let owner_fq = self.ir.classes[*owner as usize].fq_name.clone();
                let ty_fq = self.ir.classes[*ty as usize].fq_name.clone();
                let f = self.cw.fieldref(&owner_fq, field, &format!("L{ty_fq};"));
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
            IrExpr::Lambda { impl_fn, arity, captures, sam } => {
                let f = &self.ir.functions[*impl_fn as usize];
                let impl_name = f.name.clone();
                let impl_params: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
                let impl_ret = ir_ty_to_jvm(&f.ret);
                // The impl method's parameters are the captured variables (bound at the call site)
                // followed by the lambda's own parameters. Only the latter form the SAM/instantiated
                // method types; the captures parameterize the `invokedynamic` itself.
                let n_cap = impl_params.len() - *arity as usize;
                let (cap_tys, lam_tys) = impl_params.split_at(n_cap);
                let impl_desc = jvm_descriptor(&impl_params, impl_ret);
                // For a Kotlin lambda the target is `FunctionN.invoke` (samMethodType erased to
                // `(Object,…)Object`, instantiatedMethodType the boxed actuals); for a user SAM
                // conversion the target is the interface's single method, whose descriptor is the
                // lambda's concrete signature (no erasure/boxing).
                let (iface, sam_method, sam_desc, inst_desc) = match sam {
                    Some((iface, method)) => {
                        let d = jvm_descriptor(lam_tys, impl_ret);
                        (iface.clone(), method.clone(), d.clone(), d)
                    }
                    None => {
                        let iface = format!("kotlin/jvm/functions/Function{arity}");
                        let inst_params: Vec<String> = lam_tys.iter().map(|t| boxed_descriptor(*t)).collect();
                        let inst_desc = format!("({}){}", inst_params.concat(), boxed_descriptor(impl_ret));
                        (iface, "invoke".to_string(), sam_descriptor(*arity), inst_desc)
                    }
                };
                let facade = self.facade.clone();
                let meta = self.cw.method_handle_static(
                    "java/lang/invoke/LambdaMetafactory", "metafactory", LMF_METAFACTORY_DESC);
                let sam_mt = self.cw.method_type(&sam_desc);
                let impl_mh = self.cw.method_handle_static(&facade, &impl_name, &impl_desc);
                let inst_mt = self.cw.method_type(&inst_desc);
                let bsm = self.cw.add_bootstrap(meta, vec![sam_mt, impl_mh, inst_mt]);
                // The `invokedynamic` takes the captured values and yields the interface instance.
                let cap_descs: String = cap_tys.iter().map(|t| t.descriptor()).collect();
                let indy = self.cw.invoke_dynamic(bsm, &sam_method, &format!("({cap_descs})L{iface};"));
                let cap_words: i32 = cap_tys.iter().map(|t| slot_words(*t) as i32).sum();
                for &c in captures {
                    self.emit_value(c, code);
                }
                code.invokedynamic(indy, cap_words, 1);
            }
            IrExpr::UnitInstance => {
                let f = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                code.getstatic(f, 1);
            }
            IrExpr::NotNullAssert { operand } => {
                self.emit_value(*operand, code);
                code.dup();
                let m = self.cw.methodref("kotlin/jvm/internal/Intrinsics", "checkNotNull", "(Ljava/lang/Object;)V");
                code.invokestatic(m, 1, 0);
            }
            IrExpr::Throw { operand } => {
                self.emit_value(*operand, code);
                code.athrow();
            }
            IrExpr::Vararg { element_type, elements } => {
                let et = ir_ty_to_jvm(element_type);
                let elements = elements.clone();
                code.push_int(elements.len() as i32, self.cw);
                if et.is_primitive() {
                    code.newarray(prim_newarray_atype(et));
                } else {
                    let ci = self.cw.class_ref(&ref_internal(et));
                    code.anewarray(ci);
                }
                let (op, w) = array_store_op(et);
                for (i, &el) in elements.iter().enumerate() {
                    code.dup();
                    code.push_int(i as i32, self.cw);
                    self.emit_value(el, code);
                    code.array_store(op, w);
                }
            }
            IrExpr::Try { body, catches, finally, result } => {
                let catches = catches.clone();
                let result = result.clone();
                self.emit_try(*body, &catches, *finally, &result, code);
            }
            IrExpr::NewExternal { internal, ctor_desc, args } => {
                let owner = internal.clone();
                let desc = ctor_desc.clone();
                let args = args.clone();
                // Arguments were coerced to the constructor's parameter types in lowering, so each
                // argument's `value_ty` is its parameter — the descriptor's argument-word count.
                let aw: i32 = args.iter().map(|&a| slot_words(self.value_ty(a)) as i32).sum();
                if args.iter().any(|&a| self.records_frame(a)) {
                    // A branchy argument can't run with `[new, dup]` on the stack (its merge frame
                    // would omit them) — evaluate args into temps first, then build.
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
            IrExpr::InvokeFunction { func, args, ret } => {
                let n = args.len();
                if args.iter().any(|&a| self.records_frame(a)) {
                    // A branchy argument can't run with the function value on the stack — its merge
                    // frame would omit it. Evaluate the function + args into temps first (in order),
                    // then load and box.
                    let mut all = vec![*func];
                    all.extend(args.iter().copied());
                    let temps = self.spill_to_temps(&all, code);
                    load(temps[0].1, temps[0].0, code);
                    for &(slot, t, _) in &temps[1..] {
                        load(t, slot, code);
                        self.box_prim(t, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                } else {
                    self.emit_value(*func, code);
                    for &arg in args {
                        self.emit_value(arg, code);
                        let at = self.value_ty(arg);
                        self.box_prim(at, code); // box a primitive arg to its wrapper (an Object)
                    }
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
                    Ty::Obj(internal, _) => { let ci = self.cw.class_ref(internal); code.checkcast(ci); }
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
                // A branchy operand (`when`/`try`) can't be emitted with the `StringBuilder` on the
                // stack — its merge frames would omit it. Spill such operands to temps first.
                if self.records_frame(recv) || self.records_frame(arg) {
                    let temps = self.spill_to_temps(&[recv, arg], code);
                    code.new_obj(sb);
                    code.dup();
                    let init = self.cw.methodref("java/lang/StringBuilder", "<init>", "()V");
                    code.invokespecial(init, 0, 0);
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                        self.append_top(t, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                } else {
                    code.new_obj(sb);
                    code.dup();
                    let init = self.cw.methodref("java/lang/StringBuilder", "<init>", "()V");
                    code.invokespecial(init, 0, 0);
                    self.append(recv, code);
                    self.append(arg, code);
                }
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
            _ if prim_array_elem_ty(fq).is_some() => {
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
        self.append_top(ty, code);
    }

    /// Append a value already on the operand stack (of type `ty`) to a `StringBuilder` beneath it.
    fn append_top(&mut self, ty: Ty, code: &mut CodeBuilder) {
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
            IrExpr::When { .. } | IrExpr::While { .. } | IrExpr::Try { .. } => true,
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
                (matches!(op, Lt | Le | Gt | Ge | Eq | Ne) && self.value_ty(*lhs).is_primitive())
                    || self.records_frame(*lhs) || self.records_frame(*rhs)
            }
            IrExpr::Call { dispatch_receiver, args, .. } =>
                dispatch_receiver.map_or(false, |r| self.records_frame(r)) || args.iter().any(|&a| self.records_frame(a)),
            IrExpr::MethodCall { receiver, args, .. } =>
                self.records_frame(*receiver) || args.iter().any(|a| a.map_or(false, |x| self.records_frame(x))),
            IrExpr::New { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::GetField { receiver, .. } => self.records_frame(*receiver),
            IrExpr::SetField { receiver, value, .. } => self.records_frame(*receiver) || self.records_frame(*value),
            IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => self.records_frame(*value),
            IrExpr::TypeOp { arg, .. } | IrExpr::EnumValueOf { arg, .. } => self.records_frame(*arg),
            IrExpr::NotNullAssert { operand } => self.records_frame(*operand),
            IrExpr::NewExternal { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::Throw { operand } => self.records_frame(*operand),
            IrExpr::Vararg { elements, .. } => elements.iter().any(|&a| self.records_frame(a)),
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
            BitAnd | BitOr | BitXor => {
                self.emit_value(lhs, code);
                self.emit_value(rhs, code);
                match lt {
                    Ty::Long => match op { BitAnd => code.land(), BitOr => code.lor(), BitXor => code.lxor(), _ => unreachable!() },
                    _ => match op { BitAnd => code.iand(), BitOr => code.ior(), BitXor => code.ixor(), _ => unreachable!() },
                }
            }
            Shl | Shr | Ushr => {
                self.emit_value(lhs, code);
                self.emit_value(rhs, code); // shift amount is an `Int`
                match lt {
                    Ty::Long => match op { Shl => code.lshl(), Shr => code.lshr(), Ushr => code.lushr(), _ => unreachable!() },
                    _ => match op { Shl => code.ishl(), Shr => code.ishr(), Ushr => code.iushr(), _ => unreachable!() },
                }
            }
            Lt | Le | Gt | Ge | Eq | Ne => self.emit_compare(op, lhs, rhs, code),
        }
    }

    fn emit_compare(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        let lt = self.value_ty(lhs);
        // Kotlin `==`/`!=` on reference operands is structural (`a?.equals(b)`), realized by the
        // null-safe `kotlin/jvm/internal/Intrinsics.areEqual` — the exact helper kotlinc's JVM backend
        // emits (`intrinsics/Equals.kt`), so the bytecode matches. Primitives keep the
        // `if_icmp*`/3-way-compare path below.
        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) && lt.is_reference() && self.value_ty(rhs).is_reference() {
            // Spill if rhs is branchy (`x == when{…}`) so lhs isn't live across its merge frames.
            self.emit_operands(&[lhs, rhs], code);
            let m = self.cw.methodref("kotlin/jvm/internal/Intrinsics", "areEqual", "(Ljava/lang/Object;Ljava/lang/Object;)Z");
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

    /// `try { body } catch (v: E) { … } …` (no `finally`). The body value (and each catch value) is
    /// stored into a result temp, then loaded at the merge — mirroring kotlinc. The protected region
    /// `[start, end)` covers the body+store; each catch is an exception-table handler whose frame has
    /// the caught exception on the stack and the pre-`try` locals (the result temp/catch var read as
    /// `top` there, since an exception may occur before they are assigned).
    fn emit_try(&mut self, body: u32, catches: &[crate::ir::IrCatch], finally: Option<u32>, result: &IrType, code: &mut CodeBuilder) {
        let rt = ir_ty_to_jvm(result);
        let is_stmt = matches!(rt, Ty::Unit | Ty::Nothing);
        let result_slot = if is_stmt {
            None
        } else {
            let s = self.next_slot;
            self.next_slot += slot_words(rt);
            Some(s)
        };
        const RESULT_KEY: u32 = 3_000_000;
        // A `finally` that diverges (`finally { throw }`) never falls through to `after`.
        let fin_diverges = finally.map_or(false, |f| self.diverges(f));

        let start = code.new_label();
        let end = code.new_label();
        let after = code.new_label();

        code.bind(start);
        let body_diverges = self.diverges(body);
        if is_stmt || body_diverges {
            // Statement, or a diverging body (`throw`/`return`): no value reaches the result temp.
            self.emit(body, code);
        } else {
            self.emit_value(body, code);
            store(rt, result_slot.unwrap(), code);
        }
        code.bind(end);
        let mut after_reachable = false;
        if !body_diverges {
            if let Some(f) = finally { self.emit(f, code); } // `finally` inlined on the normal path
            if !fin_diverges {
                code.goto(after);
                after_reachable = true;
            }
        }

        for c in catches {
            let handler = code.new_label();
            code.bind(handler);
            let exc_ci = self.cw.class_ref(&c.exc_internal);
            // Handler entry: the exception is the sole stack value; locals are the pre-`try` state.
            self.frame(handler, vec![VerifType::Object(exc_ci)], code);
            let exc_ty = Ty::obj(&c.exc_internal);
            let cslot = self.next_slot;
            self.next_slot += 1;
            self.slots.insert(c.var, (cslot, exc_ty));
            store(exc_ty, cslot, code);
            let cbody_diverges = self.diverges(c.body);
            if is_stmt || cbody_diverges {
                self.emit(c.body, code);
            } else {
                self.emit_value(c.body, code);
                store(rt, result_slot.unwrap(), code);
            }
            self.slots.remove(&c.var);
            if !cbody_diverges {
                if let Some(f) = finally { self.emit(f, code); } // `finally` inlined after the catch
                if !fin_diverges {
                    code.goto(after);
                    after_reachable = true;
                }
            }
            code.add_exception(start, end, handler, exc_ci);
        }

        // `finally` catch-all: any exception not handled above (in the body or a catch handler) runs
        // the `finally` then re-throws. Its protected region covers the body + all catch handlers; the
        // handler's own code is past `protected_end`, so it doesn't catch itself.
        if let Some(f) = finally {
            let protected_end = code.new_label();
            code.bind(protected_end);
            let fin_handler = code.new_label();
            code.bind(fin_handler);
            let thr_ci = self.cw.class_ref("java/lang/Throwable");
            self.frame(fin_handler, vec![VerifType::Object(thr_ci)], code);
            let thr_ty = Ty::obj("java/lang/Throwable");
            let tslot = self.next_slot;
            self.next_slot += 1;
            store(thr_ty, tslot, code);
            self.emit(f, code);
            load(thr_ty, tslot, code);
            code.athrow();
            // `catch_type` 0 = catch-all (any throwable), matching kotlinc's `finally` table entry.
            code.add_exception(start, protected_end, fin_handler, 0);
        }

        if after_reachable {
            if let Some(slot) = result_slot {
                self.slots.insert(RESULT_KEY, (slot, rt));
            }
            self.frame(after, vec![], code);
            code.bind(after);
            if let Some(slot) = result_slot {
                load(rt, slot, code);
                self.slots.remove(&RESULT_KEY);
            }
        } else {
            // Every path diverges — `after` is dead; bind it so any stray reference resolves, but emit
            // no frame (nothing reaches it) and leave no value (the `try` is `Nothing`-typed).
            code.bind(after);
        }
    }

    /// Whether emitting `e` as a value always transfers control away (returns/throws), so control
    /// never falls through past it. Used to suppress dead `goto`s and unreachable merge frames.
    fn diverges(&self, e: u32) -> bool {
        match self.ir.expr(e) {
            IrExpr::Return(_) | IrExpr::Throw { .. } | IrExpr::Break | IrExpr::Continue => true,
            IrExpr::Block { stmts, value } => match value {
                Some(v) => self.diverges(*v),
                None => stmts.last().map_or(false, |s| self.diverges(*s)),
            },
            IrExpr::When { branches } => {
                branches.iter().any(|(c, _)| c.is_none())
                    && branches.iter().all(|(_, b)| self.diverges(*b))
            }
            // A `try` diverges if its `finally` diverges, or if the body and every catch diverge (no
            // path falls through to the merge).
            IrExpr::Try { body, catches, finally, .. } => {
                finally.map_or(false, |f| self.diverges(f))
                    || (self.diverges(*body) && catches.iter().all(|c| self.diverges(c.body)))
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
        // The value type comes from a branch that *falls through* — a diverging branch (`else ->
        // return …`/`throw`) contributes nothing to the merge, so its `Unit`/`Nothing` must not make
        // the whole `when` look like a statement.
        let last = branches.iter().rev().find(|(_, b)| !self.diverges(*b)).map(|(_, b)| self.value_ty(*b)).unwrap_or(Ty::Unit);
        // A `null`/`Nothing` branch carries no concrete type and would verify-type the merge stack as
        // `top`; use a concrete fall-through branch type instead (`null` is assignable to any reference).
        if matches!(last, Ty::Null | Ty::Nothing | Ty::Error) {
            for (_, b) in branches {
                if self.diverges(*b) {
                    continue;
                }
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
            Ty::Obj(n, _) => VerifType::Object(self.cw.class_ref(n)),
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
                Callee::External(fq) if prim_array_elem_ty(fq).is_some() => Ty::array(prim_array_elem_ty(fq).unwrap()),
                Callee::External(fq) => intrinsic_ret(fq),
                Callee::Static { descriptor, .. } | Callee::Virtual { descriptor, .. } => ty_from_descriptor_ret(descriptor),
            },
            IrExpr::PrimitiveBinOp { op, lhs, .. } => match op {
                IrBinOp::Lt | IrBinOp::Le | IrBinOp::Gt | IrBinOp::Ge | IrBinOp::Eq | IrBinOp::Ne | IrBinOp::And | IrBinOp::Or => Ty::Boolean,
                _ => self.value_ty(*lhs),
            },
            IrExpr::When { branches } => self.value_ty_of_when(branches),
            IrExpr::EnumEntry { class, .. } | IrExpr::EnumValueOf { class, .. } => Ty::obj(&self.ir.classes[*class as usize].fq_name),
            IrExpr::StaticInstance { ty, .. } => Ty::obj(&self.ir.classes[*ty as usize].fq_name),
            IrExpr::EnumValues { class } => Ty::array(Ty::obj(&self.ir.classes[*class as usize].fq_name)),
            IrExpr::Block { value, .. } => value.map(|v| self.value_ty(v)).unwrap_or(Ty::Unit),
            IrExpr::TypeOp { op, type_operand, .. } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                _ => ir_ty_to_jvm(type_operand),
            },
            IrExpr::Lambda { arity, .. } => Ty::obj(&format!("kotlin/jvm/functions/Function{arity}")),
            IrExpr::InvokeFunction { ret, .. } => ir_ty_to_jvm(ret),
            IrExpr::NotNullAssert { operand } => self.value_ty(*operand),
            IrExpr::NewExternal { internal, .. } => Ty::obj(internal),
            IrExpr::Throw { .. } | IrExpr::Break | IrExpr::Continue => Ty::Nothing,
            IrExpr::Vararg { element_type, .. } => Ty::array(ir_ty_to_jvm(element_type)),
            IrExpr::Try { result, .. } => ir_ty_to_jvm(result),
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
/// Parse the return type of a JVM method descriptor (`(…)Lfoo/Bar;` → `Obj("foo/Bar")`) into a `Ty`.
fn ty_from_descriptor_ret(desc: &str) -> Ty {
    let ret = desc.rsplit(')').next().unwrap_or("V");
    ty_from_field_descriptor(ret)
}

/// Parse a single JVM field/type descriptor into a `Ty`.
fn ty_from_field_descriptor(d: &str) -> Ty {
    match d.as_bytes().first() {
        Some(b'I') => Ty::Int,
        Some(b'J') => Ty::Long,
        Some(b'Z') => Ty::Boolean,
        Some(b'B') => Ty::Byte,
        Some(b'C') => Ty::Char,
        Some(b'S') => Ty::Short,
        Some(b'F') => Ty::Float,
        Some(b'D') => Ty::Double,
        Some(b'V') => Ty::Unit,
        Some(b'L') => Ty::obj(d.strip_prefix('L').and_then(|s| s.strip_suffix(';')).unwrap_or(d)),
        Some(b'[') => Ty::array(ty_from_field_descriptor(&d[1..])),
        _ => Ty::Error,
    }
}

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
        Ty::Obj(n, _) => n.to_string(),
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
        Some(Ty::Boolean) => 4,
        Some(Ty::Char) => 5,
        Some(Ty::Float) => 6,
        Some(Ty::Double) => 7,
        Some(Ty::Byte) => 8,
        Some(Ty::Short) => 9,
        Some(Ty::Long) => 11,
        _ => 10, // Int (the only remaining primitive-array element)
    }
}

/// Element `Ty` for a `kotlin/<Prim>Array.<init>` intrinsic FqName — `None` for any other call.
/// Matches the full FqName exactly (not a suffix) so a user class named `…Array` can't be mistaken
/// for a primitive-array constructor.
fn prim_array_elem_ty(fq: &str) -> Option<Ty> {
    Some(match fq {
        "kotlin/IntArray.<init>" => Ty::Int,
        "kotlin/LongArray.<init>" => Ty::Long,
        "kotlin/DoubleArray.<init>" => Ty::Double,
        "kotlin/FloatArray.<init>" => Ty::Float,
        "kotlin/BooleanArray.<init>" => Ty::Boolean,
        "kotlin/CharArray.<init>" => Ty::Char,
        "kotlin/ByteArray.<init>" => Ty::Byte,
        "kotlin/ShortArray.<init>" => Ty::Short,
        _ => return None,
    })
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
/// Push the zero value of `t` (the placeholder for an omitted `$default` argument; the stub overwrites
/// it when the mask bit is set).
fn push_zero(t: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
    match t {
        Ty::Long => code.lconst_0(),
        Ty::Double => code.dconst_0(),
        Ty::Float => code.fconst_0(),
        Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => { code.push_int(0, cw); }
        _ => code.aconst_null(),
    }
}

fn array_store_op(elem: Ty) -> (u8, i32) {
    match elem {
        Ty::Int => (0x4f, 1), Ty::Long => (0x50, 2), Ty::Float => (0x51, 1), Ty::Double => (0x52, 2),
        Ty::Boolean | Ty::Byte => (0x54, 1), Ty::Char => (0x55, 1), Ty::Short => (0x56, 1),
        _ => (0x53, 1), // aastore
    }
}

/// `newarray` atype for a primitive element (JVMS Table 6.5.newarray-A).
fn prim_newarray_atype(elem: Ty) -> u8 {
    match elem {
        Ty::Boolean => 4, Ty::Char => 5, Ty::Float => 6, Ty::Double => 7,
        Ty::Byte => 8, Ty::Short => 9, Ty::Long => 11,
        _ => 10, // int
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
        // The JVM representation of a function type is `kotlin/jvm/functions/FunctionN`.
        IrType::Function { params, .. } => Ty::obj(&format!("kotlin/jvm/functions/Function{}", params.len())),
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

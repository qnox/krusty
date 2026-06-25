//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of the backend-agnostic IR — it maps
//! Kotlin FqNames to JVM descriptors here (the IR never carries descriptors). Covers the core
//! subset (functions, simple classes); shares `CodeBuilder`/`ClassWriter` with the AST emitter.

use std::collections::HashMap;

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrType, IrTypeOp};
use crate::jvm::classfile::{ClassWriter, CodeBuilder, Label, VerifType};
use crate::jvm::inline::MethodBodies;
use crate::jvm::names::method_descriptor;
use crate::types::Ty;

// Set when the emitter hits a `must_inline` (non-public `@InlineOnly`) call it cannot splice — e.g. a
// branchy `require`/`check` on a non-empty operand stack. There is no legal `invokestatic` fallback for
// such a callee, so the whole file is skipped (`emit_all` returns `None`) rather than miscompiled.
thread_local! {
    static INLINE_BAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A built `@kotlin.Metadata` annotation for a file facade: the `k`/`mv`/`xi` ints and the `d1` (the
/// encoded protobuf, one byte per `char`) / `d2` (string table) arrays. Attached to the facade class so
/// another Kotlin/krusty compilation can resolve its top-level declarations — in particular reading the
/// `IS_SUSPEND` flag + logical signature of a `suspend fun`.
#[derive(Clone)]
pub struct KotlinMetadata {
    pub k: i32,
    pub mv: Vec<i32>,
    pub xi: i32,
    pub d1: Vec<String>,
    pub d2: Vec<String>,
}

/// Emit a whole IR file: the facade class of top-level `static` functions, plus one `.class` per
/// `IrClass`. Returns `(internal_name, bytes)` for each, or `None` when the IR uses a construct the
/// JVM backend can't represent (so every emission path skips it rather than miscompiling).
pub fn emit_all(
    ir: &IrFile,
    facade: &str,
    bodies: &dyn MethodBodies,
    metadata: Option<&KotlinMetadata>,
) -> Option<Vec<(String, Vec<u8>)>> {
    // Default: no per-class `@Metadata` — krusty-core emit is byte-identical to before (the
    // `bytecode_parity_e2e` gate compares classes byte-for-byte vs kotlinc, so the default path must
    // stay untouched). A caller that needs cross-module class metadata (krusty-compose's LibraryBinary
    // modules) uses [`emit_all_with_class_meta`].
    emit_all_with_class_meta(ir, facade, bodies, metadata, &|_| None)
}

/// Like [`emit_all`], but `class_meta` may supply a per-class `@kotlin.Metadata` (keyed by the class's
/// internal/fq name) attached to that emitted class. This lets a separately-compiled module expose its
/// classes' Kotlin signatures (member source params, etc.) so a dependent module resolves them — the
/// cross-module analogue of the facade `metadata`. OPT-IN: the default [`emit_all`] passes a provider
/// that returns `None` for every class, so krusty-core's emit is unchanged.
pub fn emit_all_with_class_meta(
    ir: &IrFile,
    facade: &str,
    bodies: &dyn MethodBodies,
    metadata: Option<&KotlinMetadata>,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
) -> Option<Vec<(String, Vec<u8>)>> {
    if !jvm_can_emit(ir) {
        return None;
    }
    INLINE_BAIL.with(|b| b.set(false));
    let mut out = Vec::new();
    // Facade: the static top-level functions (those with no dispatch receiver). A function that BELONGS
    // to a class — including a `static` member like the serialization plugin's `serializer()` accessor,
    // which has no dispatch receiver — is emitted on its class (below), NOT here; otherwise two classes'
    // same-signature statics (`C.serializer()`/`D.serializer()`) would collide on the facade.
    let class_member_fids: std::collections::HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().copied())
        .collect();
    let mut cw = ClassWriter::new(facade, "java/lang/Object");
    for (i, f) in ir.functions.iter().enumerate() {
        if class_member_fids.contains(&(i as u32)) {
            continue;
        }
        if f.dispatch_receiver.is_some() || f.body.is_none() {
            continue;
        }
        emit_method(ir, i as u32, facade, facade, &mut cw, false, bodies);
    }
    emit_statics(ir, facade, &mut cw, bodies);
    if let Some(m) = metadata {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    out.push((facade.to_string(), cw.finish()));
    // Each class — with its optional `@Metadata` (the provider returns `None` for the default emit).
    for c in &ir.classes {
        let cm = class_meta(&c.fq_name);
        out.push((
            c.fq_name.clone(),
            emit_class(ir, c, facade, bodies, cm.as_ref()),
        ));
    }
    if INLINE_BAIL.with(|b| b.get()) {
        return None; // an un-spliceable `must_inline` call — skip the file, never miscompile
    }
    Some(out)
}

/// Whether the JVM backend can represent this IR. The JVM stdlib provides fixed-arity
/// `kotlin/jvm/functions/Function0..22`; a function type or lambda of higher arity needs a different
/// vararg representation krusty doesn't emit, so such a file is skipped — never miscompiled. This is a
/// JVM constraint (the language allows any arity), so it lives in the JVM emitter, not common lowering.
/// Map every `IrExpr::Variable`'s declaration index → its JVM type, across the whole file. `value_ty`
/// consults this so a `GetValue` of a slot whose `Variable` hasn't been emit-registered yet (e.g. an
/// inline-expansion result/`this` temp queried by a comparison before its block emits) still types
/// correctly, instead of falling back to `Ty::Error` and picking the wrong (reference) operator path.
fn collect_var_types(ir: &IrFile) -> HashMap<u32, Ty> {
    let mut m = HashMap::new();
    for e in &ir.exprs {
        if let IrExpr::Variable { index, ty, .. } = e {
            m.insert(*index, ir_ty_to_jvm(ty));
        }
    }
    m
}

fn jvm_can_emit(ir: &IrFile) -> bool {
    fn ty_ok(t: &IrType) -> bool {
        match t {
            IrType::Function { params, ret, .. } => {
                params.len() <= 22 && params.iter().all(ty_ok) && ty_ok(ret)
            }
            IrType::Class { type_args, .. } => type_args.iter().all(ty_ok),
            _ => true,
        }
    }
    if ir
        .functions
        .iter()
        .any(|f| !ty_ok(&f.ret) || !f.params.iter().all(ty_ok))
    {
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
/// `VerifType`. `Uninitialized` types shouldn't reach here (`splice_unified` bails on them).
/// A method's `StackMapTable` frames resolved to byte offsets: `(offset, locals, stack)` each.
type ResolvedFrames = Vec<(usize, Vec<VerifType>, Vec<VerifType>)>;

/// The internal class name to `checkcast` a value to when narrowing an erased `Object` to `ty` — or
/// `None` when no narrowing is needed (`Object`/`Any`, a primitive, `Unit`/`Nothing`).
fn checkcast_internal(ty: Ty) -> Option<String> {
    match ty {
        Ty::String => Some("java/lang/String".to_string()),
        Ty::Array(_) => Some(ty.descriptor()),
        Ty::Obj(n, _) if n != "java/lang/Object" && n != "kotlin/Any" => Some(n.to_string()),
        _ => None,
    }
}

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

/// Expand a COLLAPSED frame-locals list (long/double = one entry) to SLOT-indexed (long/double = the
/// type + a trailing `Top` filler), so per-slot overlays line up.
fn expand_collapsed_locals(collapsed: &[VerifType]) -> Vec<VerifType> {
    let mut out = Vec::with_capacity(collapsed.len());
    for v in collapsed {
        let wide = matches!(v, VerifType::Long | VerifType::Double);
        out.push(v.clone());
        if wide {
            out.push(VerifType::Top);
        }
    }
    out
}

/// Collapse a SLOT-indexed locals list back to the JVM `StackMapTable` form (long/double = one entry,
/// its second slot dropped).
fn collapse_locals(slots: &[VerifType]) -> Vec<VerifType> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < slots.len() {
        let wide = matches!(slots[i], VerifType::Long | VerifType::Double);
        out.push(slots[i].clone());
        i += if wide { 2 } else { 1 };
    }
    out
}

/// The constant-pool index for a `const val`'s `ConstantValue` attribute when its initializer is a
/// compile-time literal; `None` otherwise (then the field is initialized in `<clinit>` as before).
fn const_value_idx(ir: &IrFile, init: crate::ir::ExprId, cw: &mut ClassWriter) -> Option<u16> {
    use crate::ir::{IrConst, IrExpr};
    match ir.expr(init) {
        IrExpr::Const(c) => Some(match c {
            IrConst::Boolean(b) => cw.const_int(*b as i32),
            IrConst::Byte(v) => cw.const_int(*v as i32),
            IrConst::Short(v) => cw.const_int(*v as i32),
            IrConst::Int(v) => cw.const_int(*v),
            IrConst::Char(c) => cw.const_int(*c as i32),
            IrConst::Long(v) => cw.const_long(*v),
            IrConst::Float(v) => cw.const_float(*v),
            IrConst::Double(v) => cw.const_double(*v),
            IrConst::String(s) => cw.const_string(s),
            IrConst::Null => return None,
        }),
        _ => None,
    }
}

/// Whether `init` is a `ConstantValue`-eligible literal (mirrors [`const_value_idx`] without interning).
fn const_value_idx_peek(ir: &IrFile, init: crate::ir::ExprId) -> bool {
    matches!(ir.expr(init), crate::ir::IrExpr::Const(c) if !matches!(c, crate::ir::IrConst::Null))
}

fn emit_statics(ir: &IrFile, facade: &str, cw: &mut ClassWriter, bodies: &dyn MethodBodies) {
    if ir.statics.is_empty() {
        return;
    }
    for s in &ir.statics {
        // kotlinc: `const val` → `public static final`; a plain `val` → `private static final`; a `var`
        // → `private static` (mutated through the synthesized setter). The private field is read/written
        // directly only from within the facade; other classes go through the get/set accessors.
        let acc = if s.is_const {
            0x0019 // PUBLIC | STATIC | FINAL
        } else if s.is_var {
            0x000A // PRIVATE | STATIC
        } else {
            0x001A // PRIVATE | STATIC | FINAL
        };
        let desc = ir_ty_to_jvm(&s.ty).descriptor();
        // A `const val` initialized by a compile-time literal carries a `ConstantValue` attribute (the
        // JVM initializes the field; its `<clinit>` store is omitted below) — byte-identical to kotlinc.
        if s.is_const {
            if let Some(cv) = const_value_idx(ir, s.init, cw) {
                cw.add_field_const(acc, &s.name, &desc, cv);
                continue;
            }
        }
        cw.add_field(acc, &s.name, &desc);
    }
    // Accessors: a plain top-level `val`/`var` gets a `public static final getX()` (and `setX()` for a
    // `var`), so other classes read/write it the way kotlinc compiles cross-file property access. A
    // `const val` is `public static final` with no accessor (kotlinc inlines const reads).
    for s in &ir.statics {
        if s.is_const {
            continue;
        }
        let jt = ir_ty_to_jvm(&s.ty);
        let desc = jt.descriptor();
        let mut g = CodeBuilder::new(0);
        let fref = cw.fieldref(facade, &s.name, &desc);
        g.getstatic(fref, slot_words(jt) as i32);
        emit_return(jt, &mut g);
        g.ensure_locals(0);
        g.link();
        cw.add_method(0x0019, &prop_getter_name(&s.name), &format!("(){desc}"), &g);
        if s.is_var {
            let words = slot_words(jt);
            let mut st = CodeBuilder::new(words);
            // kotlinc guards a non-null reference setter parameter with checkNotNullParameter("<set-?>").
            if jt.is_reference() && !ir_ty_nullable(&s.ty) {
                st.aload(0);
                st.push_string("<set-?>", cw);
                let m = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                st.invokestatic(m, 2, 0);
            }
            load(jt, 0, &mut st);
            let fref = cw.fieldref(facade, &s.name, &desc);
            st.putstatic(fref, slot_words(jt) as i32);
            st.ret_void();
            st.ensure_locals(words);
            st.link();
            cw.add_method(
                0x0019,
                &prop_setter_name(&s.name),
                &format!("({desc})V"),
                &st,
            );
        }
    }
    let mut e = Emitter {
        ir,
        cw,
        bodies,
        owner: facade.to_string(),
        facade: facade.to_string(),
        slots: HashMap::new(),
        var_types: collect_var_types(ir),
        next_slot: 0,
        ret: Ty::Unit,
        loop_stack: Vec::new(),
    };
    let mut code = CodeBuilder::new(0);
    let mut any_init = false;
    for s in &ir.statics {
        // A `const val` folded into a `ConstantValue` attribute (literal init) is initialized by the JVM
        // — kotlinc emits no `<clinit>` store for it, so skip it here too (byte-identical).
        if s.is_const && const_value_idx_peek(ir, s.init) {
            continue;
        }
        any_init = true;
        e.emit_value(s.init, &mut code);
        let jt = ir_ty_to_jvm(&s.ty);
        let fref = e.cw.fieldref(facade, &s.name, &jt.descriptor());
        code.putstatic(fref, slot_words(jt) as i32);
    }
    // When every static is a `ConstantValue`-folded `const val`, there is nothing to initialize —
    // kotlinc emits NO `<clinit>` at all (not an empty one), so skip it.
    if !any_init {
        return;
    }
    code.ret_void();
    code.ensure_locals(e.next_slot);
    code.link();
    e.cw.add_method(0x0008 /* STATIC */, "<clinit>", "()V", &code);
}

fn emit_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    bodies: &dyn MethodBodies,
    class_meta: Option<&KotlinMetadata>,
) -> Vec<u8> {
    if !c.enum_entries.is_empty() {
        return emit_enum_class(ir, c, facade, bodies);
    }
    if c.is_interface {
        return emit_interface_class(ir, c, facade, bodies, class_meta);
    }
    if let Some(user_tys) = &c.enum_entry_of {
        return emit_enum_entry_subclass(ir, c, facade, bodies, user_tys);
    }
    if c.prop_ref.is_some() {
        return emit_prop_ref_class(c);
    }
    if c.func_ref.is_some() {
        return emit_func_ref_class(c, facade);
    }
    let mut cw = ClassWriter::new(&c.fq_name, &c.superclass);
    // Access: an extended or abstract class must not be `final`; a class with an abstract method
    // (body `None`) is `ACC_ABSTRACT`.
    let extended = ir.classes.iter().any(|o| o.superclass == c.fq_name);
    let has_abstract = c
        .methods
        .iter()
        .any(|&fid| ir.functions[fid as usize].body.is_none());
    let mut access = 0x0001 | 0x0020; // PUBLIC | SUPER
    if !extended && !has_abstract {
        access |= 0x0010;
    } // FINAL
    if has_abstract {
        access |= 0x0400;
    } // ABSTRACT
    cw.set_access(access);
    if let Some(s) = ir
        .class_signatures
        .get(&c.fq_name)
        .and_then(jvm_class_signature)
    {
        cw.set_signature(&s);
    }
    for itf in &c.interfaces {
        cw.add_interface(itf);
    }
    // Public fields (the IR slice reads them cross-class directly; kotlinc uses private + getters —
    // an ABI refinement, not a runtime difference).
    // Backing fields are private; access goes through the synthesized `getX()`/`setX()` accessors
    // (kotlinc does the same) — for both normal classes and objects.
    for (i, (name, ty)) in c.fields.iter().enumerate() {
        // Map the field's (platform-neutral) visibility to JVM access flags: a `private` field →
        // `ACC_PRIVATE` (the default — Kotlin backing fields are private, reached via accessors); a
        // non-private field → `ACC_PUBLIC` (read/written cross-class, e.g. a coroutine continuation's
        // `result`/`label`). `field_private` empty ⇒ all private.
        let private = c.field_private.get(i).copied().unwrap_or(true);
        let acc = (if private { 0x0002 } else { 0x0001 })
            | if c.field_final.get(i).copied().unwrap_or(false) {
                0x0010
            } else {
                0
            };
        // A field typed by a bare type parameter (`val a: A`) carries a `Signature` (`TA;`), like kotlinc.
        let field_sig = ir
            .field_signatures
            .get(&c.fq_name)
            .and_then(|fs| fs.iter().find(|(fname, _)| fname == name))
            .map(|(_, tp)| format!("T{tp};"));
        cw.add_field_sig(
            acc,
            name,
            &ir_ty_to_jvm(ty).descriptor(),
            field_sig.as_deref(),
        );
    }
    // Constructor: super(); store each ctor *parameter* into its field; then run `init_body`
    // (body-property initializers + `init {}` blocks). Fields past `ctor_param_count` are body
    // properties — not parameters — so the descriptor covers only the leading parameter fields.
    let field_tys: Vec<Ty> = c.fields.iter().map(|(_, t)| ir_ty_to_jvm(t)).collect();
    // The constructor takes ALL primary-ctor params (`ctor_args`), in declaration order — `val`/`var`
    // params back a field, plain params are arguments only. (Synthesized classes have empty `ctor_args`
    // and fall back to the leading `ctor_param_count` fields.)
    let param_tys: Vec<Ty> = if c.ctor_args.is_empty() {
        field_tys[..c.ctor_param_count as usize].to_vec()
    } else {
        c.ctor_args.iter().map(|(t, _)| ir_ty_to_jvm(t)).collect()
    };
    // For a generic class, the `<init>` carries a `Signature` whose type-parameter params read `T<tp>;`
    // (`class Box<T>(var a: T)` → `(TT;)V`) — kotlinc does the same. No `<…>` prefix: the constructor
    // uses the class's type parameters, declares none. `None` (no attr) when no param is type-parameter-typed.
    let ctor_signature: Option<String> = ir.field_signatures.get(&c.fq_name).and_then(|ftp| {
        let is_field: Vec<bool> = if c.ctor_args.is_empty() {
            vec![true; param_tys.len()]
        } else {
            c.ctor_args.iter().map(|(_, f)| *f).collect()
        };
        let mut sig = String::from("(");
        let mut any = false;
        let mut field_i = 0usize;
        for (i, t) in param_tys.iter().enumerate() {
            if is_field.get(i).copied().unwrap_or(true) {
                let fname = c.fields.get(field_i).map(|(n, _)| n.as_str()).unwrap_or("");
                if let Some((_, tp)) = ftp.iter().find(|(f, _)| f == fname) {
                    sig.push_str(&format!("T{tp};"));
                    any = true;
                } else {
                    sig.push_str(&t.descriptor());
                }
                field_i += 1;
            } else {
                sig.push_str(&t.descriptor());
            }
        }
        sig.push_str(")V");
        any.then_some(sig)
    });
    // A class with NO primary constructor emits no primary `<init>` — every `<init>` comes from a
    // secondary constructor (below). Otherwise emit the primary `<init>` here.
    if c.has_primary_ctor {
        let params_words: u16 = param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut ctor = CodeBuilder::new(1 + params_words);
        // The superclass constructor's parameter types (empty for the erased top type — the front end
        // names it `kotlin/Any`, which this backend maps to `java/lang/Object`).
        let mut super_param_tys: Vec<Ty> =
            if crate::jvm::jvm_class_map::to_jvm_internal(&c.superclass) == "java/lang/Object" {
                Vec::new()
            } else {
                ir.classes
                    .iter()
                    .find(|sc| sc.fq_name == c.superclass)
                    .map(|sc| {
                        if sc.ctor_args.is_empty() {
                            sc.fields[..sc.ctor_param_count as usize]
                                .iter()
                                .map(|(_, t)| ir_ty_to_jvm(t))
                                .collect()
                        } else {
                            sc.ctor_args.iter().map(|(t, _)| ir_ty_to_jvm(t)).collect()
                        }
                    })
                    .unwrap_or_default()
            };
        let max_slot;
        let mut init_diverges = false;
        {
            let mut e = Emitter {
                ir,
                cw: &mut cw,
                bodies,
                owner: c.fq_name.clone(),
                facade: facade.to_string(),
                slots: HashMap::new(),
                var_types: collect_var_types(ir),
                next_slot: 1 + params_words,
                ret: Ty::Unit,
                loop_stack: Vec::new(),
            };
            e.slots.insert(0, (0, Ty::obj(&c.fq_name)));
            let mut s = 1u16;
            for (vi, t) in param_tys.iter().enumerate() {
                e.slots.insert(vi as u32 + 1, (s, *t));
                s += slot_words(*t);
            }
            // A classpath superclass (not an IR class) with `super(args)`: the IR-class lookup above
            // found no parameter types, so derive the super constructor's descriptor from the argument
            // expressions themselves (e.g. a synthesized coroutine continuation's `super(completion)`).
            if super_param_tys.is_empty() && !c.super_args.is_empty() {
                super_param_tys = c.super_args.iter().map(|&a| e.value_ty(a)).collect();
            }
            // kotlinc guards each non-null reference constructor parameter with checkNotNullParameter at
            // the very start of `<init>` — before the super() call.
            let ctor_checks = c.ctor_param_checks.clone();
            for (i, check) in ctor_checks.iter().enumerate() {
                if let Some(name) = check {
                    if let Some(&(slot, _)) = e.slots.get(&(i as u32 + 1)) {
                        ctor.aload(slot);
                        ctor.push_string(name, e.cw);
                        let m = e.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "checkNotNullParameter",
                            "(Ljava/lang/Object;Ljava/lang/String;)V",
                        );
                        ctor.invokestatic(m, 2, 0);
                    }
                }
            }
            // `super(args)` — `this` is loaded first, so spill any branchy arg to temps before it.
            let super_args = c.super_args.clone();
            if super_args.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&super_args, &mut ctor);
                ctor.aload(0);
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut ctor);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                ctor.aload(0);
                for &a in &super_args {
                    e.emit_value(a, &mut ctor);
                }
            }
            let aw: i32 = super_param_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let super_init = e.cw.methodref(
                &c.superclass,
                "<init>",
                &method_descriptor(&super_param_tys, Ty::Unit),
            );
            ctor.invokespecial(super_init, aw, 0);
            // Store this class's own primary-constructor parameter fields: each `val`/`var` param's arg is
            // stored to its field (the property fields are `fields[0..]` in declaration order among params);
            // a plain param is skipped (it stays a local for the initializer body). `is_field` flags come
            // from `ctor_args`; a synthesized class (empty `ctor_args`) stores all leading param fields.
            let mut slot = 1u16;
            let mut field_i = 0usize;
            let is_field: Vec<bool> = if c.ctor_args.is_empty() {
                vec![true; param_tys.len()]
            } else {
                c.ctor_args.iter().map(|(_, f)| *f).collect()
            };
            for (i, t) in param_tys.iter().enumerate() {
                if is_field.get(i).copied().unwrap_or(true) {
                    let name = &c.fields[field_i].0;
                    ctor.aload(0);
                    load(*t, slot, &mut ctor);
                    let fref = e.cw.fieldref(&c.fq_name, name, &t.descriptor());
                    ctor.putfield(fref, slot_words(*t) as i32);
                    field_i += 1;
                }
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
        let ctor_access = if c.is_object {
            0x0002
        } else if c.is_companion {
            0x0000
        } else {
            0x0001
        };
        cw.add_method_sig(
            ctor_access,
            "<init>",
            &method_descriptor(&param_tys, Ty::Unit),
            &ctor,
            ctor_signature.as_deref(),
        );
    } // end `if c.has_primary_ctor`

    // Secondary constructors: each `<init>(p)` delegates (via `this(…)` to an own `<init>`, or via
    // `super(…)` to the base `<init>`) then runs its body. A `super(…)`-reaching ctor's `body` already
    // has the class init steps prepended (the lowering does that). `this` is slot 0, parameters follow.
    for sc in &c.secondary_ctors {
        let sc_param_tys: Vec<Ty> = sc.params.iter().map(ir_ty_to_jvm).collect();
        let sc_words: u16 = sc_param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut sctor = CodeBuilder::new(1 + sc_words);
        let sec_max;
        let mut sec_diverges = false;
        {
            let mut e = Emitter {
                ir,
                cw: &mut cw,
                bodies,
                owner: c.fq_name.clone(),
                facade: facade.to_string(),
                slots: HashMap::new(),
                var_types: collect_var_types(ir),
                next_slot: 1 + sc_words,
                ret: Ty::Unit,
                loop_stack: Vec::new(),
            };
            e.slots.insert(0, (0, Ty::obj(&c.fq_name)));
            let mut s = 1u16;
            for (vi, t) in sc_param_tys.iter().enumerate() {
                e.slots.insert(vi as u32 + 1, (s, *t));
                s += slot_words(*t);
            }
            // Delegation target: `this(…)` → an own `<init>(target_params)`; `super(…)` → the base
            // `<init>(super_params)`. `this` is loaded first, so spill any branchy arg to a temp before.
            use crate::ir::CtorDelegateTarget;
            let (target_class, target_jvm_tys): (String, Vec<Ty>) = match &sc.delegate {
                // `this(…)` targets an own `<init>`. In a class WITH a primary ctor that target IS the
                // primary, whose live signature is `param_tys` (already value-class-rewritten) — use it
                // rather than the lower-time `target_params` (which predates the value-class pass).
                CtorDelegateTarget::This { target_params } => (
                    c.fq_name.clone(),
                    if c.has_primary_ctor {
                        param_tys.clone()
                    } else {
                        target_params.iter().map(ir_ty_to_jvm).collect()
                    },
                ),
                // `super(…)` targets the base `<init>`, whose signature is read LIVE from the base
                // class's (post-transform) ctor — mirrors the primary path, so any IR→IR pass that
                // rewrote the base ctor's parameter types (e.g. value-class erasure) is reflected here.
                CtorDelegateTarget::Super => {
                    let owner =
                        crate::jvm::jvm_class_map::to_jvm_internal(&c.superclass).to_string();
                    let tys: Vec<Ty> = if owner == "java/lang/Object" {
                        Vec::new()
                    } else {
                        ir.classes
                            .iter()
                            .find(|sc| sc.fq_name == c.superclass)
                            .map(|sc| {
                                if sc.ctor_args.is_empty() {
                                    sc.fields[..sc.ctor_param_count as usize]
                                        .iter()
                                        .map(|(_, t)| ir_ty_to_jvm(t))
                                        .collect()
                                } else {
                                    sc.ctor_args.iter().map(|(t, _)| ir_ty_to_jvm(t)).collect()
                                }
                            })
                            .unwrap_or_default()
                    };
                    (owner, tys)
                }
            };
            let dargs = sc.delegate_args.clone();
            if dargs.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&dargs, &mut sctor);
                sctor.aload(0);
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut sctor);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                sctor.aload(0);
                for &a in &dargs {
                    e.emit_value(a, &mut sctor);
                }
            }
            let aw: i32 = target_jvm_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let delegate_init = e.cw.methodref(
                &target_class,
                "<init>",
                &method_descriptor(&target_jvm_tys, Ty::Unit),
            );
            sctor.invokespecial(delegate_init, aw, 0);
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
        cw.add_method(
            0x0001,
            "<init>",
            &method_descriptor(&sc_param_tys, Ty::Unit),
            &sctor,
        );
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
            // A `static` member (e.g. a value class's `box-impl`/`constructor-impl`) emits with no
            // `this` slot; an ordinary member is an instance method.
            emit_method(ir, fid, &c.fq_name, facade, &mut cw, !f.is_static, bodies);
        } else {
            let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
            let ret = ir_ty_to_jvm(&f.ret);
            cw.add_abstract_method(
                0x0001 | 0x0400,
                &f.name,
                &method_descriptor(&param_tys, ret),
            );
        }
        // A method with default-valued parameters gets a `<name>$default(self, params…, mask, marker)`
        // synthetic stub (the JVM realization of default arguments).
        if let Some(defaults) = ir.fn_param_defaults.get(&fid) {
            emit_default_stub(ir, fid, &c.fq_name, facade, &mut cw, defaults, bodies);
        }
    }
    emit_bridges(c, &mut cw);
    if let Some(m) = class_meta {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
}

/// Emit a synthesized enum-entry subclass (`Enum$ENTRY extends Enum`) for an entry with a body: a
/// package-private `final` class with one constructor `(String name, int ordinal, <user fields>)V`
/// that delegates to the enum's `(String,int,<user>)V` constructor, plus the entry's overriding
/// methods. It has no fields of its own — overrides read the enum's fields via the inherited `this`.
fn emit_enum_entry_subclass(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    bodies: &dyn MethodBodies,
    user_tys: &[IrType],
) -> Vec<u8> {
    let mut cw = ClassWriter::new(&c.fq_name, &c.superclass);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)

    // Entry-body PROPERTIES are private backing fields (read via synthesized getters, like kotlinc).
    for (i, (name, ty)) in c.fields.iter().enumerate() {
        let acc = 0x0002
            | if c.field_final.get(i).copied().unwrap_or(false) {
                0x0010
            } else {
                0
            };
        cw.add_field(acc, name, &ir_ty_to_jvm(ty).descriptor());
    }

    // Constructor: `(String, int, <user>)V` → `super(name, ordinal, <user>)`, then the property
    // initializers (`this.<prop> = <init>`, from `init_body`).
    let user_jvm: Vec<Ty> = user_tys.iter().map(ir_ty_to_jvm).collect();
    let ctor_params: Vec<Ty> = std::iter::once(Ty::String)
        .chain(std::iter::once(Ty::Int))
        .chain(user_jvm.iter().copied())
        .collect();
    let ctor_words: u16 = ctor_params.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + ctor_words);
    ctor.aload(0);
    let mut slot = 1u16;
    for t in &ctor_params {
        load(*t, slot, &mut ctor);
        slot += slot_words(*t);
    }
    let super_init = cw.methodref(
        &c.superclass,
        "<init>",
        &method_descriptor(&ctor_params, Ty::Unit),
    );
    let argw: i32 = ctor_params.iter().map(|t| slot_words(*t) as i32).sum();
    ctor.invokespecial(super_init, argw, 0);
    let mut ctor_max = 1 + ctor_words;
    if let Some(init_body) = c.init_body {
        let mut e = Emitter {
            ir,
            cw: &mut cw,
            bodies,
            owner: c.fq_name.clone(),
            facade: facade.to_string(),
            slots: HashMap::new(),
            var_types: collect_var_types(ir),
            next_slot: 1 + ctor_words,
            ret: Ty::Unit,
            loop_stack: Vec::new(),
        };
        e.slots.insert(0, (0, Ty::obj(&c.fq_name))); // `this`
        e.emit(init_body, &mut ctor);
        ctor_max = e.next_slot;
    }
    ctor.ret_void();
    ctor.ensure_locals(ctor_max);
    ctor.link();
    cw.add_method(
        0x0000,
        "<init>",
        &method_descriptor(&ctor_params, Ty::Unit),
        &ctor,
    );

    // The overriding methods + synthesized property getters.
    for &fid in &c.methods {
        emit_method(ir, fid, &c.fq_name, facade, &mut cw, true, bodies);
    }
    cw.finish()
}

/// Emit a synthesized property-reference singleton (`Type$prop$N extends PropertyReference1Impl`):
/// a package-private `final` class with a `public static final INSTANCE`, a constructor
/// `super(owner.class, name, "getName()desc", 0)`, a `get(Object)Object` override that reads
/// `((Owner) it).getName()` (boxing a primitive), and a `<clinit>` that builds the singleton. `.name`
/// is inherited from `PropertyReference1Impl` (returns the constructor's name argument).
fn emit_prop_ref_class(c: &crate::ir::IrClass) -> Vec<u8> {
    let pr = c.prop_ref.as_ref().unwrap();
    if pr.bound {
        return emit_bound_prop_ref_class(c, pr);
    }
    let fq = c.fq_name.clone();
    let self_desc = format!("L{fq};");
    let mut cw = ClassWriter::new(&fq, &c.superclass);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)
    cw.add_field(0x0019, "INSTANCE", &self_desc); // PUBLIC | STATIC | FINAL

    let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
    let getter_desc = format!("(){}", prop_jvm.descriptor());
    let signature = format!("{}{}", pr.getter_name, getter_desc); // e.g. "getX()I"

    // `<init>()V`: super(owner.class, "name", "getName()desc", 0).
    let mut ctor = CodeBuilder::new(1);
    ctor.aload(0);
    ctor.ldc_class(&pr.owner_internal, &mut cw);
    ctor.push_string(&pr.prop_name, &mut cw);
    ctor.push_string(&signature, &mut cw);
    ctor.push_int(0, &mut cw);
    let sup = cw.methodref(
        &c.superclass,
        "<init>",
        "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
    );
    ctor.invokespecial(sup, 4, 0);
    ctor.ret_void();
    ctor.ensure_locals(1);
    ctor.link();
    cw.add_method(0x0000, "<init>", "()V", &ctor);

    // `get(Object)Object`: ((Owner) it).getName(), boxed if primitive.
    let mut get = CodeBuilder::new(2);
    get.aload(1);
    let owner_ref = cw.class_ref(&pr.owner_internal);
    get.checkcast(owner_ref);
    let gref = cw.methodref(&pr.owner_internal, &pr.getter_name, &getter_desc);
    get.invokevirtual(gref, 0, slot_words(prop_jvm) as i32);
    if prop_jvm.is_primitive() {
        box_prim_free(&mut cw, &mut get, prop_jvm);
    }
    get.areturn();
    get.ensure_locals(2);
    get.link();
    cw.add_method(
        0x0001,
        "get",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        &get,
    );

    // `<clinit>`: INSTANCE = new.
    let mut clinit = CodeBuilder::new(0);
    let cls = cw.class_ref(&fq);
    clinit.new_obj(cls);
    clinit.dup();
    let init = cw.methodref(&fq, "<init>", "()V");
    clinit.invokespecial(init, 0, 0);
    let fref = cw.fieldref(&fq, "INSTANCE", &self_desc);
    clinit.putstatic(fref, 1);
    clinit.ret_void();
    clinit.ensure_locals(0);
    clinit.link();
    cw.add_method(0x0008, "<clinit>", "()V", &clinit);
    cw.finish()
}

/// Emit a bound property-reference (`obj::prop` → `PropertyReference0Impl` subclass): a constructor
/// `(Object receiver)` delegating to `super(receiver, owner.class, name, "getName()desc", 0)` (the base
/// stores the receiver), and a no-arg `get()` reading `((Owner) this.receiver).getName()`. Constructed
/// per use with the captured receiver — no `INSTANCE` singleton.
fn emit_bound_prop_ref_class(c: &crate::ir::IrClass, pr: &crate::ir::PropRef) -> Vec<u8> {
    let fq = c.fq_name.clone();
    let mut cw = ClassWriter::new(&fq, &c.superclass);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER

    let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
    let getter_desc = format!("(){}", prop_jvm.descriptor());
    let signature = format!("{}{}", pr.getter_name, getter_desc);

    // `<init>(Object)V`: super(receiver, owner.class, name, "getName()desc", 0).
    let mut ctor = CodeBuilder::new(2);
    ctor.aload(0);
    ctor.aload(1);
    ctor.ldc_class(&pr.owner_internal, &mut cw);
    ctor.push_string(&pr.prop_name, &mut cw);
    ctor.push_string(&signature, &mut cw);
    ctor.push_int(0, &mut cw);
    let sup = cw.methodref(
        &c.superclass,
        "<init>",
        "(Ljava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
    );
    ctor.invokespecial(sup, 5, 0);
    ctor.ret_void();
    ctor.ensure_locals(2);
    ctor.link();
    cw.add_method(0x0000, "<init>", "(Ljava/lang/Object;)V", &ctor);

    // `get()Object`: ((Owner) this.receiver).getName(), boxed if primitive.
    let mut get = CodeBuilder::new(1);
    get.aload(0);
    let recv_f = cw.fieldref(&c.superclass, "receiver", "Ljava/lang/Object;");
    get.getfield(recv_f, 1);
    let owner_ref = cw.class_ref(&pr.owner_internal);
    get.checkcast(owner_ref);
    let gref = cw.methodref(&pr.owner_internal, &pr.getter_name, &getter_desc);
    get.invokevirtual(gref, 0, slot_words(prop_jvm) as i32);
    if prop_jvm.is_primitive() {
        box_prim_free(&mut cw, &mut get, prop_jvm);
    }
    get.areturn();
    get.ensure_locals(1);
    get.link();
    cw.add_method(0x0001, "get", "()Ljava/lang/Object;", &get);
    cw.finish()
}

/// The wrapper class internal name for a primitive (`Int` → `java/lang/Integer`), for casting an
/// erased `Object` argument before unboxing.
fn wrapper_internal(t: Ty) -> &'static str {
    match t {
        Ty::Int => "java/lang/Integer",
        Ty::Long => "java/lang/Long",
        Ty::Double => "java/lang/Double",
        Ty::Float => "java/lang/Float",
        Ty::Boolean => "java/lang/Boolean",
        Ty::Char => "java/lang/Character",
        Ty::Byte => "java/lang/Byte",
        Ty::Short => "java/lang/Short",
        _ => "java/lang/Object",
    }
}

/// Emit a synthesized function-reference subclass (`<Owner>$ref$N extends FunctionReferenceImpl
/// implements Function<arity>`): an UNBOUND ref gets a `public static final INSTANCE` + a no-arg ctor
/// `super(arity, owner.class, name, sig, flags)`; a BOUND ref gets a `(Object)` ctor delegating to
/// `super(arity, receiver, owner.class, name, sig, flags)` (the base stores the receiver). The single
/// erased `invoke(Object…)Object` casts/unboxes its args and dispatches to the target, boxing the
/// result (or returning the `Unit` singleton for a `void` target). Reference EQUALITY (`::f == ::f`,
/// `a::m != b::m`) is inherited from `FunctionReferenceImpl` (compares owner/name/signature/receiver).
fn emit_func_ref_class(c: &crate::ir::IrClass, facade: &str) -> Vec<u8> {
    use crate::ir::FrDispatch;
    let fr = c.func_ref.as_ref().unwrap();
    // An empty `owner_class`/`call_owner` is the facade sentinel (a top-level function lives on the
    // file facade, whose name isn't known until emit) — resolve it here.
    let owner_class: &str = if fr.owner_class.is_empty() {
        facade
    } else {
        &fr.owner_class
    };
    let call_owner: &str = if fr.call_owner.is_empty() {
        facade
    } else {
        &fr.call_owner
    };
    let fq = c.fq_name.clone();
    let self_desc = format!("L{fq};");
    let mut cw = ClassWriter::new(&fq, &c.superclass);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER
    cw.add_interface(&format!("kotlin/jvm/functions/Function{}", fr.arity));

    // The call argument param types begin AFTER the receiver for an unbound member ref.
    let first_arg = match fr.dispatch {
        FrDispatch::VirtualUnbound => 1usize,
        _ => 0,
    };
    let ret_jvm = ir_ty_to_jvm(&fr.ret_ty);
    let returns_void = matches!(fr.ret_ty, IrType::Unit | IrType::Nothing);
    // JVM method descriptor of the target call (`(argDescs)retDesc`) and the kotlin reference signature
    // (`name(argDescs)retDesc`) — both over the CALL arguments (the receiver, if any, is excluded).
    let mut call_desc = String::from("(");
    for pt in fr.param_tys.iter().skip(first_arg) {
        call_desc.push_str(&ir_ty_to_jvm(pt).descriptor());
    }
    call_desc.push(')');
    let ret_desc = if returns_void {
        "V".to_string()
    } else {
        ret_jvm.descriptor()
    };
    call_desc.push_str(&ret_desc);
    let signature = format!("{}{}", fr.fn_name, call_desc);

    if fr.bound {
        // `<init>(Object)V`: super(arity, receiver, owner.class, name, sig, flags).
        let mut ctor = CodeBuilder::new(2);
        ctor.aload(0);
        ctor.push_int(fr.arity as i32, &mut cw);
        ctor.aload(1);
        ctor.ldc_class(owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let sup = cw.methodref(
            &c.superclass,
            "<init>",
            "(ILjava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
        );
        ctor.invokespecial(sup, 6, 0);
        ctor.ret_void();
        ctor.ensure_locals(2);
        ctor.link();
        cw.add_method(0x0000, "<init>", "(Ljava/lang/Object;)V", &ctor);
    } else {
        cw.add_field(0x0019, "INSTANCE", &self_desc); // PUBLIC|STATIC|FINAL
                                                      // `<init>()V`: super(arity, owner.class, name, sig, flags).
        let mut ctor = CodeBuilder::new(1);
        ctor.aload(0);
        ctor.push_int(fr.arity as i32, &mut cw);
        ctor.ldc_class(owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let sup = cw.methodref(
            &c.superclass,
            "<init>",
            "(ILjava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
        );
        ctor.invokespecial(sup, 5, 0);
        ctor.ret_void();
        ctor.ensure_locals(1);
        ctor.link();
        cw.add_method(0x0000, "<init>", "()V", &ctor);
        // `<clinit>`: INSTANCE = new <self>().
        let mut clinit = CodeBuilder::new(0);
        let cls = cw.class_ref(&fq);
        clinit.new_obj(cls);
        clinit.dup();
        let init = cw.methodref(&fq, "<init>", "()V");
        clinit.invokespecial(init, 0, 0);
        let fref = cw.fieldref(&fq, "INSTANCE", &self_desc);
        clinit.putstatic(fref, 1);
        clinit.ret_void();
        clinit.ensure_locals(0);
        clinit.link();
        cw.add_method(0x0008, "<clinit>", "()V", &clinit);
    }

    // The erased `invoke(Object×arity)Object`.
    let arity = fr.arity as u16;
    let mut invoke_desc = String::from("(");
    for _ in 0..arity {
        invoke_desc.push_str("Ljava/lang/Object;");
    }
    invoke_desc.push_str(")Ljava/lang/Object;");
    let mut inv = CodeBuilder::new(1 + arity);
    // Push the receiver for a member dispatch (`first_arg`, computed above, skips it in the arg loop).
    match fr.dispatch {
        FrDispatch::VirtualBound => {
            inv.aload(0);
            let recv_f = cw.fieldref(&c.superclass, "receiver", "Ljava/lang/Object;");
            inv.getfield(recv_f, 1);
            let owner_ref = cw.class_ref(call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::VirtualUnbound => {
            inv.aload(1);
            let owner_ref = cw.class_ref(call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::Static => {}
    };
    // Push the call arguments (cast/unbox each erased `Object`).
    let mut call_arg_words = 0i32;
    for (k, pt) in fr.param_tys.iter().enumerate().skip(first_arg) {
        inv.aload(1 + k as u16);
        let jt = ir_ty_to_jvm(pt);
        if jt.is_primitive() {
            let wref = cw.class_ref(wrapper_internal(jt));
            inv.checkcast(wref);
            unbox_prim(&mut cw, &mut inv, jt);
        } else if let Some(internal) = checkcast_internal(jt) {
            let cref = cw.class_ref(&internal);
            inv.checkcast(cref);
        }
        call_arg_words += slot_words(jt) as i32;
    }
    // Dispatch to the target.
    let ret_words = if returns_void {
        0
    } else {
        slot_words(ret_jvm) as i32
    };
    match fr.dispatch {
        FrDispatch::Static => {
            let m = cw.methodref(call_owner, &fr.call_name, &call_desc);
            inv.invokestatic(m, call_arg_words, ret_words);
        }
        _ if fr.call_interface => {
            let m = cw.interface_methodref(call_owner, &fr.call_name, &call_desc);
            inv.invokeinterface(m, call_arg_words, ret_words);
        }
        _ => {
            let m = cw.methodref(call_owner, &fr.call_name, &call_desc);
            inv.invokevirtual(m, call_arg_words, ret_words);
        }
    }
    // Adapt the result: a `void` target yields the `Unit` singleton; a primitive is boxed.
    if returns_void {
        let unit = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        inv.getstatic(unit, 1);
    } else if ret_jvm.is_primitive() {
        box_prim_free(&mut cw, &mut inv, ret_jvm);
    }
    inv.areturn();
    inv.ensure_locals(1 + arity);
    inv.link();
    cw.add_method(0x0001, "invoke", &invoke_desc, &inv);
    cw.finish()
}

/// The `kotlin/jvm/internal/Ref$XxxRef` holder class and its `element` field descriptor for a boxed
/// mutable local of element type `elem` (a primitive picks its specialized `Ref`, any reference uses
/// `Ref$ObjectRef` whose `element` is `Object`).
fn ref_class(elem: &IrType) -> (&'static str, &'static str) {
    match ir_ty_to_jvm(elem) {
        Ty::Int => ("kotlin/jvm/internal/Ref$IntRef", "I"),
        Ty::Long => ("kotlin/jvm/internal/Ref$LongRef", "J"),
        Ty::Float => ("kotlin/jvm/internal/Ref$FloatRef", "F"),
        Ty::Double => ("kotlin/jvm/internal/Ref$DoubleRef", "D"),
        Ty::Boolean => ("kotlin/jvm/internal/Ref$BooleanRef", "Z"),
        Ty::Char => ("kotlin/jvm/internal/Ref$CharRef", "C"),
        Ty::Byte => ("kotlin/jvm/internal/Ref$ByteRef", "B"),
        Ty::Short => ("kotlin/jvm/internal/Ref$ShortRef", "S"),
        _ => ("kotlin/jvm/internal/Ref$ObjectRef", "Ljava/lang/Object;"),
    }
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
        for (k, (et, ct)) in ep.iter().zip(&cp).enumerate() {
            load(*et, slot, &mut code);
            slot += slot_words(*et);
            // A boxed value-class param (a generic supertype method `f(Object,…)` delegating to a mangled
            // concrete override taking the underlying): checkcast the incoming `Object` to the boxed `X`,
            // then `unbox-impl` it to the underlying `ct` the target expects.
            if let Some(Some(vc)) = b.unbox_params.get(k) {
                let ci = cw.class_ref(vc);
                code.checkcast(ci);
                let m = cw.methodref(vc, "unbox-impl", &format!("(){}", ct.descriptor()));
                code.invokevirtual(m, 0, slot_words(*ct) as i32);
            } else if et != ct {
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
        // A value-class boxing bridge calls the mangled override (`target_name`) which returns the
        // erased underlying, then boxes the result back to `X` with `X.box-impl`.
        let target = b.target_name.as_deref().unwrap_or(&b.name);
        let m = cw.methodref(&c.fq_name, target, &method_descriptor(&cp, cr));
        code.invokevirtual(m, argw, slot_words(cr) as i32);
        if let Some(owner) = &b.box_ret {
            let bi = cw.methodref(
                owner,
                "box-impl",
                &format!("({}){}", cr.descriptor(), Ty::obj(owner).descriptor()),
            );
            code.invokestatic(bi, slot_words(cr) as i32, 1);
        } else if cr != er {
            if er.is_reference() && cr.is_primitive() {
                box_prim_free(cw, &mut code, cr);
            } else if er.is_primitive() && cr.is_primitive() {
                emit_num_conv(cr, er, &mut code);
            } else if cr == Ty::Unit && er.is_reference() {
                // A `Unit`-returning override bridged to a reference-returning supertype method
                // (`B.foo(): Unit` over `A.foo(): Any`): the JVM call is void, so materialize the
                // `kotlin/Unit` singleton the erased bridge must return.
                let f = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                code.getstatic(f, 1);
            } else if er.is_reference()
                && !matches!(er, Ty::Array(_))
                && ref_internal(cr) == "java/lang/Object"
            {
                // Covariant generic DIAMOND: the inherited concrete getter returns the erased
                // `Object` (`val x: T` in a generic base), but an interface in the hierarchy requires
                // a NARROWER reference type (`override val x: String`). This bridge's declared return
                // (`er`) is that narrower type, so the `Object` on the stack must be `checkcast` to it
                // before `areturn` — otherwise the verifier rejects it ("Bad return type"). The usual
                // direction (concrete is a SUBtype of erased) needs no cast; this is the inverse.
                // Restricted to a plain object type (`Ty::Obj`): an array `er` would need a descriptor-
                // form class ref, and that narrowing direction doesn't arise here.
                let ci = cw.class_ref(&ref_internal(er));
                code.checkcast(ci);
            } // reference→reference (concrete is a subtype of erased): no cast needed
        }
        emit_return(er, &mut code);
        code.ensure_locals(1 + pw);
        code.link();
        cw.add_method(
            0x0001 | 0x0040 | 0x1000,
            &b.name,
            &method_descriptor(&ep, er),
            &code,
        );
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

/// Emit an `interface`: `ACC_PUBLIC|ACC_INTERFACE|ACC_ABSTRACT`, extends `java/lang/Object`. A method
/// with no body is a `public abstract` declaration; a method WITH a body is a Kotlin default method —
/// emitted as a concrete instance method (Code, no `ACC_ABSTRACT`), which the JVM treats as a default
/// method.
fn emit_interface_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    bodies: &dyn MethodBodies,
    class_meta: Option<&KotlinMetadata>,
) -> Vec<u8> {
    let mut cw = ClassWriter::new(&c.fq_name, "java/lang/Object");
    cw.set_access(0x0001 | 0x0200 | 0x0400); // PUBLIC | INTERFACE | ABSTRACT
    for itf in &c.interfaces {
        cw.add_interface(itf);
    }
    for &fid in &c.methods {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            // A default method — concrete instance method on the interface.
            emit_method(ir, fid, &c.fq_name, facade, &mut cw, !f.is_static, bodies);
        } else {
            let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
            let ret = ir_ty_to_jvm(&f.ret);
            cw.add_abstract_method(
                0x0001 | 0x0400,
                &f.name,
                &method_descriptor(&param_tys, ret),
            );
            // PUBLIC | ABSTRACT
        }
    }
    if let Some(m) = class_meta {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
}

/// Emit an `enum class`: extends `java/lang/Enum`, a private `(String name, int ordinal, …)` ctor →
/// `super(name, ordinal)`, a `public static final` field per entry plus a `$VALUES` array, a
/// `<clinit>` that constructs the entries and fills `$VALUES`, and synthetic `values()`/`valueOf`.
fn emit_enum_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    bodies: &dyn MethodBodies,
) -> Vec<u8> {
    const ACC_ENUM: u16 = 0x4000;
    const ACC_SYNTHETIC: u16 = 0x1000;
    let fq = c.fq_name.clone();
    let self_desc = format!("L{fq};");
    let arr_desc = format!("[{self_desc}");
    let mut cw = ClassWriter::new(&fq, "java/lang/Enum");
    // An enum with an abstract member is `ACC_ABSTRACT`; one with any bodied entry (so a subclass
    // extends it) must not be `final`. A plain enum stays `final`.
    let has_abstract = c
        .methods
        .iter()
        .any(|&fid| ir.functions[fid as usize].body.is_none());
    let has_subclass = c.enum_entry_subclass.iter().any(|s| s.is_some());
    let mut access = 0x0001 | 0x0020 | ACC_ENUM; // PUBLIC | SUPER | ENUM
    if has_abstract {
        access |= 0x0400;
    } // ABSTRACT
    if !has_abstract && !has_subclass {
        access |= 0x0010;
    } // FINAL
    cw.set_access(access);

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
    cw.add_field(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        "$VALUES",
        &arr_desc,
    );

    // Private constructor: `(Ljava/lang/String;I<user>)V` → `super(name, ordinal)` + store user fields.
    let ctor_params: Vec<Ty> = std::iter::once(Ty::String)
        .chain(std::iter::once(Ty::Int))
        .chain(user_tys.iter().copied())
        .collect();
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
    // A subclassed enum's constructor must be reachable from its entry subclasses' `<init>` (an
    // `invokespecial` from another class) — package-private, not private.
    let base_ctor_acc = if has_subclass {
        ACC_SYNTHETIC
    } else {
        0x0002 | ACC_SYNTHETIC
    };
    cw.add_method(base_ctor_acc, "<init>", &ctor_desc, &ctor);

    // <clinit>: construct each entry, then build `$VALUES`.
    let ctor_argw: i32 = ctor_params.iter().map(|t| slot_words(*t) as i32).sum();
    {
        let mut e = Emitter {
            ir,
            cw: &mut cw,
            bodies,
            owner: fq.clone(),
            facade: facade.to_string(),
            slots: HashMap::new(),
            var_types: collect_var_types(ir),
            next_slot: 0,
            ret: Ty::Unit,
            loop_stack: Vec::new(),
        };
        let mut clinit = CodeBuilder::new(0);
        for (i, (entry, args)) in c.enum_entries.iter().enumerate() {
            // A branchy entry arg (`X(1 == 1)`) must run on a clean stack — spill all args to temps
            // first, then construct (mirrors the `New` node's spill).
            let spill = args.iter().any(|&a| e.records_frame(a));
            let temps = if spill {
                e.spill_to_temps(args, &mut clinit)
            } else {
                Vec::new()
            };
            // A bodied entry is an instance of its synthesized subclass (`new Enum$ENTRY(...)`); the
            // subclass constructor shares the enum's `(String,int,<user>)V` descriptor.
            let new_class = c
                .enum_entry_subclass
                .get(i)
                .and_then(|s| s.clone())
                .unwrap_or_else(|| fq.clone());
            let cls = e.cw.class_ref(&new_class);
            clinit.new_obj(cls);
            clinit.dup();
            clinit.push_string(entry, e.cw);
            clinit.push_int(i as i32, e.cw);
            if spill {
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut clinit);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                for &a in args {
                    e.emit_value(a, &mut clinit);
                }
            }
            let ctor_ref = e.cw.methodref(&new_class, "<init>", &ctor_desc);
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
    let veo = cw.methodref(
        "java/lang/Enum",
        "valueOf",
        "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
    );
    vof.invokestatic(veo, 2, 1);
    let cc = cw.class_ref(&fq);
    vof.checkcast(cc);
    vof.areturn();
    vof.ensure_locals(1);
    vof.link();
    cw.add_method(
        0x0009,
        "valueOf",
        &format!("(Ljava/lang/String;){self_desc}"),
        &vof,
    );

    for &fid in &c.methods {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            // Honor `is_static` (an extension-synthesized `static` member like serialization's
            // `serializer()` accessor) — emitting it as an instance method breaks an `E.serializer()`
            // static call (`IncompatibleClassChangeError`).
            emit_method(ir, fid, &fq, facade, &mut cw, !f.is_static, bodies);
        } else {
            // An abstract enum member (`abstract fun t(): String`) — declared `ACC_ABSTRACT`, the
            // entry subclasses override it.
            let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
            cw.add_abstract_method(
                0x0001 | 0x0400,
                &f.name,
                &method_descriptor(&param_tys, ir_ty_to_jvm(&f.ret)),
            );
        }
    }
    cw.finish()
}

/// Emit function `fid` as a method on `owner`. `instance` = an instance method (`this` in slot 0).
fn emit_method(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    bodies: &dyn MethodBodies,
) {
    // An inline-only lambda impl (its body has a non-local `return`) is never a real callable method —
    // it exists only to be spliced via its `inline_body`. Emitting it would produce an invalid, dead
    // method (an `areturn` of the enclosing fn's type from the lambda's signature). Skip it.
    if ir.inline_only_fns.contains(&fid) {
        return;
    }
    let f = &ir.functions[fid as usize];
    let body = f.body.unwrap();
    let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
    let ret = ir_ty_to_jvm(&f.ret);
    let mut e = Emitter {
        ir,
        cw,
        bodies,
        owner: owner.to_string(),
        facade: facade.to_string(),
        slots: HashMap::new(),
        var_types: collect_var_types(ir),
        next_slot: 0,
        ret,
        loop_stack: Vec::new(),
    };
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
                let m = e.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
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
        // kotlinc keeps an `Object`-override (a data class's toString/hashCode/equals) open even in a
        // final class, so honor `open_methods`; otherwise a method of a final class is itself final.
        let final_class = !ir.classes.iter().any(|o| o.superclass == owner);
        // An interface default method must NOT be `final` (the JVM rejects a final interface method).
        let owner_is_iface = ir
            .classes
            .iter()
            .any(|o| o.fq_name == owner && o.is_interface);
        let fin = final_class && !ir.open_methods.contains(&fid) && !owner_is_iface;
        // A `private set` setter is `private final` (kotlinc); else `public` (+`final` per above).
        let vis = if ir.private_methods.contains(&fid) {
            0x0002
        } else {
            0x0001
        };
        vis | if fin || ir.private_methods.contains(&fid) {
            0x0010
        } else {
            0
        }
    } else {
        // A `static` method is `public static final` (kotlinc) — EXCEPT on an interface, where a
        // `final` static method is illegal (`ClassFormatError`); emit `public static`.
        let owner_is_iface = ir
            .classes
            .iter()
            .any(|o| o.fq_name == owner && o.is_interface);
        if owner_is_iface {
            0x0009 // PUBLIC | STATIC
        } else {
            0x0019 // PUBLIC | STATIC | FINAL
        }
    };
    let signature = ir
        .signatures
        .get(&fid)
        .and_then(|g| jvm_method_signature(g, f));
    e.cw.add_method_sig(
        access,
        &f.name,
        &method_descriptor(&param_tys, ret),
        &code,
        signature.as_deref(),
    );
}

/// Format a function's backend-agnostic [`crate::ir::IrGenericSig`] into a JVM generic `Signature`
/// (`<T:Ljava/lang/Object;>(TT;)TT;`). `None` if a bound can't be represented yet. Concrete parameter/
/// return descriptors come from the (erased) `IrFunction`; bare type-parameter positions are `T<name>;`.
fn jvm_method_signature(g: &crate::ir::IrGenericSig, f: &crate::ir::IrFunction) -> Option<String> {
    let mut s = jvm_type_params(g)?;
    s.push('(');
    for (i, pt) in g.param_tparams.iter().enumerate() {
        match pt {
            Some(name) => s.push_str(&format!("T{name};")),
            None => s.push_str(&ir_ty_to_jvm(&f.params[i]).descriptor()),
        }
    }
    s.push(')');
    match &g.ret_tparam {
        Some(name) => s.push_str(&format!("T{name};")),
        None => s.push_str(&ir_ty_to_jvm(&f.ret).descriptor()),
    }
    Some(s)
}

/// Format a class's generic shape into a JVM class `Signature` (`<T:Ljava/lang/Object;>Ljava/lang/Object;`).
fn jvm_class_signature(g: &crate::ir::IrGenericSig) -> Option<String> {
    let mut s = jvm_type_params(g)?;
    s.push_str("Ljava/lang/Object;");
    Some(s)
}

/// The shared `<T:bound…>` type-parameter DECLARATION section, or `""` when there are no own type
/// parameters (e.g. a generic class's getter `getA()` → `()TA;` USES the class's `A` but declares none).
/// `None` if any bound can't be represented.
fn jvm_type_params(g: &crate::ir::IrGenericSig) -> Option<String> {
    if g.type_params.is_empty() {
        return Some(String::new());
    }
    let mut s = String::from("<");
    for (name, bound) in &g.type_params {
        s.push_str(name);
        s.push(':');
        s.push_str(&jvm_bound_descriptor(bound)?);
    }
    s.push('>');
    Some(s)
}

/// A type-parameter upper bound as a JVM signature element: `kotlin/Any` → `Ljava/lang/Object;`, a
/// primitive → its boxed wrapper (`kotlin/Int` → `Ljava/lang/Integer;`). `None` for anything else.
fn jvm_bound_descriptor(bound: &crate::ir::IrType) -> Option<String> {
    let ty = ir_ty_to_jvm(bound);
    if ty == Ty::obj("kotlin/Any") {
        return Some("Ljava/lang/Object;".to_string());
    }
    if ty.is_primitive() {
        return crate::resolve::nullable_prim_wrapper(ty).map(|w| format!("L{w};"));
    }
    None
}

/// Emit the JVM `<name>$default(self, params…, mask: int, marker: Object)` synthetic stub for an
/// instance method with default-valued parameters: for each defaulted param, `if ((mask & (1<<i)) != 0)
/// param = <default>;` then tail-call the real method. The default-value exprs reference `self` as value
/// 0. This is the JVM realization of default arguments — the `param_defaults` *meaning* is in the IR.
fn emit_default_stub(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    defaults: &[Option<u32>],
    bodies: &dyn MethodBodies,
) {
    let f = &ir.functions[fid as usize];
    let method_name = f.name.clone();
    let real_params: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
    let ret = ir_ty_to_jvm(&f.ret);
    let n = real_params.len();
    let owner_ty = Ty::obj(owner);

    let mut e = Emitter {
        ir,
        cw,
        bodies,
        owner: owner.to_string(),
        facade: facade.to_string(),
        slots: HashMap::new(),
        var_types: collect_var_types(ir),
        next_slot: 0,
        ret,
        loop_stack: Vec::new(),
    };
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
    e.slots
        .insert(9_000_002, (slot, Ty::obj("java/lang/Object")));
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
    let m =
        e.cw.methodref(owner, &method_name, &method_descriptor(&real_params, ret));
    code.invokevirtual(m, aw, slot_words(ret) as i32);
    emit_return(ret, &mut code);
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = vec![owner_ty];
    stub_params.extend(real_params.iter().copied());
    stub_params.push(Ty::Int);
    stub_params.push(Ty::obj("java/lang/Object"));
    let desc = method_descriptor(&stub_params, ret);
    e.cw.add_method(
        0x1009, /* PUBLIC | STATIC | SYNTHETIC */
        &format!("{method_name}$default"),
        &desc,
        &code,
    );
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
    /// Every `Variable` index → its JVM type (file-wide); a `value_ty(GetValue)` fallback for a slot not
    /// yet registered in `slots` (queried before its declaration emits — e.g. an inline result temp).
    var_types: HashMap<u32, Ty>,
    next_slot: u16,
    ret: Ty,
    /// Stack of enclosing loops' `(continue_label, break_label)` — `break`/`continue` target the top.
    /// Stack of enclosing loops: `(continue_label, break_label, source_label)`. A labeled
    /// `break@l`/`continue@l` targets the entry whose `source_label == Some(l)`; an unlabeled one
    /// targets the innermost (top).
    loop_stack: Vec<(Label, Label, Option<String>)>,
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
    /// Emit a lambda's `inline_body` (its value-producing form) INLINE at a stdlib-inline-fn splice:
    /// bind its parameter value-indices `0..` to the given JVM slots (captures → caller slots, lambda
    /// params → the on-stack args), then emit the body as a value — leaving the result on the stack. A
    /// user `return` inside the body emits a real `*return` from the enclosing method, i.e. a correct
    /// non-local return (no synthetic-return rewriting needed).
    fn emit_fn_body_inline(
        &mut self,
        inline_body: u32,
        param_slots: &[(u16, Ty)],
        code: &mut CodeBuilder,
    ) {
        let saved_slots = std::mem::take(&mut self.slots);
        for (i, &(slot, ty)) in param_slots.iter().enumerate() {
            self.slots.insert(i as u32, (slot, ty));
        }
        self.emit_value(inline_body, code);
        self.slots = saved_slots;
    }

    /// THE unified host+lambda splice (the merge of the branchy and lambda paths): splice a possibly
    /// BRANCHY host `inline fun` body, replacing each zero-arg lambda-parameter `Function0.invoke` site
    /// with that lambda's body. Handles `require(cond) { msg }` / `check(cond) { msg }` and the like —
    /// where the lambda runs only on a branch. v1: zero-arg (Function0) lambdas with branchless bodies,
    /// at an empty operand-stack baseline. Returns `false` (caller falls back / skips) on any other shape.
    fn try_inline_unified(
        &mut self,
        descriptor: &str,
        args: &[u32],
        body: &crate::jvm::classreader::MethodCode,
        base: u16,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some(params) = parse_descriptor_params(descriptor) else {
            return false;
        };
        if params.len() != args.len() {
            return false;
        }
        let top_local = base + body.max_locals;
        self.next_slot = self.next_slot.max(top_local);
        // Build each lambda argument's pre-relocated body (leaving its boxed result on the stack), and
        // its own (branchy-predicate) frames — resolved to byte offsets within the body, relocated below.
        let mut lam_splices: Vec<crate::jvm::inline::LambdaSplice> = Vec::new();
        let mut lam_frames: Vec<ResolvedFrames> = Vec::new();
        // The deepest operand stack any spliced lambda body reaches — the host's `max_stack` must cover it,
        // since the body is inlined into the host (a deep lambda body, e.g. `123 != intArrayOf() as Any`,
        // would otherwise overflow the host's stack). Propagated to `splice_inline` below.
        let mut lam_max_stack = 0u16;
        for (i, &a) in args.iter().enumerate() {
            let IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                inline_body,
                ..
            } = self.ir.expr(a).clone()
            else {
                continue;
            };
            let Some(inline_body) = inline_body else {
                return false;
            };
            let arity = arity as usize;
            let impl_f = &self.ir.functions[impl_fn as usize];
            // The impl method's parameters are `[captures…, lambda_params…]`.
            let Some(n_cap) = impl_f.params.len().checked_sub(arity) else {
                return false;
            };
            if n_cap != captures.len() {
                return false;
            }
            let cap_tys: Vec<Ty> = impl_f.params[..n_cap].iter().map(ir_ty_to_jvm).collect();
            let lam_tys: Vec<Ty> = impl_f.params[n_cap..].iter().map(ir_ty_to_jvm).collect();
            let impl_ret = ir_ty_to_jvm(&impl_f.ret);
            // Each capture binds to the caller's actual slot (a mutable capture writes through).
            let mut cap_slots: Vec<(u16, Ty)> = Vec::with_capacity(captures.len());
            for (k, &cap) in captures.iter().enumerate() {
                let IrExpr::GetValue(v) = self.ir.expr(cap) else {
                    return false;
                };
                let Some(&(slot, _)) = self.slots.get(v) else {
                    return false;
                };
                cap_slots.push((slot, cap_tys[k]));
            }
            // Build the lambda body into a scratch builder. The host left the lambda's `arity` arguments
            // on the stack (as `Object`, the erased `FunctionN.invoke` parameters); unbox a primitive
            // parameter, or `checkcast` a specific reference parameter to its type (the erased `Object`
            // arg — e.g. `iterator.next()` in `map` — must be narrowed to `String` before `it.uppercase()`),
            // then store it (top = last). Then run the body, then box the result to `Object` (matching the
            // replaced `invoke`'s `Object` result).
            let mut scratch = CodeBuilder::new(self.next_slot);
            scratch.set_stack(arity as u16);
            let mut param_slots: Vec<(u16, Ty)> = cap_slots;
            param_slots.extend(std::iter::repeat_n((0u16, Ty::Error), arity));
            for j in (0..arity).rev() {
                let jt = lam_tys[j];
                if jt.is_primitive() {
                    unbox_prim(self.cw, &mut scratch, jt);
                } else if let Some(internal) = checkcast_internal(jt) {
                    // The erased `Object` arg (`iterator.next()` in `map`, a `Function0.invoke` result)
                    // narrows to the parameter's type before use (`it.uppercase()` needs `String`).
                    let ci = self.cw.class_ref(&internal);
                    scratch.checkcast(ci);
                }
                let slot = self.next_slot;
                self.next_slot += slot_words(jt);
                store(jt, slot, &mut scratch);
                param_slots[n_cap + j] = (slot, jt);
            }
            self.emit_fn_body_inline(inline_body, &param_slots, &mut scratch);
            if impl_ret.is_primitive() {
                box_prim_free(self.cw, &mut scratch, impl_ret);
            }
            scratch.link(); // patch the lambda body's own branch operands before reading its bytes
            let lam_fr = scratch.resolved_frames(); // branchy predicate body → its own frames
            let Some(lam_insns) = crate::jvm::inline::disassemble(&scratch.bytes) else {
                return false;
            };
            if code.max_locals < scratch.max_locals {
                code.max_locals = scratch.max_locals;
            }
            self.next_slot = self.next_slot.max(scratch.max_locals);
            lam_max_stack = lam_max_stack.max(scratch.max_stack);
            lam_frames.push(lam_fr);
            lam_splices.push(crate::jvm::inline::LambdaSplice {
                param_index: i,
                body: lam_insns,
            });
        }
        if lam_splices.is_empty() {
            return false; // no lambda argument — not this path
        }
        // Probe at offset 0 to learn whether frames are needed (HOST branchy OR any lambda BODY branchy).
        let Some(probe) =
            crate::jvm::inline::splice_unified(body, descriptor, base, &lam_splices, 0, self.cw)
        else {
            return false;
        };
        // The splice records frames if it has a join, any lambda body has frames, OR the HOST body itself
        // records frames (a loop HOF's loop frames). All of these are bound relative to an empty operand
        // baseline (no caller operand prefix is threaded into them), so a non-empty baseline must bail —
        // `records_frame` makes a parent operand sequence spill earlier operands so we reach here at 0.
        let needs_frames = probe.join_required
            || !probe.frames.is_empty()
            || lam_frames.iter().any(|f| !f.is_empty());
        if needs_frames && code.stack_height() != 0 {
            return false; // frames carry no stack prefix → need an empty baseline
        }
        let ret_words = if descriptor.ends_with(")V") {
            0
        } else {
            slot_words(ty_from_descriptor_ret(descriptor)) as i32
        };
        // Emit each NON-lambda argument (the operands the host prologue stores into its parameter slots).
        let mut arg_words = 0i32;
        for (i, &a) in args.iter().enumerate() {
            if matches!(self.ir.expr(a), IrExpr::Lambda { .. }) {
                continue;
            }
            self.emit_value(a, code);
            let at = self.value_ty(a);
            if params[i].is_reference() && at.is_primitive() {
                box_prim_free(self.cw, code, at);
            }
            arg_words += slot_words(params[i]) as i32;
        }
        if !needs_frames {
            // Pure branchless host + lambda: append the bytes, no frames; works at any stack height.
            // The host's stack must cover the host body PLUS the deepest spliced lambda body (a safe upper
            // bound on the real peak) — else a deep lambda body overflows the host's operand stack.
            code.splice_inline(
                &probe.bytes,
                body.max_stack + lam_max_stack,
                top_local,
                arg_words,
                ret_words,
            );
            return true;
        }
        // RE-splice at the real method offset (so any switch in the host/lambda body pads correctly), then
        // bind the relocated HOST frames, the LAMBDA bodies' own frames, the spliced bytes, and the join.
        let splice_start = code.bytes.len();
        let Some(bs) = crate::jvm::inline::splice_unified(
            body,
            descriptor,
            base,
            &lam_splices,
            splice_start,
            self.cw,
        ) else {
            return false;
        };
        let prefix = self.verif_locals_upto(base);
        for (abs_off, body_locals, stack) in &bs.frames {
            let mut locals = prefix.clone();
            locals.extend(body_locals.iter().map(vtype_to_verif));
            let st: Vec<VerifType> = stack.iter().map(vtype_to_verif).collect();
            let l = code.new_label();
            code.bind_at(l, *abs_off);
            code.add_frame_if_new(l, locals, st);
        }
        for (k, frames) in lam_frames.iter().enumerate() {
            let host_ctx = bs.lambda_host_locals.get(k).cloned().unwrap_or_default();
            // The lambda body's frames were compiled against an EMPTY operand base; rebase each onto the
            // host operand-stack prefix sitting below the lambda value (e.g. a `map` destination). Empty
            // for `forEach`/`fold`/`takeIf`; `splice_unified` only returns `Some` here for a branchy body.
            let op_prefix: Vec<VerifType> = bs
                .lambda_stack_prefix
                .get(k)
                .and_then(|p| p.as_ref())
                .map(|p| p.iter().map(vtype_to_verif).collect())
                .unwrap_or_default();
            for (fb, locals, stack) in frames {
                let off = bs.lambda_byte_starts[k] + fb;
                let merged = self.merge_lambda_frame_locals(base, top_local, &host_ctx, locals);
                let mut st = op_prefix.clone();
                st.extend(stack.iter().cloned());
                let l = code.new_label();
                code.bind_at(l, off);
                code.add_frame_if_new(l, merged, st);
            }
        }
        // Register the spliced body's relocated exception handlers (try/catch/finally from `use`/
        // `synchronized`/`runCatching`). The handler frames are already bound above (each handler is a
        // StackMapTable target in `bs.frames`); here we add the guarded-range entries to the caller's
        // exception table via labels bound at the absolute spliced offsets.
        for &(start, end, handler, catch_type) in &bs.handlers {
            let (ls, le, lh) = (code.new_label(), code.new_label(), code.new_label());
            code.bind_at(ls, start);
            code.bind_at(le, end);
            code.bind_at(lh, handler);
            code.add_exception(ls, le, lh, catch_type);
        }
        code.set_needs_stackmap();
        // Host stack must cover the host body PLUS the deepest spliced lambda body (safe upper bound).
        code.splice_inline(
            &bs.bytes,
            body.max_stack + lam_max_stack,
            top_local,
            arg_words,
            ret_words,
        );
        if bs.join_required {
            let join = code.new_label();
            code.bind(join);
            let join_stack: Vec<VerifType> = bs.join_stack.iter().map(vtype_to_verif).collect();
            code.add_frame_if_new(join, prefix, join_stack);
        }
        true
    }

    /// Full locals for a frame INSIDE a spliced lambda body: the caller's locals (`0..base`), then the
    /// HOST's live body locals at the invoke (`host_ctx`, slots `base..` — for a loop host the loop
    /// iterator/accumulator, not just params), then the lambda's own slots (`top_local..`) from its
    /// scratch frame. All three are slot-expanded, overlaid, and re-collapsed.
    fn merge_lambda_frame_locals(
        &mut self,
        base: u16,
        top_local: u16,
        host_ctx: &[crate::jvm::inline::VType],
        lam_locals: &[VerifType],
    ) -> Vec<VerifType> {
        let mut slots = self.verif_slots_upto(base); // 0..base caller locals (slot-indexed)
                                                     // The host's live locals at `base..` (slot-indexed), then pad to `top_local` with `Top`.
        let host_collapsed: Vec<VerifType> = host_ctx.iter().map(vtype_to_verif).collect();
        slots.extend(expand_collapsed_locals(&host_collapsed));
        slots.truncate(top_local as usize);
        while slots.len() < top_local as usize {
            slots.push(VerifType::Top);
        }
        // The lambda's own slots (`top_local..`): expand the scratch frame, take from `top_local`.
        for s in expand_collapsed_locals(lam_locals)
            .into_iter()
            .skip(top_local as usize)
        {
            slots.push(s);
        }
        collapse_locals(&slots)
    }

    /// Slot-indexed caller locals for `0..upto` (long/double take two slots; `Top` fills the gaps).
    fn verif_slots_upto(&mut self, upto: u16) -> Vec<VerifType> {
        let mut raw = vec![VerifType::Top; upto as usize];
        let entries: Vec<(u16, Ty)> = self.slots.values().copied().collect();
        for (slot, ty) in entries {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        raw
    }

    /// Attempt to splice a cross-module `inline fun`'s compiled body at the call site (the bytecode
    /// inliner; the callee body comes from [`MethodBodies::body`]). Returns `true` if spliced; `false`
    /// ⇒ the caller emits an ordinary `invokestatic`, so an un-spliceable inline call is never
    /// miscompiled. The splice itself (StackMapTable relocation for branchy bodies + lambda-argument
    /// splicing) lands in the next phase — until then this always falls back.
    fn try_inline_static(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
        args: &[u32],
        code: &mut CodeBuilder,
    ) -> bool {
        self.try_inline_static_as(owner, name, descriptor, descriptor, args, code)
    }

    /// Splice `owner.name` whose REAL (body-fetch) descriptor is `descriptor`, mapping the body's locals
    /// per `splice_desc`. For an ordinary static they are equal; for an INSTANCE inline method spliced
    /// through this path, `splice_desc` PREPENDS the receiver as the first parameter (`this` = local 0)
    /// and `args[0]` is that receiver — so the body's `aload_0`/`aload_1`/… map to receiver/params.
    fn try_inline_static_as(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
        splice_desc: &str,
        args: &[u32],
        code: &mut CodeBuilder,
    ) -> bool {
        let Some(body) = self.bodies.body(owner, name, descriptor) else {
            return false;
        };
        // Splice the body's locals above BOTH the slot allocator's next free slot and the code's
        // high-water mark, so the spliced temporaries can never collide with a caller local (live or
        // reserved-but-unstored).
        let base = self.next_slot.max(code.max_locals);
        // Route (b): a literal lambda argument → splice its body at the host's `FunctionN.invoke` site
        // (the unified host+lambda splice handles both the branchy `require(c){m}` and the branchless
        // `let`/`also`/… shapes).
        if args
            .iter()
            .any(|&a| matches!(self.ir.expr(a), IrExpr::Lambda { .. }))
        {
            return self.try_inline_unified(descriptor, args, &body, base, code);
        }
        // A function-typed parameter whose argument isn't a literal lambda (a passed `Function`) isn't
        // spliceable — fall back to a normal call.
        if descriptor.contains("Lkotlin/jvm/functions/Function") {
            return false;
        }
        // A genuinely `void` (`)V`) method leaves NOTHING on the stack; `ty_from_descriptor_ret` maps
        // `V` to `Unit` (a 1-word value), so guard it to 0 words — else the splice leaves the operand
        // stack one slot too high (a later statement then splices on a non-empty baseline and bails).
        let ret_words = if descriptor.ends_with(")V") {
            0
        } else {
            slot_words(ty_from_descriptor_ret(descriptor)) as i32
        };
        let top_local = base + body.max_locals;
        // ONE splicer for every no-lambda body (`splice_unified` subsumes the old branchless + branchy
        // paths). Probe at offset 0 to learn `join_required` (a branchless body has no switch, so its
        // layout is position-independent); a branchy body is then RE-spliced at its real method offset so
        // any `tableswitch`/`lookupswitch` pads correctly.
        let Some(probe) =
            crate::jvm::inline::splice_unified(&body, splice_desc, base, &[], 0, self.cw)
        else {
            return false;
        };
        let arg_words: i32 = args
            .iter()
            .map(|&a| slot_words(self.value_ty(a)) as i32)
            .sum();
        if !probe.join_required {
            // Branchless: append the bytes, no frames. A DIVERGING body (ends in `athrow`, e.g.
            // `error(msg)`) leaves NOTHING on the stack — its post-splice height is the baseline.
            self.emit_operands(args, code);
            let diverges = probe.bytes.last() == Some(&0xbf);
            let ret_words = if diverges { 0 } else { ret_words };
            code.splice_inline(
                &probe.bytes,
                body.max_stack,
                top_local,
                arg_words,
                ret_words,
            );
            return true;
        }
        // Branchy body: needs an empty operand-stack baseline (the relocated frames carry no stack
        // prefix); a sub-expression inline call (non-empty stack) falls back to a normal call.
        if code.stack_height() != 0 {
            return false;
        }
        self.emit_operands(args, code);
        let splice_start = code.bytes.len();
        let Some(bs) = crate::jvm::inline::splice_unified(
            &body,
            splice_desc,
            base,
            &[],
            splice_start,
            self.cw,
        ) else {
            return false;
        };
        let prefix = self.verif_locals_upto(base);
        for (abs_off, body_locals, stack) in &bs.frames {
            let mut locals = prefix.clone();
            locals.extend(body_locals.iter().map(vtype_to_verif));
            let st: Vec<VerifType> = stack.iter().map(vtype_to_verif).collect();
            let l = code.new_label();
            code.bind_at(l, *abs_off);
            code.add_frame_if_new(l, locals, st);
        }
        code.set_needs_stackmap();
        code.splice_inline(&bs.bytes, body.max_stack, top_local, arg_words, ret_words);
        // Join frame: the redirected returns land at the continuation right after the spliced body.
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
                    // See the value-context `Block` arm: a statement nets zero, so reset the tracked
                    // height afterward to undo an approximate branchy-splice drift.
                    let base = code.stack_height();
                    self.emit(s, code);
                    if self.diverges(s) {
                        dead = true;
                        break;
                    } // rest of the block is unreachable
                    code.set_stack(base.max(0) as u16);
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
                    // `return <diverging>` (`return throw e`, `return error(..)`): the value already
                    // transferred control (athrow / a `Nothing`-returning call), so the trailing return
                    // opcode is unreachable dead code the verifier rejects (no stack-map frame). Skip it.
                    if !self.diverges(v) {
                        emit_return(self.ret, code);
                    }
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
                // `i = i + k` / `i = k + i` / `i = i - k` on an `Int` local with a small constant `k`
                // compiles to `iinc slot, k` (kotlinc's form), not load/const/add/store.
                let delta: Option<i32> = if jt == Ty::Int {
                    if let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *self.ir.expr(value) {
                        let cint = |e: u32| match self.ir.expr(e) {
                            IrExpr::Const(IrConst::Int(k)) => Some(*k),
                            _ => None,
                        };
                        let isvar =
                            |e: u32| matches!(self.ir.expr(e), IrExpr::GetValue(v) if *v == var);
                        match op {
                            IrBinOp::Add if isvar(lhs) => cint(rhs),
                            IrBinOp::Add if isvar(rhs) => cint(lhs),
                            IrBinOp::Sub if isvar(lhs) => cint(rhs).map(|k| -k),
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                match delta {
                    Some(d) if (-128..=127).contains(&d) => code.iinc(slot, d as i8),
                    _ => {
                        self.emit_value(value, code);
                        store(jt, slot, code);
                    }
                }
            }
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } => {
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
                let is_const = s.is_const;
                let facade = self.facade.clone();
                self.emit_value(value, code);
                // Within the facade write the field directly; from another class go through `setX()`.
                if self.owner == facade || is_const {
                    let fref = self.cw.fieldref(&facade, &name, &jt.descriptor());
                    code.putstatic(fref, slot_words(jt) as i32);
                } else {
                    let m = self.cw.methodref(
                        &facade,
                        &prop_setter_name(&name),
                        &format!("({})V", jt.descriptor()),
                    );
                    code.invokestatic(m, slot_words(jt) as i32, 0);
                }
            }
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => {
                let start = code.new_label();
                let cont = code.new_label();
                let end = code.new_label();
                self.frame(start, vec![], code);
                code.bind(start);
                // A pre-test loop checks the condition before the body; a `do…while` skips this and
                // tests at the bottom (`cont`), so the body always runs once.
                if !post_test {
                    // Jump out of the loop when the condition is false (fused comparison branch).
                    self.emit_cond_branch(cond, end, false, code);
                }
                // `continue` targets `cont` (run the update / bottom test); `break` targets `end`.
                self.loop_stack.push((cont, end, label.clone()));
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
                    self.emit_cond_branch(cond, start, true, code);
                } else {
                    self.frame(start, vec![], code);
                    code.goto(start);
                }
                self.frame(end, vec![], code);
                code.bind(end);
            }
            IrExpr::Break { label } => {
                let (_, end) = self.loop_target(&label);
                code.goto(end);
            }
            IrExpr::Continue { label } => {
                let (cont, _) = self.loop_target(&label);
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
            IrExpr::Break { label } => {
                let (_, end) = self.loop_target(label);
                code.goto(end);
                return;
            }
            IrExpr::Continue { label } => {
                let (cont, _) = self.loop_target(label);
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
            IrExpr::ClassConst { internal } => {
                // Empty `internal` is a sentinel for "the enclosing facade/owner class" (the lowering
                // doesn't know the facade name — only the emitter does, via `self.facade`).
                let name = if internal.is_empty() {
                    self.facade.clone()
                } else {
                    internal.clone()
                };
                code.ldc_class(&name, self.cw);
            }
            IrExpr::GetValue(i) => {
                let (slot, jt) = self.slots[i];
                load(jt, slot, code);
            }
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => {
                let c = &self.ir.classes[*class as usize];
                let (name, fty) = c.fields[*index as usize].clone();
                let jt = ir_ty_to_jvm(&fty);
                let owner = c.fq_name.clone();
                let is_lateinit = c.lateinit_fields.contains(index);
                self.emit_value(*receiver, code);
                let fref = self.cw.fieldref(&owner, &name, &jt.descriptor());
                code.getfield(fref, slot_words(jt) as i32);
                // A `lateinit var` read throws `UninitializedPropertyAccessException` while the field is
                // still null (kotlinc inserts this at every access): `dup; ifnonnull L; ldc name;
                // invokestatic Intrinsics.throwUninitializedPropertyAccessException; L:`.
                if is_lateinit {
                    code.dup();
                    let lbl = code.new_label();
                    code.ifnonnull(lbl);
                    code.push_string(&name, self.cw);
                    let m = self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "throwUninitializedPropertyAccessException",
                        "(Ljava/lang/String;)V",
                    );
                    code.invokestatic(m, 1, 0);
                    // At the join the field value (non-null on the taken path) is on the stack.
                    let st = self.verif_stack(jt);
                    self.frame(lbl, st, code);
                    code.bind(lbl);
                }
            }
            IrExpr::GetStatic(i) => {
                let s = &self.ir.statics[*i as usize];
                let jt = ir_ty_to_jvm(&s.ty);
                let name = s.name.clone();
                let is_const = s.is_const;
                let facade = self.facade.clone();
                // Within the facade (or a `const val`, which is public) read the field directly; from
                // another class a plain top-level property is private, so go through `getX()` — kotlinc's
                // cross-file property-access compilation.
                if self.owner == facade || is_const {
                    let fref = self.cw.fieldref(&facade, &name, &jt.descriptor());
                    code.getstatic(fref, slot_words(jt) as i32);
                } else {
                    let m = self.cw.methodref(
                        &facade,
                        &prop_getter_name(&name),
                        &format!("(){}", jt.descriptor()),
                    );
                    code.invokestatic(m, 0, slot_words(jt) as i32);
                }
            }
            IrExpr::New {
                class,
                args,
                ctor_params,
            } => {
                let c = &self.ir.classes[*class as usize];
                let owner = c.fq_name.clone();
                // The constructor takes only the parameter fields (primary), or a secondary
                // constructor's explicit parameter types; body properties are set inside it.
                let field_tys: Vec<Ty> = match ctor_params {
                    Some(ps) => ps.iter().map(ir_ty_to_jvm).collect(),
                    None if !c.ctor_args.is_empty() => {
                        c.ctor_args.iter().map(|(t, _)| ir_ty_to_jvm(t)).collect()
                    }
                    None => c.fields[..c.ctor_param_count as usize]
                        .iter()
                        .map(|(_, t)| ir_ty_to_jvm(t))
                        .collect(),
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
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
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
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } => {
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
                            None => {
                                push_zero(param_tys[i], code, self.cw);
                                mask |= 1 << i;
                            }
                        }
                    }
                    code.push_int(mask, self.cw);
                    code.aconst_null();
                    let mut stub_params = vec![Ty::obj(&owner)];
                    stub_params.extend(param_tys.iter().copied());
                    stub_params.push(Ty::Int);
                    stub_params.push(Ty::obj("java/lang/Object"));
                    let aw: i32 = stub_params.iter().map(|t| slot_words(*t) as i32).sum();
                    let m = self.cw.methodref(
                        &owner,
                        &format!("{name}$default"),
                        &method_descriptor(&stub_params, ret),
                    );
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
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => match callee {
                Callee::Local(fid) => {
                    let f = &self.ir.functions[*fid as usize];
                    let param_tys: Vec<Ty> = f.params.iter().map(ir_ty_to_jvm).collect();
                    let ret = ir_ty_to_jvm(&f.ret);
                    let name = f.name.clone();
                    let args = args.clone();
                    self.emit_operands(&args, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self
                        .cw
                        .methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::External(fq) => self.emit_intrinsic(fq, dispatch_receiver, args, code),
                Callee::CrossFile {
                    facade,
                    name,
                    params,
                    ret,
                } => {
                    // A top-level function from another file → `invokestatic <facade>.<name>(desc)`.
                    let param_tys: Vec<Ty> = params.iter().map(ir_ty_to_jvm).collect();
                    let ret = ir_ty_to_jvm(ret);
                    let (facade, name) = (facade.clone(), name.clone());
                    let args = args.clone();
                    self.emit_operands(&args, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let desc = method_descriptor(&param_tys, ret);
                    // A static method declared on an INTERFACE (`@Serializable(with=X) interface I` whose
                    // synthetic `serializer()` is static) needs an InterfaceMethodref constant, even for
                    // `invokestatic` (else `IncompatibleClassChangeError`).
                    let m = if self
                        .ir
                        .classes
                        .iter()
                        .any(|c| c.fq_name == facade && c.is_interface)
                    {
                        self.cw.interface_methodref(&facade, &name, &desc)
                    } else {
                        self.cw.methodref(&facade, &name, &desc)
                    };
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Static {
                    owner,
                    name,
                    descriptor,
                    inline,
                    must_inline,
                } => {
                    let (owner, name, descriptor, inline, must_inline) = (
                        owner.clone(),
                        name.clone(),
                        descriptor.clone(),
                        *inline,
                        *must_inline,
                    );
                    let args = args.clone();
                    // An inline call to an INSTANCE method on a singleton receiver (a value-class COMPANION
                    // fn, `Result.success`): the receiver loads first, then the body is spliced with the
                    // receiver prepended as `this` (local 0). `splice_desc` adds the receiver as the first
                    // parameter; the body is fetched by its REAL (instance) descriptor.
                    if inline {
                        if let Some(&recv) = dispatch_receiver.as_ref() {
                            let recv_desc = self.value_ty(recv).descriptor();
                            let splice_desc = format!("({}{}", recv_desc, &descriptor[1..]);
                            let mut all = Vec::with_capacity(args.len() + 1);
                            all.push(recv);
                            all.extend(args.iter().copied());
                            if self.try_inline_static_as(
                                &owner,
                                &name,
                                &descriptor,
                                &splice_desc,
                                &all,
                                code,
                            ) {
                                return;
                            }
                        }
                    }
                    // A cross-module `inline fun`: try to splice its compiled body here (the bytecode
                    // inliner). On any unsupported shape `try_inline_static` returns false and we emit the
                    // ordinary `invokestatic` — so an un-spliceable inline call is never miscompiled.
                    if inline
                        && dispatch_receiver.is_none()
                        && self.try_inline_static(&owner, &name, &descriptor, &args, code)
                    {
                        return;
                    }
                    // A `must_inline` callee (non-public `@InlineOnly`) has no legal `invokestatic`: the
                    // splice failed (e.g. a branchy body on a non-empty operand stack), so skip the file.
                    if must_inline {
                        INLINE_BAIL.with(|b| b.set(true));
                        // Still emit a (discarded) call so the builder's stack height stays consistent.
                    }
                    self.emit_operands(&args, code);
                    let aw: i32 = args
                        .iter()
                        .map(|&a| slot_words(self.value_ty(a)) as i32)
                        .sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    let m = self.cw.methodref(&owner, &name, &descriptor);
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Virtual {
                    owner,
                    name,
                    descriptor,
                    interface,
                } => {
                    let (owner, name, descriptor, interface) =
                        (owner.clone(), name.clone(), descriptor.clone(), *interface);
                    let recv = dispatch_receiver.expect("virtual call needs a receiver");
                    let args = args.clone();
                    let mut ops = vec![recv];
                    ops.extend(args.iter().copied());
                    self.emit_operands(&ops, code);
                    let aw: i32 = args
                        .iter()
                        .map(|&a| slot_words(self.value_ty(a)) as i32)
                        .sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    if interface {
                        let m = self.cw.interface_methodref(&owner, &name, &descriptor);
                        code.invokeinterface(m, aw, slot_words(ret) as i32);
                    } else {
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        code.invokevirtual(m, aw, slot_words(ret) as i32);
                    }
                }
                Callee::CrossFileVirtual {
                    owner,
                    name,
                    params,
                    ret,
                    interface,
                } => {
                    let owner = owner.clone();
                    let name = name.clone();
                    let interface = *interface;
                    let param_tys: Vec<Ty> = params.iter().map(ir_ty_to_jvm).collect();
                    let ret = ir_ty_to_jvm(ret);
                    let descriptor = method_descriptor(&param_tys, ret);
                    let recv = dispatch_receiver.expect("cross-file virtual call needs a receiver");
                    let args = args.clone();
                    let mut ops = vec![recv];
                    ops.extend(args.iter().copied());
                    self.emit_operands(&ops, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    if interface {
                        let m = self.cw.interface_methodref(&owner, &name, &descriptor);
                        code.invokeinterface(m, aw, slot_words(ret) as i32);
                    } else {
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        code.invokevirtual(m, aw, slot_words(ret) as i32);
                    }
                }
                Callee::Special {
                    owner,
                    name,
                    descriptor,
                } => {
                    let (owner, name, descriptor) =
                        (owner.clone(), name.clone(), descriptor.clone());
                    let recv = dispatch_receiver.expect("special call needs a receiver");
                    let args = args.clone();
                    let mut ops = vec![recv];
                    ops.extend(args.iter().copied());
                    self.emit_operands(&ops, code);
                    let aw: i32 = args
                        .iter()
                        .map(|&a| slot_words(self.value_ty(a)) as i32)
                        .sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    let m = self.cw.methodref(&owner, &name, &descriptor);
                    code.invokespecial(m, aw, slot_words(ret) as i32);
                }
            },
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => {
                // A primitive target of `instanceof`/`checkcast` (`x is Int`) tests the boxed wrapper.
                let jvm_ty = ir_ty_to_jvm(type_operand);
                let internal = if jvm_ty.is_primitive() {
                    crate::jvm::jvm_class_map::wrapper_internal(jvm_ty)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| ref_internal(jvm_ty))
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
                        code.push_string(
                            &format!("null cannot be cast to non-null type {kotlin_name}"),
                            self.cw,
                        );
                        let m = self.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "checkNotNull",
                            "(Ljava/lang/Object;Ljava/lang/String;)V",
                        );
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
            IrExpr::StringConcat(parts) => {
                let parts = parts.clone();
                if parts.len() == 1 {
                    let p = parts[0];
                    if matches!(self.ir.expr(p), IrExpr::Const(IrConst::String(_))) {
                        // A lone string constant is already a `String`.
                        self.emit_value(p, code);
                    } else {
                        // A single interpolation `"$x"` → `String.valueOf(x)` (kotlinc's form).
                        let pty = self.value_ty(p);
                        self.emit_value(p, code);
                        let m = self
                            .cw
                            .methodref("java/lang/String", "valueOf", valueof_desc(pty));
                        code.invokestatic(m, slot_words(pty) as i32, 1);
                    }
                } else {
                    let sb = self.cw.class_ref("java/lang/StringBuilder");
                    let init = self
                        .cw
                        .methodref("java/lang/StringBuilder", "<init>", "()V");
                    // A branchy part (`"${when{…}}"`) records merge frames that would omit the
                    // StringBuilder on the stack — spill every part to a temp first, then build.
                    if parts.iter().any(|&p| self.records_frame(p)) {
                        let temps = self.spill_to_temps(&parts, code);
                        code.new_obj(sb);
                        code.dup();
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
                        code.invokespecial(init, 0, 0);
                        for &p in &parts {
                            self.append_part(p, code);
                        }
                    }
                    let ts = self.cw.methodref(
                        "java/lang/StringBuilder",
                        "toString",
                        "()Ljava/lang/String;",
                    );
                    code.invokevirtual(ts, 0, 1);
                }
            }
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
            IrExpr::ExternalStaticInstance { owner, ty, field } => {
                let f = self.cw.fieldref(owner, field, &format!("L{ty};"));
                code.getstatic(f, 1);
            }
            IrExpr::ExternalStaticField {
                owner,
                name,
                descriptor,
            } => {
                let f = self.cw.fieldref(owner, name, descriptor);
                let words = if descriptor == "J" || descriptor == "D" {
                    2
                } else {
                    1
                };
                code.getstatic(f, words);
            }
            IrExpr::EnumValues { class } => {
                let fq = self.ir.classes[*class as usize].fq_name.clone();
                let m = self.cw.methodref(&fq, "values", &format!("()[L{fq};"));
                code.invokestatic(m, 0, 1);
            }
            IrExpr::EnumValueOf { class, arg } => {
                let fq = self.ir.classes[*class as usize].fq_name.clone();
                self.emit_value(*arg, code);
                let m = self
                    .cw
                    .methodref(&fq, "valueOf", &format!("(Ljava/lang/String;)L{fq};"));
                code.invokestatic(m, 1, 1);
            }
            IrExpr::When { branches } => self.emit_when(branches, code),
            // Block in value position: run its statements for effect, leave the trailing value on the
            // stack. Scope block-locals (restore the slot map) so they don't leak into outer frames.
            IrExpr::Block { stmts, value } => {
                let saved = self.slots.clone();
                let mut dead = false;
                for s in stmts {
                    // A statement nets zero on the operand stack (its value is stored/discarded). Reset
                    // the tracked height to that baseline afterward: a branchy lambda splice (`takeIf`)
                    // tracks its internal branches only approximately and can leave `cur_stack` drifted
                    // above the real (verified-balanced) height, which would make a LATER branchy splice
                    // in the same block falsely see a non-empty baseline and bail.
                    let base = code.stack_height();
                    self.emit(*s, code);
                    if self.diverges(*s) {
                        dead = true;
                        break;
                    }
                    code.set_stack(base.max(0) as u16);
                }
                if !dead {
                    if let Some(v) = value {
                        self.emit_value(*v, code);
                    }
                }
                self.slots = saved;
            }
            IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                sam,
                ..
            } => {
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
                        let inst_params: Vec<String> =
                            lam_tys.iter().map(|t| boxed_descriptor(*t)).collect();
                        let inst_desc =
                            format!("({}){}", inst_params.concat(), boxed_descriptor(impl_ret));
                        (
                            iface,
                            "invoke".to_string(),
                            sam_descriptor(*arity),
                            inst_desc,
                        )
                    }
                };
                let facade = self.facade.clone();
                let meta = self.cw.method_handle_static(
                    "java/lang/invoke/LambdaMetafactory",
                    "metafactory",
                    LMF_METAFACTORY_DESC,
                );
                let sam_mt = self.cw.method_type(&sam_desc);
                let impl_mh = self
                    .cw
                    .method_handle_static(&facade, &impl_name, &impl_desc);
                let inst_mt = self.cw.method_type(&inst_desc);
                let bsm = self.cw.add_bootstrap(meta, vec![sam_mt, impl_mh, inst_mt]);
                // The `invokedynamic` takes the captured values and yields the interface instance.
                let cap_descs: String = cap_tys.iter().map(|t| t.descriptor()).collect();
                let indy =
                    self.cw
                        .invoke_dynamic(bsm, &sam_method, &format!("({cap_descs})L{iface};"));
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
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNull",
                    "(Ljava/lang/Object;)V",
                );
                code.invokestatic(m, 1, 0);
            }
            IrExpr::Throw { operand } => {
                self.emit_value(*operand, code);
                code.athrow();
            }
            // `return v` in value position (`x ?: return v`): emit the return; control transfers away, so
            // (like `throw`) nothing is left for the surrounding merge.
            IrExpr::Return(ret_val) => match ret_val {
                Some(rv) => {
                    self.emit_value(*rv, code);
                    if !self.diverges(*rv) {
                        emit_return(self.ret, code);
                    }
                }
                None => code.ret_void(),
            },
            IrExpr::Vararg {
                element_type,
                elements,
            } => {
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
            IrExpr::NewArray { element_type, size } => {
                let et = ir_ty_to_jvm(element_type);
                self.emit_value(*size, code);
                if et.is_primitive() {
                    code.newarray(prim_newarray_atype(et));
                } else {
                    let ci = self.cw.class_ref(&ref_internal(et));
                    code.anewarray(ci);
                }
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                result,
            } => {
                let catches = catches.clone();
                let result = result.clone();
                self.emit_try(*body, &catches, *finally, &result, code);
            }
            IrExpr::NewExternal {
                internal,
                ctor_desc,
                args,
            } => {
                let owner = internal.clone();
                let desc = ctor_desc.clone();
                let args = args.clone();
                // Arguments were coerced to the constructor's parameter types in lowering, so each
                // argument's `value_ty` is its parameter — the descriptor's argument-word count.
                let aw: i32 = args
                    .iter()
                    .map(|&a| slot_words(self.value_ty(a)) as i32)
                    .sum();
                if args.iter().any(|&a| self.records_frame(a)) {
                    // A branchy argument can't run with `[new, dup]` on the stack (its merge frame
                    // would omit them) — evaluate args into temps first, then build.
                    let temps = self.spill_to_temps(&args, code);
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
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
            IrExpr::NewCrossFile {
                internal,
                params,
                args,
            } => {
                let owner = internal.clone();
                let param_tys: Vec<Ty> = params.iter().map(ir_ty_to_jvm).collect();
                let desc = method_descriptor(&param_tys, Ty::Unit);
                let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let args = args.clone();
                if args.iter().any(|&a| self.records_frame(a)) {
                    let temps = self.spill_to_temps(&args, code);
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
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
            IrExpr::RefNew { elem, init } => {
                let (cls, fdesc) = ref_class(elem);
                let ew = slot_words(ir_ty_to_jvm(elem)) as i32;
                // A branchy initializer can't run with `[holder, holder]` on the stack — spill it.
                if self.records_frame(*init) {
                    let temps = self.spill_to_temps(&[*init], code);
                    let ci = self.cw.class_ref(cls);
                    code.new_obj(ci);
                    code.dup();
                    let m = self.cw.methodref(cls, "<init>", "()V");
                    code.invokespecial(m, 0, 0);
                    code.dup();
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                } else {
                    let ci = self.cw.class_ref(cls);
                    code.new_obj(ci);
                    code.dup();
                    let m = self.cw.methodref(cls, "<init>", "()V");
                    code.invokespecial(m, 0, 0);
                    code.dup();
                    self.emit_value(*init, code);
                }
                let f = self.cw.fieldref(cls, "element", fdesc);
                code.putfield(f, ew);
            }
            IrExpr::RefGet { holder, elem } => {
                self.emit_value(*holder, code);
                let (cls, fdesc) = ref_class(elem);
                let f = self.cw.fieldref(cls, "element", fdesc);
                let ejvm = ir_ty_to_jvm(elem);
                code.getfield(f, slot_words(ejvm) as i32);
                // An `ObjectRef.element` is typed `Object`; narrow to the boxed value's reference type.
                if ejvm.is_reference() && ref_internal(ejvm) != "java/lang/Object" {
                    let cc = self.cw.class_ref(&ref_internal(ejvm));
                    code.checkcast(cc);
                }
            }
            IrExpr::RefSet {
                holder,
                elem,
                value,
            } => {
                self.emit_value(*holder, code);
                self.emit_value(*value, code);
                let (cls, fdesc) = ref_class(elem);
                let f = self.cw.fieldref(cls, "element", fdesc);
                code.putfield(f, slot_words(ir_ty_to_jvm(elem)) as i32);
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
                let m = self
                    .cw
                    .interface_methodref(&iface, "invoke", &sam_descriptor(n as u8));
                code.invokeinterface(m, n as i32, 1);
                // The interface returns `Object`; cast/unbox to the function's declared return type.
                let rt = ir_ty_to_jvm(ret);
                match rt {
                    Ty::Int
                    | Ty::Long
                    | Ty::Double
                    | Ty::Float
                    | Ty::Boolean
                    | Ty::Char
                    | Ty::Byte
                    | Ty::Short => self.unbox_to(rt, code),
                    Ty::Unit | Ty::Nothing => code.pop(),
                    Ty::String => {
                        let ci = self.cw.class_ref("java/lang/String");
                        code.checkcast(ci);
                    }
                    Ty::Obj(internal, _) => {
                        let ci = self.cw.class_ref(internal);
                        code.checkcast(ci);
                    }
                    Ty::Array(_) => {
                        let ci = self.cw.class_ref(&rt.descriptor());
                        code.checkcast(ci);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn emit_intrinsic(
        &mut self,
        fq: &str,
        recv: &Option<u32>,
        args: &[u32],
        code: &mut CodeBuilder,
    ) {
        match fq {
            // Static numeric helpers used by synthesized data-class equals/hashCode.
            "java/lang/Double.hashCode"
            | "java/lang/Long.hashCode"
            | "java/lang/Float.hashCode"
            | "java/lang/Boolean.hashCode"
            | "java/lang/Integer.hashCode"
            | "java/lang/Short.hashCode"
            | "java/lang/Byte.hashCode"
            | "java/lang/Character.hashCode"
            | "java/util/Objects.hashCode" => {
                self.emit_value(args[0], code);
                let (cls, d) = match fq {
                    "java/lang/Double.hashCode" => ("java/lang/Double", "(D)I"),
                    "java/lang/Long.hashCode" => ("java/lang/Long", "(J)I"),
                    "java/lang/Float.hashCode" => ("java/lang/Float", "(F)I"),
                    "java/lang/Boolean.hashCode" => ("java/lang/Boolean", "(Z)I"),
                    "java/lang/Integer.hashCode" => ("java/lang/Integer", "(I)I"),
                    "java/lang/Short.hashCode" => ("java/lang/Short", "(S)I"),
                    "java/lang/Byte.hashCode" => ("java/lang/Byte", "(B)I"),
                    "java/lang/Character.hashCode" => ("java/lang/Character", "(C)I"),
                    _ => ("java/util/Objects", "(Ljava/lang/Object;)I"),
                };
                let aw = slot_words(self.value_ty(args[0])) as i32;
                let m = self.cw.methodref(cls, "hashCode", d);
                code.invokestatic(m, aw, 1);
            }
            "java/lang/Double.compare" | "java/lang/Float.compare" => {
                self.emit_value(args[0], code);
                self.emit_value(args[1], code);
                let (cls, d, aw) = if fq == "java/lang/Double.compare" {
                    ("java/lang/Double", "(DD)I", 4)
                } else {
                    ("java/lang/Float", "(FF)I", 2)
                };
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
                    let init = self
                        .cw
                        .methodref("java/lang/StringBuilder", "<init>", "()V");
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
                    let init = self
                        .cw
                        .methodref("java/lang/StringBuilder", "<init>", "()V");
                    code.invokespecial(init, 0, 0);
                    self.append(recv, code);
                    self.append(arg, code);
                }
                let ts = self.cw.methodref(
                    "java/lang/StringBuilder",
                    "toString",
                    "()Ljava/lang/String;",
                );
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
                let m = self
                    .cw
                    .methodref("java/lang/Enum", "name", "()Ljava/lang/String;");
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
                // A boxed primitive array (`Array<Int>` = `Integer[]`): `a[i]` is an unboxed `Int`, so
                // unbox the loaded wrapper. (`value_ty` for this call reports the same primitive.)
                if let Some(p) = boxed_prim_of(elem) {
                    self.unbox_to(p, code);
                }
            }
            "kotlin/Array.set" => {
                let arr = recv.unwrap();
                let elem = self.array_elem(arr);
                self.emit_value(arr, code);
                self.emit_value(args[0], code);
                self.emit_value(args[1], code);
                // Boxed primitive array: box the primitive value before the `aastore`.
                if let Some(p) = boxed_prim_of(elem) {
                    box_prim_free(self.cw, code, p);
                }
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
            "kotlin/Any.hashCode" => {
                let r = recv.unwrap();
                let ty = self.value_ty(r);
                self.emit_value(r, code);
                match ty {
                    // A primitive hashes via its wrapper's static `hashCode`.
                    Ty::Int | Ty::Short | Ty::Byte | Ty::Char => {}
                    Ty::Long => {
                        let m = self.cw.methodref("java/lang/Long", "hashCode", "(J)I");
                        code.invokestatic(m, 2, 1);
                    }
                    Ty::Boolean => {
                        let m = self.cw.methodref("java/lang/Boolean", "hashCode", "(Z)I");
                        code.invokestatic(m, 1, 1);
                    }
                    Ty::Double => {
                        let m = self.cw.methodref("java/lang/Double", "hashCode", "(D)I");
                        code.invokestatic(m, 2, 1);
                    }
                    Ty::Float => {
                        let m = self.cw.methodref("java/lang/Float", "hashCode", "(F)I");
                        code.invokestatic(m, 1, 1);
                    }
                    // A reference dispatches to its `hashCode` override.
                    _ => {
                        let owner = ref_internal(ty);
                        let m = self.cw.methodref(&owner, "hashCode", "()I");
                        code.invokevirtual(m, 0, 1);
                    }
                }
            }
            _ => {}
        }
    }

    fn append(&mut self, e: u32, code: &mut CodeBuilder) {
        let ty = self.value_ty(e);
        self.emit_value(e, code);
        self.append_top(ty, code);
    }

    /// Append one string-template part to the `StringBuilder` beneath it. A single-character string
    /// constant appends as a `char` (kotlinc emits `append(C)` with the char constant, not `append(String)`).
    fn append_part(&mut self, p: u32, code: &mut CodeBuilder) {
        let single_char = if let IrExpr::Const(IrConst::String(s)) = self.ir.expr(p) {
            if s.chars().count() == 1 {
                s.chars().next()
            } else {
                None
            }
        } else {
            None
        };
        if let Some(c) = single_char {
            code.push_int(c as i32, self.cw);
            self.append_top(Ty::Char, code);
        } else {
            self.append(p, code);
        }
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
            // The multi-part `StringConcat` itself spills branchy parts internally, so as a whole it
            // leaves only its `String` result — but a parent operand sequence still must treat it as
            // frame-recording if any part does (it builds the StringBuilder mid-stack otherwise).
            IrExpr::StringConcat(parts) => parts.iter().any(|&p| self.records_frame(p)),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
                (matches!(op, Lt | Le | Gt | Ge | Eq | Ne) && self.value_ty(*lhs).is_primitive())
                    // `===`/`!==` always emits a branch+merge frame — the `if_acmp*` path (references)
                    // and the value-compare path it remaps to for primitives both do.
                    || matches!(op, RefEq | RefNe)
                    // `x == null`/`x != null` emits an `ifnull`/`ifnonnull` branch+merge frame.
                    || (matches!(op, Eq | Ne)
                        && (matches!(self.ir.expr(*lhs), IrExpr::Const(IrConst::Null))
                            || matches!(self.ir.expr(*rhs), IrExpr::Const(IrConst::Null))))
                    || self.records_frame(*lhs) || self.records_frame(*rhs)
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => {
                // An inline call whose SPLICED body records StackMapTable frames — a branchy lambda body,
                // or a branchy host body (a loop HOF like `map`/`filter`, or an `@InlineOnly` `require`/
                // `check`) — records frames at THIS position. So a parent operand sequence must spill the
                // earlier operands to temps (keeping the splice at an empty baseline), exactly as for
                // `when`/`try`. Without this, an inline HOF used as a non-first operand
                // (`sb.append(xs.map { … }))`) would splice at a non-empty baseline and bail to a real call.
                let splice_records = match callee {
                    Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline,
                        must_inline,
                    } if *inline || *must_inline => {
                        args.iter().any(|&a| {
                            matches!(self.ir.expr(a),
                                IrExpr::Lambda { inline_body: Some(b), .. } if self.records_frame(*b))
                        }) || self
                            .bodies
                            .body(owner, name, descriptor)
                            .and_then(|b| crate::jvm::inline::disassemble(&b.code))
                            .is_some_and(|ins| {
                                ins.iter()
                                    .any(|i| !matches!(i, crate::jvm::inline::Insn::Plain { .. }))
                            })
                    }
                    _ => false,
                };
                splice_records
                    || dispatch_receiver.map_or(false, |r| self.records_frame(r))
                    || args.iter().any(|&a| self.records_frame(a))
            }
            IrExpr::MethodCall { receiver, args, .. } => {
                self.records_frame(*receiver)
                    || args
                        .iter()
                        .any(|a| a.map_or(false, |x| self.records_frame(x)))
            }
            IrExpr::New { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::GetField { receiver, .. } => self.records_frame(*receiver),
            IrExpr::SetField {
                receiver, value, ..
            } => self.records_frame(*receiver) || self.records_frame(*value),
            IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => {
                self.records_frame(*value)
            }
            IrExpr::TypeOp { arg, .. } | IrExpr::EnumValueOf { arg, .. } => {
                self.records_frame(*arg)
            }
            IrExpr::NotNullAssert { operand } => self.records_frame(*operand),
            IrExpr::NewExternal { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::NewCrossFile { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::RefGet { holder, .. } => self.records_frame(*holder),
            IrExpr::RefSet { holder, value, .. } => {
                self.records_frame(*holder) || self.records_frame(*value)
            }
            IrExpr::RefNew { init, .. } => self.records_frame(*init),
            IrExpr::Throw { operand } => self.records_frame(*operand),
            IrExpr::Vararg { elements, .. } => elements.iter().any(|&a| self.records_frame(a)),
            IrExpr::NewArray { size, .. } => self.records_frame(*size),
            IrExpr::Return(v) => v.map_or(false, |x| self.records_frame(x)),
            IrExpr::Variable { init, .. } => init.map_or(false, |i| self.records_frame(i)),
            IrExpr::Block { stmts, value } => {
                stmts.iter().any(|&s| self.records_frame(s))
                    || value.map_or(false, |v| self.records_frame(v))
            }
            _ => false, // Const, GetValue, GetStatic, EnumEntry, EnumValues — no frames
        }
    }

    /// Push `ops` onto the stack in order. If any op after the first records a frame (so an earlier
    /// op would be live on the stack across that frame), evaluate all ops into temps first, then load
    /// them — keeping the stack empty while each frame-recording op runs.
    fn emit_operands(&mut self, ops: &[u32], code: &mut CodeBuilder) {
        if ops.iter().skip(1).any(|&o| self.records_frame(o)) {
            let temps = self.spill_to_temps(ops, code);
            for &(slot, t, _) in &temps {
                load(t, slot, code);
            }
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
        } else {
            for &o in ops {
                self.emit_value(o, code);
            }
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
                // `emit_operands` spills the lhs to a temp when the rhs records a stackmap frame (a
                // branchy operand, `5 + if (c) 1 else 2`) — else it just emits both in order, so the
                // bytecode is unchanged for the common case. Without it the lhs is stranded on the stack
                // across the rhs's merge frame (`VerifyError: Inconsistent stackmap frames`).
                self.emit_operands(&[lhs, rhs], code);
                match lt {
                    Ty::Long => match op {
                        Add => code.ladd(),
                        Sub => code.lsub(),
                        Mul => code.lmul(),
                        Div => code.ldiv(),
                        Rem => code.lrem(),
                        _ => unreachable!(),
                    },
                    Ty::Double => match op {
                        Add => code.dadd(),
                        Sub => code.dsub(),
                        Mul => code.dmul(),
                        Div => code.ddiv(),
                        Rem => code.drem(),
                        _ => unreachable!(),
                    },
                    Ty::Float => match op {
                        Add => code.fadd(),
                        Sub => code.fsub(),
                        Mul => code.fmul(),
                        Div => code.fdiv(),
                        Rem => code.frem(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        Add => code.iadd(),
                        Sub => code.isub(),
                        Mul => code.imul(),
                        Div => code.idiv(),
                        Rem => code.irem(),
                        _ => unreachable!(),
                    },
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
                if op == And {
                    code.iand()
                } else {
                    code.ior()
                }
                self.slots.remove(&key);
            }
            BitAnd | BitOr | BitXor => {
                self.emit_operands(&[lhs, rhs], code);
                match lt {
                    Ty::Long => match op {
                        BitAnd => code.land(),
                        BitOr => code.lor(),
                        BitXor => code.lxor(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        BitAnd => code.iand(),
                        BitOr => code.ior(),
                        BitXor => code.ixor(),
                        _ => unreachable!(),
                    },
                }
            }
            Shl | Shr | Ushr => {
                self.emit_operands(&[lhs, rhs], code); // shift amount is an `Int`
                match lt {
                    Ty::Long => match op {
                        Shl => code.lshl(),
                        Shr => code.lshr(),
                        Ushr => code.lushr(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        Shl => code.ishl(),
                        Shr => code.ishr(),
                        Ushr => code.iushr(),
                        _ => unreachable!(),
                    },
                }
            }
            Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe => self.emit_compare(op, lhs, rhs, code),
        }
    }

    fn emit_compare(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        let lt = self.value_ty(lhs);
        // Referential identity (`===`/`!==`) on *reference* operands: compare the two object refs
        // directly with `if_acmp*` (never the structural `Intrinsics.areEqual` the `Eq`/`Ne` reference
        // path uses below). On *primitive* operands Kotlin's `===` is just value `==`, so those fall
        // through to the ordinary numeric comparison after remapping to `Eq`/`Ne`.
        if matches!(op, IrBinOp::RefEq | IrBinOp::RefNe)
            && lt.is_reference()
            && self.value_ty(rhs).is_reference()
        {
            self.emit_operands(&[lhs, rhs], code);
            let t = code.new_label();
            let end = code.new_label();
            self.frame(t, vec![], code);
            if op == IrBinOp::RefEq {
                code.if_acmpeq(t)
            } else {
                code.if_acmpne(t)
            }
            code.push_int(0, self.cw);
            self.frame(end, vec![VerifType::Integer], code);
            code.goto(end);
            code.bind(t);
            code.push_int(1, self.cw);
            code.bind(end);
            return;
        }
        let op = match op {
            IrBinOp::RefEq => IrBinOp::Eq,
            IrBinOp::RefNe => IrBinOp::Ne,
            o => o,
        };
        // `x == null` / `x != null`: compare against null directly with `ifnull`/`ifnonnull` (kotlinc's
        // bytecode), regardless of the operand's static value type. `Intrinsics.areEqual` below is only
        // for two reference operands neither of which is the `null` literal — and a plain `if_icmp*` on
        // a reference (what the numeric path would emit) is only accepted by the verifier when no
        // stackmap frame pins the operand types, so it must not be relied on.
        let lhs_null = matches!(self.ir.expr(lhs), IrExpr::Const(IrConst::Null));
        let rhs_null = matches!(self.ir.expr(rhs), IrExpr::Const(IrConst::Null));
        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) && (lhs_null || rhs_null) {
            let operand = if lhs_null { rhs } else { lhs };
            self.emit_value(operand, code);
            let t = code.new_label();
            let end = code.new_label();
            self.frame(t, vec![], code);
            if op == IrBinOp::Eq {
                code.ifnull(t)
            } else {
                code.ifnonnull(t)
            }
            code.push_int(0, self.cw);
            self.frame(end, vec![VerifType::Integer], code);
            code.goto(end);
            code.bind(t);
            code.push_int(1, self.cw);
            code.bind(end);
            return;
        }
        // Kotlin `==`/`!=` on reference operands is structural (`a?.equals(b)`), realized by the
        // null-safe `kotlin/jvm/internal/Intrinsics.areEqual` — the exact helper kotlinc's JVM backend
        // emits (`intrinsics/Equals.kt`), so the bytecode matches. Primitives keep the
        // `if_icmp*`/3-way-compare path below.
        if matches!(op, IrBinOp::Eq | IrBinOp::Ne)
            && lt.is_reference()
            && self.value_ty(rhs).is_reference()
        {
            // Spill if rhs is branchy (`x == when{…}`) so lhs isn't live across its merge frames.
            self.emit_operands(&[lhs, rhs], code);
            let m = self.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "areEqual",
                "(Ljava/lang/Object;Ljava/lang/Object;)Z",
            );
            code.invokestatic(m, 2, 1);
            if op == IrBinOp::Ne {
                code.push_int(1, self.cw);
                code.ixor();
            }
            return;
        }
        self.emit_operands(&[lhs, rhs], code);
        // Long/Double/Float compare to a 3-way result, then test against 0 with `if_icmp*`. For float
        // types `>`/`>=` use the `*l` variant (NaN → -1) and `<`/`<=` the `*g` variant (NaN → +1), so a
        // NaN operand makes the comparison false either way — matching kotlinc.
        let nan_l = matches!(op, IrBinOp::Gt | IrBinOp::Ge);
        match lt {
            Ty::Long => {
                code.lcmp();
                code.push_int(0, self.cw);
            }
            Ty::Double => {
                if nan_l {
                    code.dcmpl();
                } else {
                    code.dcmpg();
                }
                code.push_int(0, self.cw);
            }
            Ty::Float => {
                if nan_l {
                    code.fcmpl();
                } else {
                    code.fcmpg();
                }
                code.push_int(0, self.cw);
            }
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
        // The `if_icmp*` popped both operands — this is the height on BOTH merge paths (the `t`
        // branch and the fall-through). The 0/1 booleans below each leave exactly one value, so the
        // tracker must be reset to this height at `bind(t)`; otherwise the linear counter carries the
        // fall-through's `push 0` past the `goto`, drifting `cur_stack` +1 (harmless for max_stack, but
        // it makes `stack_height()` over-report, which the branchy-inline baseline check relies on).
        let merged = code.stack_height().max(0) as u16;
        code.push_int(0, self.cw);
        self.frame(end, vec![VerifType::Integer], code);
        code.goto(end);
        code.bind(t);
        code.set_stack(merged);
        code.push_int(1, self.cw);
        code.bind(end);
    }

    /// Emit a conditional jump to `target`, taken exactly when `cond` evaluates to `jump_when_true`.
    /// When `cond` is a primitive/reference comparison it is FUSED into the branch (`if_icmpge`,
    /// `ifnull`, `if_acmpeq`, `lcmp;ifge`, …) instead of materializing a 0/1 boolean and testing it
    /// with `ifeq`/`ifne` — the bytecode kotlinc emits for every `if`/`while`/`for` over a comparison.
    fn emit_cond_branch(
        &mut self,
        cond: u32,
        target: Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) {
        // A constant condition folds: `while (true)` (a `Boolean(true)` pre-test, jump-out-when-false)
        // emits NO branch — a spurious `ifeq end` to the method end leaves a branch target with no
        // stack-map frame. An always-taken branch becomes an unconditional `goto`.
        if let IrExpr::Const(IrConst::Boolean(b)) = *self.ir.expr(cond) {
            // Frame the target regardless (callers — `when`/loop emission — rely on the branch target
            // having a stack-map frame), but only emit the jump when the constant actually takes it.
            self.frame(target, vec![], code);
            if b == jump_when_true {
                code.goto(target);
            }
            return;
        }
        if let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *self.ir.expr(cond) {
            use IrBinOp::*;
            if matches!(op, Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe) {
                self.emit_compare_branch(op, lhs, rhs, target, jump_when_true, code);
                return;
            }
        }
        // Fuse `x is T` / `x !is T` (a reference target) into `instanceof; if{ne,eq}` — no 0/1 boolean is
        // materialized (kotlinc's shape, e.g. a data class `equals`' `instanceof; ifne <ok>`).
        let inst_fuse = if let IrExpr::TypeOp {
            op: to,
            arg,
            type_operand,
        } = self.ir.expr(cond)
        {
            if matches!(to, IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf) {
                let jvm_ty = ir_ty_to_jvm(type_operand);
                (!jvm_ty.is_primitive()).then(|| (*to, *arg, ref_internal(jvm_ty)))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((to, arg, internal)) = inst_fuse {
            self.emit_value(arg, code);
            let ci = self.cw.class_ref(&internal);
            code.instance_of(ci);
            self.frame(target, vec![], code);
            // Stack holds 1 iff `arg instanceof T`. The condition is true on `instanceof` for `InstanceOf`
            // and on `!instanceof` for `NotInstanceOf`; jump when the condition equals `jump_when_true`.
            let jump_on_instance = if matches!(to, IrTypeOp::InstanceOf) {
                jump_when_true
            } else {
                !jump_when_true
            };
            if jump_on_instance {
                code.ifne(target);
            } else {
                code.ifeq(target);
            }
            return;
        }
        self.emit_value(cond, code);
        self.frame(target, vec![], code);
        if jump_when_true {
            code.ifne(target);
        } else {
            code.ifeq(target);
        }
    }

    /// Emit the comparison `lhs <op> rhs` directly as a single conditional jump to `target`, taken when
    /// the comparison's result equals `jt` — no 0/1 boolean is materialized. Mirrors `emit_compare`'s
    /// operand/3-way/null/ref handling but ends in one fused branch with the right polarity.
    fn emit_compare_branch(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        // Referential identity (`===`/`!==`) on references → `if_acmpeq`/`if_acmpne`.
        if matches!(op, RefEq | RefNe) && lt.is_reference() && self.value_ty(rhs).is_reference() {
            self.emit_operands(&[lhs, rhs], code);
            self.frame(target, vec![], code);
            if (op == RefEq) == jt {
                code.if_acmpeq(target);
            } else {
                code.if_acmpne(target);
            }
            return;
        }
        let op = match op {
            RefEq => Eq,
            RefNe => Ne,
            o => o,
        };
        // `x == null` / `x != null` → `ifnull`/`ifnonnull`.
        let lhs_null = matches!(self.ir.expr(lhs), IrExpr::Const(IrConst::Null));
        let rhs_null = matches!(self.ir.expr(rhs), IrExpr::Const(IrConst::Null));
        if matches!(op, Eq | Ne) && (lhs_null || rhs_null) {
            let operand = if lhs_null { rhs } else { lhs };
            self.emit_value(operand, code);
            self.frame(target, vec![], code);
            if (op == Eq) == jt {
                code.ifnull(target);
            } else {
                code.ifnonnull(target);
            }
            return;
        }
        // Reference structural `==`/`!=` → `Intrinsics.areEqual` then test the `Z` result.
        if matches!(op, Eq | Ne) && lt.is_reference() && self.value_ty(rhs).is_reference() {
            self.emit_operands(&[lhs, rhs], code);
            let m = self.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "areEqual",
                "(Ljava/lang/Object;Ljava/lang/Object;)Z",
            );
            code.invokestatic(m, 2, 1);
            self.frame(target, vec![], code);
            if (op == Eq) == jt {
                code.ifne(target); // areEqual true ⇒ equal
            } else {
                code.ifeq(target);
            }
            return;
        }
        // Numeric. A comparison against the integer literal `0` uses the single-operand compare-to-zero
        // branch (`ifeq`/`iflt`/… — kotlinc's form), saving the `iconst_0`. Only the int category; the
        // others compare 3-way through `lcmp`/`dcmp*`/`fcmp*`, which already tests the result vs 0.
        let int_cat = !matches!(lt, Ty::Long | Ty::Double | Ty::Float);
        let zero = |e: u32| matches!(self.ir.expr(e), IrExpr::Const(IrConst::Int(0)));
        if int_cat && zero(rhs) {
            self.emit_value(lhs, code);
            self.frame(target, vec![], code);
            self.cmp0_branch(op, jt, target, code);
            return;
        }
        if int_cat && zero(lhs) {
            self.emit_value(rhs, code);
            self.frame(target, vec![], code);
            self.cmp0_branch(swap_cmp(op), jt, target, code);
            return;
        }
        // int-category fuses to `if_icmp*`; Long/Double/Float → 3-way compare then single-operand `if*`.
        self.emit_operands(&[lhs, rhs], code);
        // `>`/`>=` use the `*l` float-compare variant, `<`/`<=` the `*g` — so NaN yields false (kotlinc).
        let nan_l = matches!(op, Gt | Ge);
        match lt {
            Ty::Long => code.lcmp(),
            Ty::Double => {
                if nan_l {
                    code.dcmpl()
                } else {
                    code.dcmpg()
                }
            }
            Ty::Float => {
                if nan_l {
                    code.fcmpl()
                } else {
                    code.fcmpg()
                }
            }
            _ => {}
        }
        self.frame(target, vec![], code);
        if !int_cat {
            self.cmp0_branch(op, jt, target, code);
        } else {
            match (op, jt) {
                (Lt, true) => code.if_icmplt(target),
                (Lt, false) => code.if_icmpge(target),
                (Le, true) => code.if_icmple(target),
                (Le, false) => code.if_icmpgt(target),
                (Gt, true) => code.if_icmpgt(target),
                (Gt, false) => code.if_icmple(target),
                (Ge, true) => code.if_icmpge(target),
                (Ge, false) => code.if_icmplt(target),
                (Eq, true) => code.if_icmpeq(target),
                (Eq, false) => code.if_icmpne(target),
                (Ne, true) => code.if_icmpne(target),
                (Ne, false) => code.if_icmpeq(target),
                _ => unreachable!(),
            }
        }
    }

    /// A single-operand compare-to-zero branch (`ifeq`/`ifne`/`iflt`/`ifle`/`ifgt`/`ifge`) to `target`,
    /// taken when `(value <op> 0) == jt`. Used for `x <op> 0` and for the 3-way `lcmp`/`dcmp*`/`fcmp*`
    /// result tested against 0.
    fn cmp0_branch(&self, op: IrBinOp, jt: bool, target: Label, code: &mut CodeBuilder) {
        use IrBinOp::*;
        match (op, jt) {
            (Lt, true) => code.iflt(target),
            (Lt, false) => code.ifge(target),
            (Le, true) => code.ifle(target),
            (Le, false) => code.ifgt(target),
            (Gt, true) => code.ifgt(target),
            (Gt, false) => code.ifle(target),
            (Ge, true) => code.ifge(target),
            (Ge, false) => code.iflt(target),
            (Eq, true) => code.ifeq(target),
            (Eq, false) => code.ifne(target),
            (Ne, true) => code.ifne(target),
            (Ne, false) => code.ifeq(target),
            _ => unreachable!(),
        }
    }

    fn emit_when(&mut self, branches: &[(Option<u32>, u32)], code: &mut CodeBuilder) {
        let end = code.new_label();
        let has_else = branches.iter().any(|(c, _)| c.is_none());
        // A `when` with no `else`, or one whose value is `Unit`, is a statement: branch values are
        // discarded and nothing reaches the operand stack at `end`.
        let is_stmt = !has_else || self.value_ty_of_when(branches) == Ty::Unit;
        let result_stack = if is_stmt {
            vec![]
        } else {
            self.verif_stack(self.value_ty_of_when(branches))
        };
        // `end` is reachable if any branch falls through to it (i.e. doesn't return/throw). A
        // no-`else` statement always has the implicit no-match fallthrough.
        let mut end_reachable = !has_else;
        for (cond, body) in branches {
            match cond {
                Some(c) => {
                    // Skip to the next branch when this condition is false (fused comparison branch).
                    let next = code.new_label();
                    self.emit_cond_branch(*c, next, false, code);
                    self.emit_value(*body, code);
                    if !self.diverges(*body) {
                        // A diverging branch (e.g. an inlined `error(...)`) left nothing and ended in
                        // `athrow` — don't discard (nothing to pop) and don't jump to `end`.
                        if is_stmt {
                            discard(self.value_ty(*body), code);
                        }
                        // Only a falling-through branch jumps to (and needs a frame at) `end`.
                        self.frame(end, result_stack.clone(), code);
                        code.goto(end);
                        end_reachable = true;
                    }
                    code.bind(next);
                }
                None => {
                    self.emit_value(*body, code);
                    if !self.diverges(*body) {
                        if is_stmt {
                            discard(self.value_ty(*body), code);
                        }
                        end_reachable = true;
                    }
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
    fn emit_try(
        &mut self,
        body: u32,
        catches: &[crate::ir::IrCatch],
        finally: Option<u32>,
        result: &IrType,
        code: &mut CodeBuilder,
    ) {
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
            if let Some(f) = finally {
                self.emit(f, code);
            } // `finally` inlined on the normal path
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
                if let Some(f) = finally {
                    self.emit(f, code);
                } // `finally` inlined after the catch
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
            // Re-raise the caught exception after the `finally` — unless the `finally` itself transfers
            // control (`finally { return … }` / `finally { throw … }`), in which case the rethrow is
            // unreachable and emitting it would leave a dead instruction without a stackmap frame.
            if !fin_diverges {
                load(thr_ty, tslot, code);
                code.athrow();
            }
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
    /// Resolve a `break`/`continue` target to `(continue_label, break_label)`. `None` → the innermost
    /// loop; `Some(l)` → the nearest enclosing loop carrying `l@`. Falls back to the innermost if the
    /// label isn't found (a compilable program always has the labeled loop in scope).
    fn loop_target(&self, label: &Option<String>) -> (Label, Label) {
        let entry = match label {
            Some(l) => self
                .loop_stack
                .iter()
                .rev()
                .find(|(_, _, sl)| sl.as_deref() == Some(l.as_str()))
                .or_else(|| self.loop_stack.last()),
            None => self.loop_stack.last(),
        };
        let (cont, end, _) = entry.expect("break/continue outside loop");
        (*cont, *end)
    }

    /// never falls through past it. Used to suppress dead `goto`s and unreachable merge frames.
    fn diverges(&self, e: u32) -> bool {
        match self.ir.expr(e) {
            IrExpr::Return(_)
            | IrExpr::Throw { .. }
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. } => true,
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
            IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                finally.map_or(false, |f| self.diverges(f))
                    || (self.diverges(*body) && catches.iter().all(|c| self.diverges(c.body)))
            }
            // A `Nothing`-typed call never returns — an inlined `error(...)`/`throw`-helper diverges via
            // `athrow`, so the branch it ends doesn't fall through to the merge.
            IrExpr::Call { .. } | IrExpr::MethodCall { .. } => self.value_ty(e) == Ty::Nothing,
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
        let last = branches
            .iter()
            .rev()
            .find(|(_, b)| !self.diverges(*b))
            .map(|(_, b)| self.value_ty(*b))
            .unwrap_or(Ty::Unit);
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
        // When the falling-through branches are references of DIFFERENT classes (`if (c) Foo() else Bar()`,
        // joined by the checker to `Any`), the merge-point stack type must be a common supertype — krusty
        // uses `Object`. Each branch value is a subtype, so the merge frame (`Object`) verifies; the last
        // branch's own (more specific) class would mismatch the other predecessor's value (a VerifyError).
        if last.is_reference() {
            // Compare by the JVM internal name (`String` and `Obj("java/lang/String")` are the same type
            // but distinct `Ty` values), so only a genuinely differing class triggers the `Object` merge.
            let internal = |t: &Ty| -> Option<String> {
                match t {
                    Ty::String => Some("java/lang/String".to_string()),
                    Ty::Obj(n, _) => Some(n.to_string()),
                    Ty::Array(_) => Some(t.descriptor()),
                    _ => None,
                }
            };
            let mut names = branches
                .iter()
                .filter(|(_, b)| !self.diverges(*b))
                .map(|(_, b)| self.value_ty(*b))
                .filter(|t| !matches!(t, Ty::Null | Ty::Nothing | Ty::Error))
                .filter_map(|t| internal(&t));
            if let Some(first) = names.next() {
                if names.any(|n| n != first) {
                    return Ty::obj("kotlin/Any");
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
            IrExpr::StringConcat(_) => Ty::String,
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
            IrExpr::GetValue(i) => self
                .slots
                .get(i)
                .map(|(_, t)| *t)
                .or_else(|| self.var_types.get(i).copied())
                .unwrap_or(Ty::Error),
            IrExpr::GetField { class, index, .. } => {
                ir_ty_to_jvm(&self.ir.classes[*class as usize].fields[*index as usize].1)
            }
            IrExpr::GetStatic(i) => ir_ty_to_jvm(&self.ir.statics[*i as usize].ty),
            IrExpr::New { class, .. } => Ty::obj(&self.ir.classes[*class as usize].fq_name),
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                ir_ty_to_jvm(&self.ir.functions[fid as usize].ret)
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                ..
            } => match callee {
                Callee::Local(fid) => ir_ty_to_jvm(&self.ir.functions[*fid as usize].ret),
                Callee::CrossFile { ret, .. } => ir_ty_to_jvm(ret),
                // Array `get` returns the receiver's element; an array `<init>` returns the array type.
                Callee::External(fq) if fq == "kotlin/Array.get" => dispatch_receiver
                    .map(|r| {
                        // A boxed primitive array yields the UNBOXED primitive (`a[i]: Int`).
                        let e = self.array_elem(r);
                        boxed_prim_of(e).unwrap_or(e)
                    })
                    .unwrap_or(Ty::Error),
                Callee::External(fq) if prim_array_elem_ty(fq).is_some() => {
                    Ty::array(prim_array_elem_ty(fq).unwrap())
                }
                Callee::External(fq) => intrinsic_ret(fq),
                Callee::Static { descriptor, .. }
                | Callee::Virtual { descriptor, .. }
                | Callee::Special { descriptor, .. } => {
                    // A kotlin `Nothing` return is a `java/lang/Void` JVM descriptor — report it as
                    // `Nothing` so a diverging (inlined `error(...)`) call is treated as never returning
                    // (no value, no dead epilogue after the spliced `athrow`).
                    if descriptor.ends_with(")Ljava/lang/Void;") {
                        Ty::Nothing
                    } else {
                        ty_from_descriptor_ret(descriptor)
                    }
                }
                Callee::CrossFileVirtual { ret, .. } => ir_ty_to_jvm(ret),
            },
            IrExpr::PrimitiveBinOp { op, lhs, .. } => match op {
                IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne
                | IrBinOp::RefEq
                | IrBinOp::RefNe
                | IrBinOp::And
                | IrBinOp::Or => Ty::Boolean,
                _ => self.value_ty(*lhs),
            },
            IrExpr::When { branches } => self.value_ty_of_when(branches),
            IrExpr::EnumEntry { class, .. } | IrExpr::EnumValueOf { class, .. } => {
                Ty::obj(&self.ir.classes[*class as usize].fq_name)
            }
            IrExpr::StaticInstance { ty, .. } => Ty::obj(&self.ir.classes[*ty as usize].fq_name),
            IrExpr::ExternalStaticInstance { ty, .. } => Ty::obj(ty),
            IrExpr::ExternalStaticField { descriptor, .. } => {
                // The static field's JVM type, from its descriptor (an object `L…;` for an `object`'s
                // INSTANCE; primitives for the rare const-field case).
                match descriptor.as_str() {
                    "J" => Ty::Long,
                    "D" => Ty::Double,
                    "I" => Ty::Int,
                    "Z" => Ty::Boolean,
                    d => d
                        .strip_prefix('L')
                        .and_then(|s| s.strip_suffix(';'))
                        .map(Ty::obj)
                        .unwrap_or(Ty::obj("java/lang/Object")),
                }
            }
            IrExpr::RefNew { elem, .. } => Ty::obj(ref_class(elem).0),
            IrExpr::RefGet { elem, .. } => ir_ty_to_jvm(elem),
            IrExpr::RefSet { .. } => Ty::Unit,
            IrExpr::EnumValues { class } => {
                Ty::array(Ty::obj(&self.ir.classes[*class as usize].fq_name))
            }
            IrExpr::Block { value, .. } => value.map(|v| self.value_ty(v)).unwrap_or(Ty::Unit),
            IrExpr::TypeOp {
                op, type_operand, ..
            } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                _ => ir_ty_to_jvm(type_operand),
            },
            IrExpr::Lambda { arity, .. } => {
                Ty::obj(&format!("kotlin/jvm/functions/Function{arity}"))
            }
            IrExpr::InvokeFunction { ret, .. } => ir_ty_to_jvm(ret),
            IrExpr::NotNullAssert { operand } => self.value_ty(*operand),
            IrExpr::NewExternal { internal, .. } => Ty::obj(internal),
            IrExpr::NewCrossFile { internal, .. } => Ty::obj(internal),
            IrExpr::Throw { .. } | IrExpr::Break { .. } | IrExpr::Continue { .. } => Ty::Nothing,
            IrExpr::Vararg { element_type, .. } => Ty::array(ir_ty_to_jvm(element_type)),
            IrExpr::NewArray { element_type, .. } => Ty::array(ir_ty_to_jvm(element_type)),
            IrExpr::UnitInstance => Ty::obj("kotlin/Unit"),
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
        Some(b'L') => Ty::obj(
            d.strip_prefix('L')
                .and_then(|s| s.strip_suffix(';'))
                .unwrap_or(d),
        ),
        Some(b'[') => Ty::array(ty_from_field_descriptor(&d[1..])),
        _ => Ty::Error,
    }
}

fn emit_num_conv(from: Ty, to: Ty, code: &mut CodeBuilder) {
    use Ty::*;
    if from == to {
        return;
    }
    let wide = |t: Ty| match t {
        Byte | Short | Char | Int => Int,
        o => o,
    };
    match (wide(from), wide(to)) {
        (Int, Long) => code.i2l(),
        (Int, Float) => code.i2f(),
        (Int, Double) => code.i2d(),
        (Long, Int) => code.l2i(),
        (Long, Float) => code.l2f(),
        (Long, Double) => code.l2d(),
        (Float, Int) => code.f2i(),
        (Float, Long) => code.f2l(),
        (Float, Double) => code.f2d(),
        (Double, Int) => code.d2i(),
        (Double, Long) => code.d2l(),
        (Double, Float) => code.d2f(),
        _ => {} // same wide category (e.g. Byte→Int): the value is already correct on the stack
    }
    match to {
        Byte => code.i2b(),
        Short => code.i2s(),
        Char => code.i2c(),
        _ => {}
    }
}

fn ref_internal(t: Ty) -> String {
    match t {
        Ty::String => "java/lang/String".to_string(),
        // Erase a Kotlin built-in name (`kotlin/collections/MutableList`) to its JVM identity here at the
        // bytecode boundary, so `instanceof`/`checkcast`/method-owner refs never leak a Kotlin-only name.
        Ty::Obj(n, _) => crate::jvm::jvm_class_map::to_jvm_internal(n).to_string(),
        Ty::Array(_) => t.descriptor(),
        _ => "java/lang/Object".to_string(),
    }
}

fn intrinsic_ret(fq: &str) -> Ty {
    match fq {
        "kotlin/String.plus" | "kotlin/Any.toString" => Ty::String,
        "kotlin/Any.hashCode" => Ty::Int,
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
/// If `t` is the boxed-reference form of a primitive (the element of a `Array<Int>` etc., carried as
/// `Obj("kotlin/Int")`), the underlying primitive `Ty`. Used to insert box/unbox at the boxed-array
/// element boundary (`a[i]` yields an unboxed `Int`; `a[i] = v` boxes the `Int`).
fn boxed_prim_of(t: Ty) -> Option<Ty> {
    match t {
        Ty::Obj("kotlin/Int", _) => Some(Ty::Int),
        Ty::Obj("kotlin/Long", _) => Some(Ty::Long),
        Ty::Obj("kotlin/Short", _) => Some(Ty::Short),
        Ty::Obj("kotlin/Byte", _) => Some(Ty::Byte),
        Ty::Obj("kotlin/Double", _) => Some(Ty::Double),
        Ty::Obj("kotlin/Float", _) => Some(Ty::Float),
        Ty::Obj("kotlin/Boolean", _) => Some(Ty::Boolean),
        Ty::Obj("kotlin/Char", _) => Some(Ty::Char),
        _ => None,
    }
}

fn array_load_op(elem: Ty) -> (u8, i32) {
    match elem {
        Ty::Int => (0x2e, 1),
        Ty::Long => (0x2f, 2),
        Ty::Float => (0x30, 1),
        Ty::Double => (0x31, 2),
        Ty::Boolean | Ty::Byte => (0x33, 1),
        Ty::Char => (0x34, 1),
        Ty::Short => (0x35, 1),
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
        Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => {
            code.push_int(0, cw);
        }
        _ => code.aconst_null(),
    }
}

fn array_store_op(elem: Ty) -> (u8, i32) {
    match elem {
        Ty::Int => (0x4f, 1),
        Ty::Long => (0x50, 2),
        Ty::Float => (0x51, 1),
        Ty::Double => (0x52, 2),
        Ty::Boolean | Ty::Byte => (0x54, 1),
        Ty::Char => (0x55, 1),
        Ty::Short => (0x56, 1),
        _ => (0x53, 1), // aastore
    }
}

/// `newarray` atype for a primitive element (JVMS Table 6.5.newarray-A).
fn prim_newarray_atype(elem: Ty) -> u8 {
    match elem {
        Ty::Boolean => 4,
        Ty::Char => 5,
        Ty::Float => 6,
        Ty::Double => 7,
        Ty::Byte => 8,
        Ty::Short => 9,
        Ty::Long => 11,
        _ => 10, // int
    }
}

pub fn ir_ty_to_jvm(t: &IrType) -> Ty {
    match t {
        IrType::Unit => Ty::Unit,
        IrType::Nothing => Ty::Nothing,
        IrType::Class {
            fq_name, type_args, ..
        } => match fq_name.as_str() {
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
            // A `kotlin/Array<T>` is a JVM reference array: a primitive element `T` is BOXED
            // (`Array<Int>` = `[Ljava/lang/Integer;`, distinct from the unboxed `IntArray` = `[I`).
            "kotlin/Array" => Ty::array(
                type_args
                    .first()
                    .map(|e| {
                        let et = ir_ty_to_jvm(e);
                        et.boxed_ref().unwrap_or(et)
                    })
                    .unwrap_or(Ty::obj("java/lang/Object")),
            ),
            _ => Ty::obj(fq_name),
        },
        // The JVM representation of a function type is `kotlin/jvm/functions/FunctionN`. A `suspend`
        // function type carries a trailing `Continuation` parameter, so its arity is one greater.
        IrType::Function {
            params, suspend, ..
        } => Ty::obj(&format!(
            "kotlin/jvm/functions/Function{}",
            params.len() + usize::from(*suspend)
        )),
        _ => Ty::Error,
    }
}

/// Swap the operands of a comparison operator (`a < b` ≡ `b > a`) — used to normalize `0 <op> x` into
/// `x <swapped-op> 0` so the single-operand compare-to-zero branch applies.
fn swap_cmp(op: IrBinOp) -> IrBinOp {
    use IrBinOp::*;
    match op {
        Lt => Gt,
        Le => Ge,
        Gt => Lt,
        Ge => Le,
        o => o,
    }
}

/// The `String.valueOf` overload descriptor for a single interpolated value's type (`"$x"`).
fn valueof_desc(t: Ty) -> &'static str {
    match t {
        Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/String;",
        Ty::Long => "(J)Ljava/lang/String;",
        Ty::Float => "(F)Ljava/lang/String;",
        Ty::Double => "(D)Ljava/lang/String;",
        Ty::Boolean => "(Z)Ljava/lang/String;",
        Ty::Char => "(C)Ljava/lang/String;",
        _ => "(Ljava/lang/Object;)Ljava/lang/String;",
    }
}

/// `true` if a lowered IR type is a nullable reference (`String?` etc.).
fn ir_ty_nullable(t: &IrType) -> bool {
    matches!(t, IrType::Class { nullable: true, .. })
}

/// JVM accessor names for a top-level property, matching kotlinc: `x`→`getX`/`setX`; an `is`-prefixed
/// boolean keeps its name on the getter (`isOpen`→`isOpen`) and drops `is` on the setter (`setOpen`).
fn prop_getter_name(prop: &str) -> String {
    let b = prop.as_bytes();
    if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() {
        return prop.to_string();
    }
    let mut c = prop.chars();
    format!("get{}{}", c.next().unwrap().to_uppercase(), c.as_str())
}

fn prop_setter_name(prop: &str) -> String {
    let b = prop.as_bytes();
    let base = if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() {
        &prop[2..]
    } else {
        prop
    };
    let mut c = base.chars();
    format!("set{}{}", c.next().unwrap().to_uppercase(), c.as_str())
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

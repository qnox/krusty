//! The facade's and each class's STATIC fields, and the `<clinit>` that initializes them.
//!
//! A `const val` whose initializer is a compile-time literal carries a `ConstantValue` attribute and
//! is not assigned at run time at all; everything else is stored by `<clinit>` in declaration order.
//! Field flags are decided here too, and they are not uniform: a `const val`'s field takes its
//! DECLARATION's visibility, while a plain `val`/`var` is private whatever the source said, because
//! every reader outside the facade goes through the generated accessor.

use super::*;

/// The constant-pool index for a `const val`'s `ConstantValue` attribute when its initializer is a
/// compile-time literal; `None` otherwise (then the field is initialized in `<clinit>` as before).
pub(super) fn const_value_idx(
    ir: &IrFile,
    init: crate::ir::ExprId,
    cw: &mut ClassWriter,
) -> Option<u16> {
    use crate::ir::{IrConst, IrExpr};
    match ir.expr(init) {
        IrExpr::Const(c) => Some(match c {
            IrConst::Boolean(b) => cw.const_int(*b as i32),
            IrConst::Byte(v) => cw.const_int(*v as i32),
            IrConst::Short(v) => cw.const_int(*v as i32),
            IrConst::Int(v) => cw.const_int(*v),
            // `UByte`/`UShort` ride in the `B`/`S` their value class wraps.
            IrConst::UByte(v) => cw.const_int(i32::from(*v as i8)),
            IrConst::UShort(v) => cw.const_int(i32::from(*v as i16)),
            IrConst::UInt(v) => cw.const_int(*v as i32),
            IrConst::ULong(v) => cw.const_long(*v as i64),
            IrConst::Char(c) => cw.const_int(*c as i32),
            IrConst::Long(v) => cw.const_long(*v),
            IrConst::Float(v) => cw.const_float(*v),
            IrConst::Double(v) => cw.const_double(*v),
            IrConst::String(s) => cw.const_string_kt(s),
            IrConst::Null => return None,
        }),
        _ => None,
    }
}

/// Whether `init` is a `ConstantValue`-eligible literal (mirrors [`const_value_idx`] without interning).
pub(super) fn const_value_idx_peek(ir: &IrFile, init: crate::ir::ExprId) -> bool {
    matches!(ir.expr(init), crate::ir::IrExpr::Const(c) if !matches!(c, crate::ir::IrConst::Null))
}

pub(super) fn emit_statics(ir: &IrFile, facade: &str, cw: &mut ClassWriter, env: &EmitEnv) {
    // Statics OWNED by a specific class (a companion `const val`) are emitted on that class, not the
    // facade — see `emit_owned_consts`.
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let facade_statics: Vec<(u32, &crate::ir::IrStatic)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, property)| property.is_facade_owned())
        .map(|(index, property)| (index as u32, property))
        .collect();
    if facade_statics.is_empty() {
        return;
    }
    for &(static_index, s) in &facade_statics {
        // kotlinc: `const val` → `static final` at the DECLARATION's visibility; a plain `val` →
        // `private static final`; a `var` → `private static` (mutated through the synthesized
        // setter). The private field is read/written directly only from within the facade; other
        // classes go through the get/set accessors.
        //
        // A `const val` is the one shape whose field visibility follows the source: measured against
        // kotlinc 2.4.10, `private const val` is `private static final` (0x001A) while `internal`
        // and `public` are both `public static final` (0x0019) — `internal` is a Kotlin-only
        // boundary with no JVM spelling. Publishing a private one as public leaks a declaration the
        // source hid, and it is the whole difference on a facade that otherwise matches.
        let acc = if ir.is_jvm_field_static(static_index) {
            0x0009 | if s.is_var { 0 } else { 0x0010 } // PUBLIC | STATIC [| FINAL]
        } else if s.is_const {
            let visibility = if s.visibility.is_private() {
                0x0002
            } else {
                0x0001
            };
            visibility | 0x0018 // [PRIVATE | PUBLIC] | STATIC | FINAL
        } else if s.is_var {
            0x000A // PRIVATE | STATIC
        } else {
            0x001A // PRIVATE | STATIC | FINAL
        };
        let desc = ir_type_desc(&s.ty);
        // A PARAMETERIZED type (`val xs: List<String>`) carries its full generic `Signature`, exactly
        // as the same property declared inside a class does — the facade's field table is a different
        // emitter, and without this a top-level property's element type was lost to erasure.
        let signatures = property_jvm_signatures(&signature_formatter, &s.ty, None);
        // A reference-typed facade static carries kotlinc's nullability annotation like any other
        // backing field.
        let nullability = field_nullability_kind(ir, facade, &s.name, s.ty);
        let field_ann = match nullability {
            1 => Some("Lorg/jetbrains/annotations/NotNull;"),
            2 => Some("Lorg/jetbrains/annotations/Nullable;"),
            _ => None,
        };
        // A `const val` initialized by a compile-time literal carries a `ConstantValue` attribute (the
        // JVM initializes the field; its `<clinit>` store is omitted below) — byte-identical to kotlinc.
        // LATE adds: kotlinc visits the facade's fields AFTER its methods, so a backing field's name
        // first interns at its accessor body and the const payload lands after the `<clinit>` window.
        let cv = (s.is_const && const_value_idx_peek(ir, s.init))
            .then(|| match ir.expr(s.init) {
                crate::ir::IrExpr::Const(c) if !matches!(c, crate::ir::IrConst::Null) => {
                    Some(c.clone())
                }
                _ => None,
            })
            .flatten();
        cw.add_field_late_sig(
            acc,
            &s.name,
            &desc,
            signatures.field.as_deref(),
            cv,
            field_ann,
        );
    }
    // Which statics a CLASS body (a different JVM class than the facade) reads/writes — a PRIVATE
    // top-level property has no public accessors, so those references need kotlinc's `access$get<X>$p` /
    // `access$set<X>$p` bridges (emitted below, only when actually referenced).
    let mut cross_get: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut cross_set: std::collections::HashSet<u32> = std::collections::HashSet::new();
    {
        let mut roots: Vec<u32> = Vec::new();
        for c in &ir.classes {
            for &fid in &c.methods {
                if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                    roots.push(b);
                }
            }
            roots.extend(c.init_body);
            roots.extend(c.super_arg_prelude.iter().copied());
            roots.extend(c.super_args.iter().copied());
            for sc in &c.secondary_ctors {
                roots.extend(sc.body);
                roots.extend(sc.delegate_prelude.iter().copied());
                roots.extend(sc.delegate_args.iter().copied());
            }
            for en in &c.enum_entries {
                roots.extend(en.args.iter().copied());
            }
        }
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            match &ir.exprs[cur as usize] {
                IrExpr::GetStatic(i) => {
                    cross_get.insert(*i);
                }
                IrExpr::SetStatic { index, .. } => {
                    cross_set.insert(*index);
                }
                _ => {}
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
    }
    // A PRIVATE property gets NO public accessors — only the `access$…$p` bridges, and only when
    // referenced. kotlinc's SyntheticAccessorLowering appends them after every declared and lifted
    // member, so they trail the facade's methods; the public accessors are placed by
    // `emit_static_accessors` at the property's source position.
    for (sidx, s) in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_facade_owned())
    {
        // A `const val` inlines (no accessor); a CUSTOM-accessor property emits its `getX`/`setX` as
        // ordinary facade methods (from `ir.functions`), so skip the trivial auto-accessor here.
        if s.is_const
            || s.custom_accessor
            || ir.is_jvm_field_static(sidx as u32)
            || !s.visibility.is_private()
        {
            continue;
        }
        let jt = jvm_declared_ty(&s.ty);
        let desc = type_descriptor(jt);
        {
            if cross_get.contains(&(sidx as u32)) {
                let mut g = CodeBuilder::new(0);
                let fref = cw.fieldref(facade, &s.name, &desc);
                g.getstatic(fref, slot_words(jt) as i32);
                emit_return(jt, &mut g);
                g.ensure_locals(0);
                g.link();
                cw.add_method(
                    0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
                    &format!("access${}$p", property_getter_name(&s.name)),
                    &format!("(){desc}"),
                    &g,
                );
            }
            if s.is_var && cross_set.contains(&(sidx as u32)) {
                let words = slot_words(jt);
                let mut st = CodeBuilder::new(words);
                load(jt, 0, &mut st);
                let fref = cw.fieldref(facade, &s.name, &desc);
                st.putstatic(fref, slot_words(jt) as i32);
                st.ret_void();
                st.ensure_locals(words);
                st.link();
                cw.add_method(
                    0x1019,
                    &format!("access${}$p", property_setter_name(&s.name)),
                    &format!("({desc})V"),
                    &st,
                );
            }
        }
    }
    // A store the JVM already performs is pure redundancy: kotlinc emits no `<clinit>` store for a
    // `const val` folded into a `ConstantValue`, nor for an initializer that IS the field's default
    // (`val absent: String? = null`, `var count: Int = 0`) — the same elision instance fields get
    // from `elide_default_property_stores`.
    let should_store = |s: &crate::ir::IrStatic| {
        !(ir.is_storage_default(s.init) || s.is_const && const_value_idx_peek(ir, s.init))
    };
    // kotlinc visits `<clinit>` (name + descriptor) before the initializer constants its body
    // interns. With nothing left to store there is NO `<clinit>` at all, so reserve only when a
    // store will be emitted.
    if !facade_statics
        .iter()
        .any(|(_, property)| should_store(property))
    {
        return;
    }
    cw.reserve_method_name("<clinit>");
    cw.seed_utf8("()V");
    let mut e = Emitter::new(
        ir,
        cw,
        env,
        facade,
        facade,
        Ty::Unit,
        facade_statics.iter().map(|(_, property)| property.init),
    );
    let mut code = CodeBuilder::new(0);
    // Each store maps to its property's declaration line (kotlinc's `<clinit>` LineNumberTable).
    // `add_method` drops a `<clinit>`'s inline marks (they are curated), so collect + set after.
    let mut clinit_lines: Vec<(u16, u32)> = Vec::new();
    for &(index, s) in &facade_statics {
        if !should_store(s) {
            continue;
        }
        if s.line != 0 {
            clinit_lines.push((code.bytes.len() as u16, s.line));
        }
        e.emit_static_initializer_store(facade, index, &mut code);
    }
    code.ret_void();
    finish_code::<0x0008>(e.cw, "<clinit>", "()V", &mut code, e.frame.max());
    if !clinit_lines.is_empty() {
        e.cw.set_method_lines("<clinit>", "()V", &clinit_lines);
    }
}

/// The public `getX`/`setX` of one plain static property stored on `owner`, emitted at the
/// property's place among its owner's declared members: kotlinc's JvmPropertiesLowering replaces each
/// property with its accessors in place. The owner is the file facade for a top-level property and
/// the declaring class for a `companion { … }` block property. A `const val`, a `@JvmField`, a
/// custom-accessor property (its accessors are ordinary functions) and a private property (reached
/// only through `access$…$p` bridges or its field) publish none here.
pub(super) fn emit_static_accessors(
    ir: &IrFile,
    owner: &str,
    cw: &mut ClassWriter,
    env: &EmitEnv,
    param_assertions: bool,
    static_index: u32,
) {
    let s = &ir.statics[static_index as usize];
    if s.is_const
        || s.custom_accessor
        || ir.is_jvm_field_static(static_index)
        || s.visibility.is_private()
    {
        return;
    }
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let jt = jvm_declared_ty(&s.ty);
    let desc = type_descriptor(jt);
    // kotlinc visits the accessor's name, descriptor, and nullability annotation BEFORE its
    // body's field cluster; the accessor maps to the property's declaration line.
    //
    // An accessor's nullability is the PROPERTY's, which for a value-class-typed static whose
    // storage was erased is no longer readable off `s.ty` — that holds the carrier now. Reading
    // it there published a non-null `String` setter for a `var x: Label?` and refused the null
    // the property accepts, so the declaration's own recorded type answers instead.
    let accessor_ty = s.erased_declared_ty.unwrap_or(s.ty);
    let nullability = field_nullability_kind(ir, owner, &s.name, accessor_ty);
    let acc_ann = match nullability {
        1 => Some("Lorg/jetbrains/annotations/NotNull;"),
        2 => Some("Lorg/jetbrains/annotations/Nullable;"),
        _ => None,
    };
    // The accessors erase the property's type arguments in their descriptors, so each carries the
    // same generic `Signature` its backing field does — kotlinc signs `getXs()` as
    // `()Ljava/util/List<Ljava/lang/String;>;` and `setXs(List)` as `(Ljava/util/List<…>;)V`.
    let signatures = property_jvm_signatures(&signature_formatter, &s.ty, None);
    let gname = property_getter_name(&s.name);
    cw.reserve_method_name(&gname);
    cw.seed_utf8(&format!("(){desc}"));
    // kotlinc interns an accessor's `Signature` between its descriptor and its nullability
    // annotation, BEFORE the body's field cluster.
    if let Some(signature) = &signatures.getter {
        cw.seed_utf8(signature);
    }
    if let Some(a) = acc_ann {
        cw.seed_utf8(a);
    }
    let mut g = CodeBuilder::new(0);
    if s.line != 0 {
        g.mark_line(s.line);
    }
    let fref = cw.fieldref(owner, &s.name, &desc);
    g.getstatic(fref, slot_words(jt) as i32);
    emit_return(jt, &mut g);
    finish_code_sig::<0x0019>(
        cw,
        &gname,
        &format!("(){desc}"),
        &mut g,
        0,
        signatures.getter.as_deref(),
    );
    cw.set_method_nullability(&gname, &format!("(){desc}"), acc_ann, &[None]);
    if s.is_var {
        // A value-class-typed property's setter carries the value-class mangle: the parameter
        // it takes is the carrier, and a value-class PARAMETER always mangles.
        let sname = s
            .setter_jvm_name
            .clone()
            .unwrap_or_else(|| property_setter_name(&s.name));
        cw.reserve_method_name(&sname);
        cw.seed_utf8(&format!("({desc})V"));
        if let Some(signature) = &signatures.setter {
            cw.seed_utf8(signature);
        }
        let words = slot_words(jt);
        let mut st = CodeBuilder::new(words);
        // kotlinc guards a non-null reference setter parameter with checkNotNullParameter("<set-?>").
        // `-Xno-param-assertions` removes it, like every other parameter guard.
        if param_assertions && jt.is_reference() && nullability == 1 {
            st.aload(0);
            st.push_string("<set-?>", cw);
            let m = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullParameter",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            );
            st.invokestatic(m, 2, 0);
        }
        // The store maps to the property line at the POST-GUARD pc (kotlinc's shape).
        if s.line != 0 {
            st.mark_line(s.line);
        }
        load(jt, 0, &mut st);
        let fref = cw.fieldref(owner, &s.name, &desc);
        st.putstatic(fref, slot_words(jt) as i32);
        st.ret_void();
        finish_code_sig::<0x0019>(
            cw,
            &sname,
            &format!("({desc})V"),
            &mut st,
            words,
            signatures.setter.as_deref(),
        );
        cw.set_method_nullability(&sname, &format!("({desc})V"), None, &[acc_ann]);
        // The setter's value parameter is kotlinc's synthetic `<set-?>`, live for the body.
        cw.set_method_debug(
            &sname,
            &format!("({desc})V"),
            None,
            &[("<set-?>".to_string(), desc.clone(), 0)],
        );
    }
}

/// A `companion object`'s `const val`s live on THIS (outer) class as `public static final` +
/// `ConstantValue` fields (kotlinc's layout); they have no `<clinit>` store (the JVM initializes them).
/// DECLARATION order: common lowering groups declarations by shape, while kotlinc's field table
/// follows the companion source order.
pub(super) fn emit_class_static_fields(
    ir: &IrFile,
    c: &IrClass,
    fq_name: &str,
    signature_formatter: &JvmSignatureFormatter<'_>,
    cw: &mut ClassWriter,
) {
    let mut owner_statics: Vec<(u32, &crate::ir::IrStatic)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, s)| s.owner_matches(fq_name))
        .map(|(index, property)| (index as u32, property))
        .collect();
    owner_statics.sort_by_key(|(_, property)| property.line);
    for (static_index, s) in owner_statics {
        let desc = ir_type_desc(&s.ty);
        // A `private const val`/`private val` on an object/companion keeps its declared visibility
        // (kotlinc: PRIVATE static final; const reads are inlined so no cross-class getstatic needs it).
        // A `var` is reassignable, so it must NOT carry ACC_FINAL — a `putstatic` on a final field
        // outside `<clinit>` is an IllegalAccessError.
        let final_flag = if s.is_var { 0x0000 } else { 0x0010 };
        // A HOISTED companion property's field is PRIVATE regardless of the property's declared
        // visibility (kotlinc: every access goes through the accessors/bridges, never the field) —
        // EXCEPT under `@JvmField`, where the PUBLIC field IS the property's whole JVM surface
        // (kotlinc emits it public even for an `internal` declaration, with no accessors at all).
        let hoisted = ir.is_jvm_companion_hoisted_static(static_index);
        let jvm_field = ir.is_jvm_field_static(static_index);
        let block_storage = companion_blocks::storage_is_private(ir, static_index);
        let acc = if (s.visibility.is_private() || hoisted || block_storage) && !jvm_field {
            0x000A | final_flag // PRIVATE | STATIC [| FINAL]
        } else {
            0x0009 | final_flag // PUBLIC | STATIC [| FINAL]
        };
        // `ConstantValue` is only meaningful on a FINAL field (JVMS 4.7.2 ignores it otherwise), and a
        // `var` is initialized by the `<clinit>` store anyway. A HOISTED companion property never
        // folds either — kotlinc initializes it in `<clinit>` (only `const val` gets the attribute).
        // LATE adds: kotlinc's field-table visit runs after the methods, so a hoisted static's name
        // first interns at its `access$…$cp` bridge and a folded const's name + `ConstantValue`
        // land after the `<clinit>` window.
        let fold = s.is_const && !s.is_var && !hoisted;
        let cv = fold
            .then(|| match ir.expr(s.init) {
                crate::ir::IrExpr::Const(c) if !matches!(c, crate::ir::IrConst::Null) => {
                    Some(c.clone())
                }
                _ => None,
            })
            .flatten();
        // Generated storage with no declaration of its own is ACC_SYNTHETIC and unannotated.
        let synthetic = ir.is_compiler_generated_static(static_index);
        let acc = if synthetic { acc | 0x1000 } else { acc };
        // Reference-typed statics, including private hoisted fields, carry nullability annotations.
        let ann = (!synthetic && (desc.starts_with('L') || desc.starts_with('['))).then(|| {
            if s.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        let signature = property_jvm_signatures(signature_formatter, &s.ty, None).field;
        cw.add_field_late_sig(
            acc,
            ir.static_field_jvm_name(static_index),
            &desc,
            signature.as_deref(),
            cv,
            ann,
        );
        // A field carries its FIELD-targeted annotations as `RuntimeInvisibleAnnotations`, BEFORE
        // the nullability entry — kotlinc's attribute order.
        //
        // They come from wherever the field's DECLARATION lives: a hoisted companion property
        // records them on the COMPANION, while a static this class owns outright — one a compiler
        // plugin generated, say — records them on the class itself. The companion path keeps its
        // `@JvmField` condition, which is what that annotation has always meant there: it is the
        // reason the field is the property's whole JVM surface.
        if let Some(annotations) = c
            .companion_class
            .and_then(|companion| ir.class_id_by_name(companion))
            .filter(|_| jvm_field)
            .and_then(|companion| {
                ir.classes[companion as usize]
                    .field_annotations
                    .iter()
                    .find(|annotations| annotations.field == s.name)
            })
            .or_else(|| {
                c.field_annotations
                    .iter()
                    .find(|annotations| annotations.field == s.name)
            })
        {
            cw.set_last_late_field_annotations(&annotations.annotations);
        }
    }
}

/// HOISTED companion properties: the private static field lives on THIS class, so the companion's
/// delegating accessors reach it through PUBLIC synthetic `access$get<X>$cp`/`access$set<X>$cp`
/// bridges — emitted AFTER the instance methods, right before `<clinit>` (kotlinc's order).
pub(super) fn emit_hoisted_companion_bridges(ir: &IrFile, fq_name: &str, cw: &mut ClassWriter) {
    for (static_index, s) in ir.statics.iter().enumerate().filter(|(index, s)| {
        ir.is_jvm_companion_hoisted_static(*index as u32)
            && !ir.is_jvm_field_static(*index as u32)
            && s.owner_matches(fq_name)
    }) {
        let field_name = ir.static_field_jvm_name(static_index as u32);
        let jt = jvm_declared_ty(&s.ty);
        let desc = type_descriptor(jt);
        // kotlinc visits the bridge's name before its body's field cluster.
        let getter_bridge = format!("access${}$cp", property_getter_name(&s.name));
        cw.reserve_method_name(&getter_bridge);
        cw.seed_utf8(&format!("(){desc}"));
        let mut g = CodeBuilder::new(0);
        let fref = cw.fieldref(fq_name, field_name, &desc);
        g.getstatic(fref, slot_words(jt) as i32);
        emit_return(jt, &mut g);
        g.ensure_locals(0);
        g.link();
        cw.add_method(
            0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
            &getter_bridge,
            &format!("(){desc}"),
            &g,
        );
        if s.is_var {
            let setter_bridge = format!("access${}$cp", property_setter_name(&s.name));
            cw.reserve_method_name(&setter_bridge);
            cw.seed_utf8(&format!("({desc})V"));
            // The setter bridge's `<set-?>` LocalVariableTable strings intern at its method visit.
            cw.seed_utf8("<set-?>");
            cw.seed_utf8(&desc);
            let words = slot_words(jt);
            let mut st = CodeBuilder::new(words);
            load(jt, 0, &mut st);
            let fref = cw.fieldref(fq_name, field_name, &desc);
            st.putstatic(fref, slot_words(jt) as i32);
            st.ret_void();
            st.ensure_locals(words);
            st.link();
            cw.add_method(0x1019, &setter_bridge, &format!("({desc})V"), &st);
        }
    }
}

/// A class with a `companion object` gets its `<clinit>` LAST among the methods (kotlinc's
/// order): the `Companion` instance store, then each non-const owner static's initializer (a
/// hoisted companion property, or a companion `const val` whose initializer isn't a compile-time
/// literal — the `ConstantValue` path covers only folded consts). One shared `<clinit>`.
/// A singleton object's storage is [`object_static_initialization`]'s, not this.
pub(super) fn emit_class_static_initializer(
    ir: &IrFile,
    c: &IrClass,
    facade: &str,
    env: &EmitEnv,
    fq_name: &str,
    byte_parity: bool,
    cw: &mut ClassWriter,
) {
    let clinit_statics: Vec<(u32, &crate::ir::IrStatic)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|s| {
            s.1.owner_matches(fq_name) && !(s.1.is_const && const_value_idx_peek(ir, s.1.init))
        })
        .map(|(index, s)| (index as u32, s))
        .collect();
    if c.companion_class.is_some() || !clinit_statics.is_empty() {
        // kotlinc visits `<clinit>` (name + descriptor) before its body's companion
        // construction and hoisted-initializer constants.
        cw.reserve_method_name("<clinit>");
        cw.seed_utf8("()V");
        let mut e = Emitter::new(
            ir,
            cw,
            env,
            fq_name,
            facade,
            Ty::Unit,
            clinit_statics.iter().map(|(_, property)| property.init),
        );
        let mut clinit = CodeBuilder::new(0);
        emit_companion_init(e.cw, &mut clinit, fq_name, c);
        // kotlinc's `<clinit>` LineNumberTable: one entry per hoisted-property store, at the
        // store's pc, mapping to the property's declaration line in the COMPANION source. The
        // `Companion` construction itself has no entry.
        let mut clinit_lines: Vec<(u16, u32)> = Vec::new();
        for (static_index, s) in &clinit_statics {
            let pc = clinit.bytes.len() as u16;
            // A hoisted companion property's line lives on the companion. A static a compiler
            // plugin generated has no property to look up and carries its own.
            let line = if ir.is_jvm_companion_hoisted_static(*static_index) {
                c.companion_class
                    .as_ref()
                    .and_then(|companion| ir.prop_decl_lines.get(&(*companion, s.name.clone())))
                    .copied()
                    .unwrap_or(0)
            } else {
                s.line
            };
            if line != 0 {
                clinit_lines.push((pc, line));
            }
            e.emit_static_initializer_store(fq_name, *static_index, &mut clinit);
        }
        clinit.ret_void();
        clinit.ensure_locals(e.frame.max());
        clinit.link();
        e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
        if byte_parity && !clinit_lines.is_empty() {
            e.cw.set_method_lines("<clinit>", "()V", &clinit_lines);
        }
    }
}

impl Emitter<'_> {
    /// Read static `index` from wherever its storage lives, as seen from the class being emitted.
    pub(super) fn emit_get_static(&mut self, i: u32, code: &mut CodeBuilder) {
        let s = &self.ir.statics[i as usize];
        let jt = jvm_declared_ty(&s.ty);
        let name = s.name.clone();
        let is_const = s.is_const;
        let facade = self.facade.clone();
        // A static declaring an OWNER lives on that class, not the facade. Within the owner
        // read the (private) field directly; from any other class — the companion's
        // delegating accessors — go through the owner's PUBLIC synthetic `access$get<X>$cp`
        // bridge, kotlinc's hoisted-companion-property access shape. A `@JvmField` static
        // is a PUBLIC field with no bridges: every reader goes `getstatic` directly.
        if let Some(owner) = self.ir.statics[i as usize].owner {
            let owner_name = owner.render();
            // A `companion { … }` block property is read like a top-level one, with its
            // class in the facade's place: another class calls its public getter.
            if self.owner != owner_name && companion_blocks::accessor_owned(self.ir, i) {
                let m = self.cw.methodref(
                    &owner_name,
                    &property_getter_name(&name),
                    &format!("(){}", type_descriptor(jt)),
                );
                code.invokestatic(m, 0, slot_words(jt) as i32);
            } else if self.owner == owner_name
                || !self.ir.is_jvm_companion_hoisted_static(i)
                || self.ir.is_jvm_field_static(i)
            {
                let fref = self.cw.fieldref(
                    &owner_name,
                    self.ir.static_field_jvm_name(i),
                    &type_descriptor(jt),
                );
                code.getstatic(fref, slot_words(jt) as i32);
            } else {
                let m = self.cw.methodref(
                    &owner_name,
                    &format!("access${}$cp", property_getter_name(&name)),
                    &format!("(){}", type_descriptor(jt)),
                );
                code.invokestatic(m, 0, slot_words(jt) as i32);
            }
        }
        // Within the facade (or a `const val`, which is public) read the field directly; from
        // another class a plain top-level property is private, so go through `getX()` — kotlinc's
        // cross-file property-access compilation.
        else if self.owner == facade || is_const {
            let fref = self.cw.fieldref(&facade, &name, &type_descriptor(jt));
            code.getstatic(fref, slot_words(jt) as i32);
        } else {
            // A PRIVATE top-level property has no public getter; cross-class reads inside the
            // file go through kotlinc's `access$get<X>$p` bridge.
            let gname = if self.ir.statics[i as usize].visibility.is_private() {
                format!("access${}$p", property_getter_name(&name))
            } else {
                property_getter_name(&name)
            };
            let m = self
                .cw
                .methodref(&facade, &gname, &format!("(){}", type_descriptor(jt)));
            code.invokestatic(m, 0, slot_words(jt) as i32);
        }
    }

    /// Store `value` into static `index`, through the storage boundary visible from this class.
    pub(super) fn emit_set_static(
        &mut self,
        index: u32,
        value: crate::ir::ExprId,
        code: &mut CodeBuilder,
    ) {
        let s = &self.ir.statics[index as usize];
        let jt = jvm_declared_ty(&s.ty);
        let name = s.name.clone();
        let is_const = s.is_const;
        let facade = self.facade.clone();
        self.emit_value(value, code);
        if self.diverges(value) {
            return;
        }
        // Within the facade write the field directly; from another class go through `setX()` —
        // or, for a PRIVATE top-level property (no public setter), the `access$set<X>$p` bridge.
        let private = self.ir.statics[index as usize].visibility.is_private();
        // A static declaring an OWNER lives on that class (a companion property is a static
        // field on the outer class). Within the owner write the (private) field directly;
        // from another class — the companion's delegating setter — go through the owner's
        // PUBLIC synthetic `access$set<X>$cp` bridge (kotlinc's hoisted-companion shape).
        // A `@JvmField` static is a PUBLIC field with no bridges: every writer goes
        // `putstatic` directly.
        if let Some(owner) = self.ir.statics[index as usize].owner {
            let owner_name = owner.render();
            if self.owner != owner_name && companion_blocks::accessor_owned(self.ir, index) {
                let setter = self.ir.statics[index as usize]
                    .setter_jvm_name
                    .clone()
                    .unwrap_or_else(|| property_setter_name(&name));
                let m =
                    self.cw
                        .methodref(&owner_name, &setter, &format!("({})V", type_descriptor(jt)));
                code.invokestatic(m, slot_words(jt) as i32, 0);
            } else if self.owner == owner_name
                || !self.ir.is_jvm_companion_hoisted_static(index)
                || self.ir.is_jvm_field_static(index)
            {
                let fref = self.cw.fieldref(
                    &owner_name,
                    self.ir.static_field_jvm_name(index),
                    &type_descriptor(jt),
                );
                code.putstatic(fref, slot_words(jt) as i32);
            } else {
                let m = self.cw.methodref(
                    &owner_name,
                    &format!("access${}$cp", property_setter_name(&name)),
                    &format!("({})V", type_descriptor(jt)),
                );
                code.invokestatic(m, slot_words(jt) as i32, 0);
            }
        } else if self.owner == facade || is_const {
            let fref = self.cw.fieldref(&facade, &name, &type_descriptor(jt));
            code.putstatic(fref, slot_words(jt) as i32);
        } else {
            let sname = if private {
                format!("access${}$p", property_setter_name(&name))
            } else {
                property_setter_name(&name)
            };
            let m = self
                .cw
                .methodref(&facade, &sname, &format!("({})V", type_descriptor(jt)));
            code.invokestatic(m, slot_words(jt) as i32, 0);
        }
    }

    /// Emit one checked static initializer through its declared JVM storage boundary. Facade,
    /// class/object, interface, and enum `<clinit>` paths share this operation so boxing and carrier
    /// selection cannot diverge by owner shape.
    pub(super) fn emit_static_initializer_store(
        &mut self,
        owner: &str,
        static_index: u32,
        code: &mut CodeBuilder,
    ) {
        let field = &self.ir.statics[static_index as usize];
        self.emit_value(field.init, code);
        if self.diverges(field.init) {
            return;
        }
        let physical = jvm_declared_ty(&field.ty);
        self.adapt_physical_operand_for(field.init, self.value_ty(field.init), physical, code);
        // Static storage is identified by its place in the file's static table.
        let reference = self.cw.fieldref(
            owner,
            self.ir.static_field_jvm_name(static_index),
            &type_descriptor(physical),
        );
        code.putstatic(reference, slot_words(physical) as i32);
    }
}

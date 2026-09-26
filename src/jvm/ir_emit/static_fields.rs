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
        !(crate::jvm::fresh_storage::holds_fresh_value(ir, s.ty, s.init)
            || s.is_const && const_value_idx_peek(ir, s.init))
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
    for &(_, s) in &facade_statics {
        if !should_store(s) {
            continue;
        }
        if s.line != 0 {
            clinit_lines.push((code.bytes.len() as u16, s.line));
        }
        e.emit_static_initializer_store(facade, s, &mut code);
    }
    code.ret_void();
    finish_code::<0x0008>(e.cw, "<clinit>", "()V", &mut code, e.frame.max());
    if !clinit_lines.is_empty() {
        e.cw.set_method_lines("<clinit>", "()V", &clinit_lines);
    }
}

/// The public `getX`/`setX` of one plain facade property, emitted at the property's place among the
/// facade's declared functions: kotlinc's JvmPropertiesLowering replaces each property with its
/// accessors in place, and the facade's methods follow that declaration order. A `const val`, a
/// `@JvmField`, a custom-accessor property (its accessors are ordinary facade functions) and a private
/// property (reached only through `access$…$p` bridges) publish none here.
pub(super) fn emit_static_accessors(
    ir: &IrFile,
    facade: &str,
    cw: &mut ClassWriter,
    env: &EmitEnv,
    param_assertions: bool,
    static_index: u32,
) {
    let s = &ir.statics[static_index as usize];
    if !s.is_facade_owned()
        || s.is_const
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
    let nullability = field_nullability_kind(ir, facade, &s.name, accessor_ty);
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
    let fref = cw.fieldref(facade, &s.name, &desc);
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
        let fref = cw.fieldref(facade, &s.name, &desc);
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

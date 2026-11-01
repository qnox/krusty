//! The facade's and each class's STATIC fields, and the `<clinit>` that initializes them.
//!
//! A `const val` whose initializer is a compile-time literal carries a `ConstantValue` attribute and
//! is not assigned at run time at all; everything else is stored by `<clinit>` in declaration order.
//! Field flags are decided here too, and they are not uniform: a `const val`'s field takes its
//! DECLARATION's visibility, while a plain `val`/`var` is private whatever the source said, because
//! every reader outside the facade goes through the generated accessor. A `lateinit var` is the
//! exception: its field follows the setter's visibility, and the default getter throws while that
//! field is null.

use super::field_visibility::annotated_field_modifiers;
use super::*;

/// The visibility of a hoisted `lateinit` property's one physical outer field. Kotlin exposes
/// reads through the getter and writes through the setter, but the field itself follows the
/// setter; every bytecode route must therefore make its bridge decision from this single fact.
pub(super) fn hoisted_lateinit_field_visibility(
    ir: &IrFile,
    index: u32,
) -> Option<crate::types::Visibility> {
    ir.statics[index as usize]
        .is_lateinit
        .then(|| {
            ir.jvm_companion_static_declaration(index)
                .map(|(_, property)| property.setter_visibility)
        })
        .flatten()
}

fn static_visibility_access(visibility: crate::types::Visibility) -> u16 {
    match visibility {
        crate::types::Visibility::Private => 0x0002,
        crate::types::Visibility::Protected => 0x0004,
        _ => 0x0001,
    }
}

/// The `ConstantValue` payload of a compile-time literal, excluding `null` (JVMS 4.7.2 has no
/// constant for it). The field-table visit interns it; callers must not intern it earlier.
pub(super) fn constant_value(ir: &IrFile, init: crate::ir::ExprId) -> Option<crate::ir::IrConst> {
    match ir.expr(init) {
        crate::ir::IrExpr::Const(value) if !matches!(value, crate::ir::IrConst::Null) => {
            Some(value.clone())
        }
        _ => None,
    }
}

/// Whether `init` is a `ConstantValue`-eligible literal. The field-table visit interns the payload;
/// this only decides that `<clinit>` must not store the same value.
pub(super) fn const_value_idx_peek(ir: &IrFile, init: crate::ir::ExprId) -> bool {
    matches!(
        ir.expr(init),
        crate::ir::IrExpr::Const(value) if !matches!(value, crate::ir::IrConst::Null)
    )
}

/// The initializer `<clinit>` runs for static `s`: its source initializer, unless a `const val`
/// folds it into a `ConstantValue`. `None` for a static with no initializer (`lateinit var`).
pub(super) fn clinit_initializer(
    ir: &IrFile,
    s: &crate::ir::IrStatic,
) -> Option<crate::ir::ExprId> {
    s.init
        .filter(|&init| !(s.is_const && const_value_idx_peek(ir, init)))
}

/// The store `<clinit>` emits for static `s`: [`clinit_initializer`], unless that initializer is
/// the field's JVM default (`var count: Int = 0`), which kotlinc's `visitSetField` drops from a
/// class initializer as the same elision instance fields get from `elide_default_property_stores`.
pub(super) fn stored_clinit_initializer(
    ir: &IrFile,
    s: &crate::ir::IrStatic,
) -> Option<crate::ir::ExprId> {
    clinit_initializer(ir, s)
        .filter(|&init| !crate::jvm::fresh_storage::holds_fresh_value(ir, s.ty, init))
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
    if facade_statics.is_empty() && !delegated_property_array::exists(env, env.facade_class) {
        return;
    }
    delegated_property_array::declare(env, env.facade_class, cw);
    let mut facade_constants = Vec::new();
    for &(static_index, s) in &facade_statics {
        // A delegate evaluated in place has no storage.
        if ir.is_retired_jvm_delegate_static(static_index) {
            continue;
        }
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
            // A public or internal `lateinit var` with a setter method is `public static`; every
            // plain `var` stays private.
            if crate::jvm::private_static_access::lateinit_field_is_public(ir, static_index) {
                0x0009 // PUBLIC | STATIC
            } else {
                0x000A // PRIVATE | STATIC
            }
        } else {
            0x001A // PRIVATE | STATIC | FINAL
        } | annotated_field_modifiers(ir.static_declaration_annotations(static_index));
        let desc = ir_type_desc(&s.ty);
        // A PARAMETERIZED type (`val xs: List<String>`) carries its full generic `Signature`, exactly
        // as the same property declared inside a class does — the facade's field table is a different
        // emitter, and without this a top-level property's element type was lost to erasure.
        let signature = signature_formatter.field_signature(&s.ty, None, s.is_var);
        // A reference-typed facade static carries kotlinc's nullability annotation like any other
        // backing field, except a `lateinit` one, which holds null until assigned.
        let field_ann = (!s.is_lateinit)
            .then(|| super::declared_static_annotation(s))
            .flatten();
        // A `const val` initialized by a compile-time literal carries a `ConstantValue` attribute (the
        // JVM initializes the field; its `<clinit>` store is omitted below) — byte-identical to kotlinc.
        // LATE adds: kotlinc visits the facade's fields AFTER its methods, so a backing field's name
        // first interns at its accessor body and the const payload lands after the `<clinit>` window.
        let cv = s
            .init
            .filter(|_| s.is_const)
            .and_then(|init| constant_value(ir, init));
        // A multifile part's visible `const val` is a field of its facade alone.
        if env.multifile_facade.is_some() && s.is_const && !s.visibility.is_private() {
            facade_constants.push(super::multifile_facades::FacadeConstant {
                access: acc,
                name: ir.static_field_jvm_name(static_index).to_string(),
                desc,
                signature: signature.filter(|_| !s.property_delegate),
                value: cv,
                annotation: field_ann.map(str::to_string),
            });
            continue;
        }
        cw.add_field_late_sig(
            acc,
            ir.static_field_jvm_name(static_index),
            &desc,
            signature.as_deref().filter(|_| !s.property_delegate),
            cv,
            field_ann,
        );
        if let Some(annotations) = ir.package_static_field_annotations(static_index) {
            cw.set_last_late_field_annotations(annotations);
        }
    }
    env.run.multifile_part.record_constants(facade_constants);
    // kotlinc visits `<clinit>` (name + descriptor) before the initializer constants its body
    // interns. Every initializer that is not folded into a `ConstantValue` gives the facade a
    // `<clinit>`, even when its store is elided above (`var x = 0` leaves a lone `return`); a
    // file of only `const val`s and `lateinit var`s has none.
    if !facade_statics
        .iter()
        .any(|(_, property)| clinit_initializer(ir, property).is_some())
        && !delegated_property_array::exists(env, env.facade_class)
    {
        return;
    }
    cw.reserve_method_name("<clinit>");
    cw.seed_utf8("()V");
    let mut e = Emitter::new(
        ir,
        cw,
        env,
        Some(StaticOwner::Facade),
        facade,
        facade,
        Ty::Unit,
        facade_statics
            .iter()
            .filter_map(|(_, property)| property.init),
    );
    e.enter_class_initializer(env.signature_symbols);
    let mut code = CodeBuilder::new(0);
    e.emit_delegated_property_array(env, env.facade_class, facade, &mut code);
    if delegated_property_array::exists(env, env.facade_class) {
        e.cw.omit_method_lines("<clinit>", "()V");
    }
    // Each store opens on its initializer's line, and the lines the initializer marks inside
    // itself (an argument on a line of its own at the same offset, an inlined body) stay.
    // `add_method` drops a `<clinit>`'s marks, so collect them and set them after.
    let first_mark = code.line_marks().len();
    for &(index, s) in &facade_statics {
        // A singleton delegate evaluated in place is still read where its storage was initialized;
        // a constant one kept no initializer.
        if ir.is_retired_jvm_delegate_static(index) {
            if let Some(init) = s.init {
                e.emit_discarding(init, &mut code);
            }
            continue;
        }
        let Some(init) = stored_clinit_initializer(ir, s) else {
            continue;
        };
        code.mark_line_retained(initializer_line(ir, s, init));
        e.emit_static_initializer_store(facade, index, init, &mut code);
    }
    code.ret_void();
    let mut clinit_lines: Vec<(u16, u32)> = code.line_marks()[first_mark..]
        .iter()
        .map(|&(pc, line)| (pc, u32::from(line)))
        .collect();
    // The store's own mark is retained so an argument line at the same offset follows it; the
    // initializer marking that same line again there adds nothing.
    clinit_lines.dedup();
    finish_code::<0x0008>(e.cw, "<clinit>", "()V", &mut code, e.frame.max());
    if !clinit_lines.is_empty() {
        e.cw.set_method_lines("<clinit>", "()V", &clinit_lines);
    }
}

/// The line a static's `<clinit>` store opens on: its initializer's own line, which kotlinc gives the
/// field store. An explicit backing field's `field = …` and a `val x =` whose initializer starts on
/// the next line both differ from the declaration line; generated storage has only the latter.
pub(super) fn initializer_line(ir: &IrFile, s: &crate::ir::IrStatic, init: ExprId) -> u32 {
    ir.expr_source_lines.get(&init).copied().unwrap_or(s.line)
}

/// The HOISTED outer-class static backing a companion property of `c` (a companion class), if any:
/// the companion has no field for it, so field-driven attribute passes need this lookup instead.
pub(super) fn hoisted_static_for<'a>(
    ir: &'a IrFile,
    c: &crate::ir::IrClass,
    property: usize,
) -> Option<&'a crate::ir::IrStatic> {
    hoisted_static_declaration_for(ir, c, property).map(|(_, storage)| storage)
}

/// The indexed HOISTED declaration. Metadata publication needs the index because physical field
/// naming is owned by the static-realization table rather than the semantic `IrStatic::name`.
pub(super) fn hoisted_static_declaration_for<'a>(
    ir: &'a IrFile,
    c: &crate::ir::IrClass,
    property: usize,
) -> Option<(u32, &'a crate::ir::IrStatic)> {
    if !c.is_companion {
        return None;
    }
    let static_id = ir.jvm_companion_property_static(c.fq_name_id(), property as u32)?;
    ir.statics
        .get(static_id as usize)
        .map(|storage| (static_id, storage))
}

/// The HOISTED outer-class static holding the `x$delegate` of delegated companion property
/// `property` of `c`, if any.
pub(super) fn hoisted_delegate_static_for(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    property: usize,
) -> Option<u32> {
    if !c.is_companion {
        return None;
    }
    ir.jvm_companion_delegate_static(c.fq_name_id(), property as u32)
}

/// Whether this companion property's hoisted static is the `@JvmField` realization — a public owner
/// field with NO companion accessors, so accessor-shaped emission must skip it entirely.
pub(super) fn jvm_field_static_for(ir: &IrFile, c: &crate::ir::IrClass, property: usize) -> bool {
    c.is_companion
        && ir
            .jvm_companion_property_static(c.fq_name_id(), property as u32)
            .is_some_and(|static_id| ir.is_jvm_field_static(static_id))
}

/// One of a static property's two accessors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StaticAccessor {
    Getter,
    Setter,
}

/// The exact compiler-default accessor one static property claims. Collision planning calls the
/// same operation as emission so a diagnostic cannot drift from the method the class writer will
/// publish after value-class storage erasure or setter-name mangling.
pub(in crate::jvm) fn declared_default_static_accessor_signature(
    ir: &IrFile,
    static_index: u32,
    side: PropertyAccessorSide,
) -> Option<super::declared_property_accessor::EmittedAccessorSignature> {
    let present = match side {
        PropertyAccessorSide::Getter => ir.has_jvm_default_static_getter(static_index),
        PropertyAccessorSide::Setter => ir.has_jvm_default_static_setter(static_index),
    };
    if !present {
        return None;
    }
    let storage = &ir.statics[static_index as usize];
    let descriptor = type_descriptor(jvm_declared_ty(&storage.ty));
    let (name, descriptor) = match side {
        PropertyAccessorSide::Getter => {
            (storage.default_getter_jvm_name(), format!("(){descriptor}"))
        }
        PropertyAccessorSide::Setter => (
            storage.default_setter_jvm_name(),
            format!("({descriptor})V"),
        ),
    };
    Some(super::declared_property_accessor::EmittedAccessorSignature { name, descriptor })
}

/// The compiler-default public `getX` or `setX` of one static property stored on `owner`, emitted
/// at the property's place among its owner's declared members: kotlinc's JvmPropertiesLowering
/// replaces each property with its accessors in place. The owner is the file facade for a top-level
/// property and the declaring class for a `companion { … }` block property. A `const val`, a
/// `@JvmField` and a private property (reached only through `access$…$p` bridges or its field)
/// publish none, and an accessor source declared is an ordinary function; the other one is still
/// the default.
pub(super) fn emit_default_static_accessor(
    ir: &IrFile,
    owner: &str,
    cw: &mut ClassWriter,
    env: &EmitEnv,
    param_assertions: bool,
    static_index: u32,
    accessor: StaticAccessor,
) {
    let side = match accessor {
        StaticAccessor::Getter => PropertyAccessorSide::Getter,
        StaticAccessor::Setter => PropertyAccessorSide::Setter,
    };
    let Some(accessor_signature) =
        declared_default_static_accessor_signature(ir, static_index, side)
    else {
        return;
    };
    let s = &ir.statics[static_index as usize];
    let lateinit_end = (s.is_lateinit && s.line != 0 && s.accessor_lines.declaration_end > s.line)
        .then_some(s.accessor_lines.declaration_end);
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
    let nullability = field_nullability_kind(accessor_ty);
    let acc_ann = super::nullability_annotation(nullability);
    // The accessors erase the property's type arguments in their descriptors, so each carries the
    // same generic `Signature` its backing field does — kotlinc signs `getXs()` as
    // `()Ljava/util/List<Ljava/lang/String;>;` and `setXs(List)` as `(Ljava/util/List<…>;)V`.
    let signatures = property_jvm_signatures(&signature_formatter, &s.ty, None);
    let declared = ir.static_accessor_annotations(static_index);
    if accessor == StaticAccessor::Setter {
        emit_default_static_setter(
            cw,
            owner,
            s,
            ir.static_field_jvm_name(static_index),
            DefaultSetterHeader {
                name: &accessor_signature.name,
                descriptor: &accessor_signature.descriptor,
                signature: signatures.setter.as_deref(),
                nullability: acc_ann,
                annotations: declared.map(|declared| &declared.setter),
                parameter_annotations: declared.map(|declared| &declared.setter_parameter),
            },
            // The guard checks the stored value: a non-null `N` over a `String?` carrier admits
            // null there, so kotlinc emits none.
            param_assertions && nullability == 1 && field_nullability_kind(s.ty) == 1,
        );
        return;
    }
    let field_name = ir.static_field_jvm_name(static_index);
    // kotlinc interns an accessor's `Signature` between its descriptor and its annotations (the
    // declared ones, then nullability), BEFORE the body's field cluster.
    let annotations = declared.map(|declared| &declared.getter);
    reserve_default_accessor_pool(
        cw,
        &accessor_signature.name,
        &accessor_signature.descriptor,
        signatures.getter.as_deref(),
        acc_ann,
        annotations,
    );
    let mut g = CodeBuilder::new(0);
    let getter_line = if s.accessor_lines.getter != 0 {
        s.accessor_lines.getter
    } else {
        s.line
    };
    if getter_line != 0 {
        g.mark_line(getter_line);
    }
    let fref = cw.fieldref(owner, field_name, &desc);
    g.getstatic(fref, slot_words(jt) as i32);
    // kotlinc emits no guard in the getter of a property another class probes with
    // `isInitialized`: that getter returns the raw field.
    let guarded = s.is_lateinit
        && !env
            .run
            .static_accessor_plan
            .borrow()
            .probed_from_another_class(static_index);
    if guarded {
        // kotlinc's getter: return the value, or throw and then `aconst_null; areturn` so the
        // null path has a reference for the verifier.
        g.dup();
        let missing = g.new_label();
        g.ifnull(missing);
        emit_return(jt, &mut g);
        g.bind(missing);
        g.pop();
        g.push_string(&s.name, cw);
        let throw = cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "throwUninitializedPropertyAccessException",
            "(Ljava/lang/String;)V",
        );
        g.invokestatic(throw, 1, 0);
        if let Some(end) = lateinit_end {
            g.mark_line(end);
        }
        g.aconst_null();
    }
    emit_return(jt, &mut g);
    g.link();
    cw.add_method_sig(
        0x0019 | super::method_access::annotated_method_modifiers(annotations),
        &accessor_signature.name,
        &accessor_signature.descriptor,
        &g,
        signatures.getter.as_deref(),
    );
    cw.set_method_nullability(
        &accessor_signature.name,
        &accessor_signature.descriptor,
        acc_ann,
        &[None],
    );
    if let Some(annotations) = annotations {
        super::function_annotations::emit_declared(
            cw,
            annotations,
            &accessor_signature.name,
            &accessor_signature.descriptor,
        );
    }
}

/// Intern a default accessor's header as kotlinc visits it: name, descriptor, `Signature`, the
/// declared annotations, then the nullability annotation.
fn reserve_default_accessor_pool(
    cw: &mut ClassWriter,
    name: &str,
    descriptor: &str,
    signature: Option<&str>,
    nullability: Option<&str>,
    annotations: Option<&crate::ir::DeclarationAnnotations>,
) {
    cw.reserve_method_name(name);
    cw.reserve_method_pool_with_annotations(
        name,
        descriptor,
        signature,
        &nullability.into_iter().collect::<Vec<_>>(),
        annotations.unwrap_or(&crate::ir::DeclarationAnnotations::default()),
        &[],
    );
}

/// What a default static setter's header carries besides its name and descriptor.
struct DefaultSetterHeader<'a> {
    name: &'a str,
    descriptor: &'a str,
    signature: Option<&'a str>,
    nullability: Option<&'a str>,
    annotations: Option<&'a crate::ir::DeclarationAnnotations>,
    parameter_annotations: Option<&'a crate::ir::DeclarationAnnotations>,
}

/// A static property's compiler-default `setX`; see [`emit_default_static_accessor`].
fn emit_default_static_setter(
    cw: &mut ClassWriter,
    owner: &str,
    s: &crate::ir::IrStatic,
    field_name: &str,
    header: DefaultSetterHeader<'_>,
    checks_parameter: bool,
) {
    let DefaultSetterHeader {
        name,
        descriptor,
        signature,
        nullability: acc_ann,
        annotations,
        parameter_annotations,
    } = header;
    let jt = jvm_declared_ty(&s.ty);
    let field_descriptor = type_descriptor(jt);
    cw.reserve_method_name(name);
    cw.seed_utf8(descriptor);
    if let Some(signature) = signature {
        cw.seed_utf8(signature);
    }
    if let Some(annotations) = annotations {
        reserve_default_accessor_pool(cw, name, descriptor, signature, None, Some(annotations));
    }
    let unannotated = crate::ir::DeclarationAnnotations::default();
    let parameter_annotations = std::slice::from_ref(parameter_annotations.unwrap_or(&unannotated));
    cw.reserve_method_parameter_annotation_pool(None, parameter_annotations, &[acc_ann], true);
    let words = slot_words(jt);
    let mut st = CodeBuilder::new(words);
    // kotlinc guards a non-null reference setter parameter with checkNotNullParameter("<set-?>").
    // `-Xno-param-assertions` removes it, like every other parameter guard.
    if checks_parameter && jt.is_reference() {
        st.aload(0);
        st.push_string("<set-?>", cw);
        let m = cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "checkNotNullParameter",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        );
        st.invokestatic(m, 2, 0);
    }
    // A written setter starts at its annotation/modifier prefix; an implicit one starts at the
    // property. The store maps to that line at the POST-GUARD pc (kotlinc's shape).
    let setter_line = if s.accessor_lines.setter != 0 {
        s.accessor_lines.setter
    } else {
        s.line
    };
    if setter_line != 0 {
        st.mark_line(setter_line);
    }
    load(jt, 0, &mut st);
    let fref = cw.fieldref(owner, field_name, &field_descriptor);
    st.putstatic(fref, slot_words(jt) as i32);
    let setter_end = if s.is_lateinit && s.line != 0 && s.accessor_lines.declaration_end > s.line {
        s.accessor_lines.declaration_end
    } else {
        s.accessor_lines.setter_end
    };
    if setter_end != 0 {
        st.mark_line(setter_end);
    }
    st.ret_void();
    // kotlinc writes the setter's `LocalVariableTable` as it finishes the method, so the
    // `<set-?>` name joins the pool right after the body's constants.
    cw.seed_utf8("<set-?>");
    st.ensure_locals(words);
    st.link();
    cw.add_method_sig(
        0x0019 | super::method_access::annotated_method_modifiers(annotations),
        name,
        descriptor,
        &st,
        signature,
    );
    cw.set_method_nullability(name, descriptor, None, &[acc_ann]);
    cw.set_method_param_annotations(name, descriptor, parameter_annotations);
    if let Some(annotations) = annotations {
        super::function_annotations::emit_declared(cw, annotations, name, descriptor);
    }
    // The setter's value parameter is kotlinc's synthetic `<set-?>`, live for the body.
    cw.set_method_debug(
        name,
        descriptor,
        None,
        &[("<set-?>".to_string(), field_descriptor, 0)],
    );
}

/// Declare class-owned static storage in field-table order: a named object's backing fields and
/// its other statics interleaved in source order, as kotlinc visits the declarations. A `companion object`'s `const val`s live on the outer class as
/// `public static final` + `ConstantValue`; source lines and declaration order restore the ordering
/// that common lowering's separate shape passes cannot retain. This ordering is deliberately not
/// reused for `<clinit>`: kotlinc gives block initialization its own precedence.
pub(super) fn declare(
    ir: &IrFile,
    run: &EmitRun,
    c: &IrClass,
    fq_name: &str,
    signature_formatter: &JvmSignatureFormatter<'_>,
    cw: &mut ClassWriter,
) {
    let mut object_fields = object_static_initialization::declared_fields(ir, c)
        .into_iter()
        .peekable();
    let mut owner_statics: Vec<(u32, &crate::ir::IrStatic)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, property)| property.owner_matches(fq_name))
        // An interface companion emits its const fields beside its instance fields, in source
        // order. The interface keeps a separate copy of each public or internal const.
        .filter(|(_, property)| !(property.is_const && companion_of_interface(ir, c)))
        .map(|(index, property)| (index as u32, property))
        .collect();
    owner_statics.sort_by_key(|(index, property)| (property.line, property.source_order, *index));
    // On a named object, generated storage (a `$cachedSerializer$delegate`) follows every declared
    // field.
    let (generated, declared): (Vec<_>, Vec<_>) =
        owner_statics.into_iter().partition(|&(index, _)| {
            object_fields.peek().is_some() && ir.is_compiler_generated_static(index)
        });
    for (static_index, s) in declared {
        while let Some((_, field_index)) =
            object_fields.next_if(|&(position, _)| position < s.source_order)
        {
            object_static_initialization::add_late_member_field(
                ir,
                run,
                c,
                signature_formatter,
                fq_name,
                cw,
                field_index,
            );
        }
        declare_static(ir, c, signature_formatter, cw, static_index, s);
    }
    for (_, field_index) in object_fields {
        object_static_initialization::add_late_member_field(
            ir,
            run,
            c,
            signature_formatter,
            fq_name,
            cw,
            field_index,
        );
    }
    for (static_index, s) in generated {
        declare_static(ir, c, signature_formatter, cw, static_index, s);
    }
}

/// Declare one class-owned static for the field-table visit.
fn declare_static(
    ir: &IrFile,
    c: &IrClass,
    signature_formatter: &JvmSignatureFormatter<'_>,
    cw: &mut ClassWriter,
    static_index: u32,
    s: &crate::ir::IrStatic,
) {
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
    let lateinit_visibility = hoisted_lateinit_field_visibility(ir, static_index);
    let acc = if let Some(visibility) = lateinit_visibility {
        0x0008 | static_visibility_access(visibility) | final_flag
    } else if (s.visibility.is_private() || hoisted || block_storage) && !jvm_field {
        0x000A | final_flag // PRIVATE | STATIC [| FINAL]
    } else {
        0x0009 | final_flag // PUBLIC | STATIC [| FINAL]
    } | annotated_field_modifiers(ir.static_declaration_annotations(static_index));
    // `ConstantValue` is only meaningful on a FINAL field (JVMS 4.7.2 ignores it otherwise), and a
    // `var` is initialized by the `<clinit>` store anyway. A HOISTED companion property never
    // folds either — kotlinc initializes it in `<clinit>` (only `const val` gets the attribute).
    // LATE adds: kotlinc's field-table visit runs after the methods, so a hoisted static's name
    // first interns at its `access$…$cp` bridge and a folded const's name + `ConstantValue`
    // land after the `<clinit>` window.
    let fold = s.is_const && !s.is_var && !hoisted;
    let cv = s
        .init
        .filter(|_| fold)
        .and_then(|init| constant_value(ir, init));
    // Generated storage with no declaration of its own is ACC_SYNTHETIC and unannotated.
    let synthetic = ir.is_compiler_generated_static(static_index);
    let acc = if synthetic { acc | 0x1000 } else { acc };
    // Reference-typed statics, including private hoisted fields, carry nullability annotations,
    // except a `lateinit` one, which holds null until assigned.
    let ann = (!synthetic && !s.is_lateinit)
        .then(|| super::declared_static_annotation(s))
        .flatten();
    let signature = signature_formatter
        .field_signature(&s.ty, None, s.is_var)
        .filter(|_| !s.property_delegate);
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
    // They come from wherever the field's DECLARATION lives: every hoisted companion property
    // records them on the COMPANION, while a static this class owns outright — one a compiler
    // plugin generated, say — records them on the class itself. `@JvmField` changes the
    // property's JVM surface, not the ownership of its backing-field annotations.
    if let Some(annotations) = static_field_annotations(ir, c, static_index) {
        cw.set_last_late_field_annotations(annotations);
    }
}

fn static_field_annotations<'a>(
    ir: &'a IrFile,
    owner: &'a IrClass,
    static_index: u32,
) -> Option<&'a crate::ir::DeclarationAnnotations> {
    if let Some(property) = ir.companion_blocks.property_for_storage(static_index) {
        return property
            .field_annotations
            .declares_annotations()
            .then_some(&property.field_annotations);
    }
    if let Some((companion, property)) = ir.jvm_companion_static_declaration(static_index) {
        return companion
            .field_annotations
            .iter()
            .find(|annotations| annotations.field == property.name)
            .map(|annotations| &annotations.annotations)
            .or_else(|| {
                // Enum metadata currently records a companion field annotation on the physical
                // enum inventory. It can belong to this exact companion declaration only when the
                // enum declares no same-named property of its own.
                owner
                    .properties
                    .iter()
                    .all(|declaration| declaration.name != property.name)
                    .then(|| {
                        owner
                            .field_annotations
                            .iter()
                            .find(|annotations| annotations.field == property.name)
                            .map(|annotations| &annotations.annotations)
                    })
                    .flatten()
            });
    }
    let field = &ir.statics[static_index as usize].name;
    owner
        .field_annotations
        .iter()
        .find(|annotations| annotations.field == *field)
        .map(|annotations| &annotations.annotations)
}

/// An enum class's own statics, after its property fields and before its entry constants: a
/// `@Serializable enum`'s `$cachedSerializer$delegate` (`Companion, value,
/// $cachedSerializer$delegate, <entries>`), and its companion's hoisted properties.
pub(super) fn emit_enum_static_fields(
    ir: &IrFile,
    c: &IrClass,
    fq: &str,
    signature_formatter: &JvmSignatureFormatter<'_>,
    cw: &mut ClassWriter,
) {
    // Declaration order, as in the class path: a companion `const val` among hoisted properties.
    let mut owner_statics: Vec<_> = (ir.statics.iter().enumerate())
        .filter(|(_, s)| s.owner_matches(fq))
        .map(|(index, s)| (index as u32, s))
        .collect();
    owner_statics
        .sort_by_key(|(index, declaration)| (declaration.line, declaration.source_order, *index));
    for (index, s) in owner_statics {
        // A `var` is reassignable, so it must not carry ACC_FINAL (see the class path). A hoisted
        // companion property's field is private unless `@JvmField` makes it the property's surface.
        let final_flag = if s.is_var { 0x0000 } else { 0x0010 };
        let jvm_field = ir.is_jvm_field_static(index);
        let private = s.visibility.is_private() || ir.is_jvm_companion_hoisted_static(index);
        let acc = if let Some(visibility) = hoisted_lateinit_field_visibility(ir, index) {
            0x0008 | static_visibility_access(visibility) | final_flag
        } else if private && !jvm_field {
            0x000A | final_flag // PRIVATE | STATIC [| FINAL]
        } else {
            0x0009 | final_flag // PUBLIC | STATIC [| FINAL]
        } | annotated_field_modifiers(ir.static_declaration_annotations(index));
        // A PARAMETERIZED static keeps its generic `Signature` (the delegate is a
        // `Lazy<KSerializer<Object>>`), and a reference-typed one carries kotlinc's nullability
        // annotation — the same treatment the class and facade field tables give their statics.
        let signature = signature_formatter.field_signature(&s.ty, None, s.is_var);
        let ann = super::declared_static_annotation(s).filter(|_| !s.is_lateinit);
        // A folded `const val` is its `ConstantValue`; `<clinit>` does not store it.
        let constant = (s.init)
            .filter(|_| s.is_const && !s.is_var && !ir.is_jvm_companion_hoisted_static(index))
            .and_then(|init| constant_value(ir, init));
        cw.add_field_late_sig(
            acc,
            ir.static_field_jvm_name(index),
            &ir_type_desc(&s.ty),
            signature.as_deref().filter(|_| !s.property_delegate),
            constant,
            ann,
        );
        if let Some(annotations) = static_field_annotations(ir, c, index) {
            cw.set_last_late_field_annotations(annotations);
        }
    }
}

/// Whether the outer `<clinit>` must bind value 0 before running a moved companion initializer.
///
/// A hoisted companion property is a static field of this class. Emission does not evaluate a
/// bare local used only as that property's receiver, so storing the instance for it becomes
/// `getstatic`/`pop`. A member call, a capture, or any other use of the local does load it.
fn companion_initializer_loads_instance(ir: &IrFile, body: crate::ir::ExprId) -> bool {
    fn walk(ir: &IrFile, expr: crate::ir::ExprId) -> bool {
        if matches!(ir.expr(expr), crate::ir::IrExpr::GetValue(0)) {
            return true;
        }
        let dropped_receiver = match ir.expr(expr) {
            crate::ir::IrExpr::PropertyRead {
                receiver,
                owner,
                name,
                ..
            } => receiver.filter(|receiver| {
                crate::ir::expr_runs_no_code(ir, *receiver)
                    && hoisted_companion_field(ir, *owner, name, false)
            }),
            crate::ir::IrExpr::PropertyWrite {
                receiver,
                owner,
                name,
                ..
            } => receiver.filter(|receiver| {
                crate::ir::expr_runs_no_code(ir, *receiver)
                    && hoisted_companion_field(ir, *owner, name, true)
            }),
            _ => None,
        };
        if let crate::ir::IrExpr::Lambda { captures, .. } = ir.expr(expr) {
            // The lambda's `inline_body` numbers its own values; only its captures read ours.
            return captures.iter().any(|&capture| walk(ir, capture));
        }
        let mut found = false;
        crate::ir::for_each_child(&ir.exprs, expr, &mut |child| {
            if dropped_receiver == Some(child) {
                return;
            }
            if walk(ir, child) {
                found = true;
            }
        });
        found
    }
    walk(ir, body)
}

/// A companion property whose storage was moved onto the outer class and whose access from
/// `<clinit>` is the field itself, so its receiver is not an instance load.
///
/// A declared getter or setter is called instead. That call needs the companion instance.
fn hoisted_companion_field(
    ir: &IrFile,
    owner: crate::types::TypeName,
    name: &str,
    write: bool,
) -> bool {
    let Some(class) = ir.classes.iter().find(|class| class.fq_name == owner) else {
        return false;
    };
    let Some(index) = class
        .properties
        .iter()
        .position(|property| property.name == name)
    else {
        return false;
    };
    let property = &class.properties[index];
    let calls_accessor = if write {
        property.modifiers.declared_setter
    } else {
        property.modifiers.declared_getter
    };
    if calls_accessor {
        return false;
    }
    ir.jvm_companion_property_static(owner, index as u32)
        .is_some()
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
    let mut clinit_statics: Vec<(u32, &crate::ir::IrStatic, crate::ir::ExprId)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, s)| s.owner_matches(fq_name))
        .filter_map(|(index, s)| Some((index as u32, s, clinit_initializer(ir, s)?)))
        .collect();
    // Generated statics (no source position) initialize after the declared ones, whatever order
    // the passes created them in.
    clinit_statics.sort_by_key(|&(_, s, _)| s.source_order == u32::MAX);
    if c.companion_class.is_some()
        || !clinit_statics.is_empty()
        || delegated_property_array::exists(env, c.fq_name)
    {
        // kotlinc visits `<clinit>` (name + descriptor) before its body's companion
        // construction and hoisted-initializer constants.
        cw.reserve_method_name("<clinit>");
        cw.seed_utf8("()V");
        let companion_initializer = ir.companion_clinit_body(c.fq_name);
        let mut e = Emitter::new(
            ir,
            cw,
            env,
            Some(StaticOwner::Class(c.fq_name)),
            fq_name,
            facade,
            Ty::Unit,
            clinit_statics
                .iter()
                .map(|&(_, _, init)| init)
                .chain(companion_initializer),
        );
        e.enter_class_initializer(env.signature_symbols);
        let has_delegated_array = delegated_property_array::exists(env, c.fq_name);
        let canonical_delegated_array = has_delegated_array
            && env
                .property_reference_realizations
                .delegated_arrays
                .field_name(c.fq_name)
                == crate::jvm::property_references::delegated_arrays::FIELD;
        let mut clinit = CodeBuilder::new(0);
        e.emit_delegated_property_array(env, c.fq_name, fq_name, &mut clinit);
        if canonical_delegated_array {
            e.cw.omit_method_lines("<clinit>", "()V");
        }
        // PropertyReferenceLowering suppresses the whole class-initializer line table while its
        // generated array keeps the canonical spelling. If a public source field displaces that
        // array to `$N`, kotlinc instead retains the delegated property's element mark and the
        // following source initializer marks.
        let mut clinit_lines: Vec<(u16, u32)> = if has_delegated_array && !canonical_delegated_array
        {
            clinit
                .line_marks()
                .iter()
                .map(|&(pc, line)| (pc, u32::from(line)))
                .collect()
        } else {
            Vec::new()
        };
        emit_companion_init(e.cw, &mut clinit, fq_name, c);
        // Every hoisted-property store gets an entry at its pc, mapping to the property's
        // declaration line in the COMPANION source. The `Companion` construction itself has no
        // entry.
        // A generated static that ends the initializer returns on its declaration's last line.
        let mut closing_line = None;
        for &(static_index, s, _) in &clinit_statics {
            let Some(init) = stored_clinit_initializer(ir, s) else {
                continue;
            };
            let pc = clinit.bytes.len() as u16;
            // Every static carries its declaration's line: a hoisted companion property and its
            // `x$delegate` storage the companion declaration's, a plugin-generated static its own.
            let line = s.line;
            if line != 0 && clinit_lines.last().map(|&(_, last)| last) != Some(line) {
                clinit_lines.push((pc, line));
            }
            e.emit_static_initializer_store(fq_name, static_index, init, &mut clinit);
            closing_line = (s.source_order == u32::MAX)
                .then_some(s.accessor_lines.declaration_end)
                .filter(|&end| end != 0);
        }
        if let Some(body) = companion_initializer {
            e.emit_companion_clinit_body(c, fq_name, body, &mut clinit, &mut clinit_lines);
            closing_line = None;
        }
        if let Some(end) =
            closing_line.filter(|&end| clinit_lines.last().is_some_and(|&(_, last)| last != end))
        {
            clinit_lines.push((clinit.bytes.len() as u16, end));
        }
        clinit.ret_void();
        clinit.ensure_locals(e.frame.max());
        clinit.link();
        e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
        if byte_parity && !canonical_delegated_array && !clinit_lines.is_empty() {
            e.cw.set_method_lines("<clinit>", "()V", &clinit_lines);
        }
    }
}

impl<'a> Emitter<'a> {
    /// Enter the implicit callable body of a synthetic class initializer. Its expressions then
    /// own nested lexical scopes, as expressions inside a declared method body do.
    pub(super) fn enter_class_initializer(&mut self, symbols: &'a dyn BackendClassifierSource) {
        self.regeneration_site = Some(bytecode_inline_call::RegenerationSite::class_initializer(
            symbols,
        ));
        self.record_locals = true;
        self.block_depth = 1;
    }

    /// Emit a class companion's initializer body in the outer `<clinit>`, after the `Companion`
    /// instance store, recording its line marks in `clinit_lines`.
    pub(super) fn emit_companion_clinit_body(
        &mut self,
        c: &IrClass,
        fq_name: &str,
        body: crate::ir::ExprId,
        clinit: &mut CodeBuilder,
        clinit_lines: &mut Vec<(u16, u32)>,
    ) {
        let ir = self.ir;
        // The companion constructor no longer runs this body. Value 0 is the companion
        // instance, which `<clinit>` has already stored. Bind it only when emission loads
        // that value. A property receiver that is a bare local is not loaded once the
        // property is a static field, and storing it anyway becomes `getstatic`/`pop`.
        if companion_initializer_loads_instance(ir, body) {
            let companion = c
                .companion_class
                .expect("a companion initializer belongs to a class that declares one");
            let companion_name = companion.render();
            let descriptor = format!("L{companion_name};");
            let field = self.cw.fieldref(
                fq_name,
                crate::jvm::companion_storage::field_name(c.fq_name, companion),
                &descriptor,
            );
            clinit.getstatic(field, 1);
            let receiver_ty = Ty::obj_name(companion);
            let existing = self.slots.get(&0).map(|(slot, _)| *slot);
            let receiver = match existing {
                Some(slot) => slot,
                None => self
                    .frame
                    .enter(super::frame_map::FrameKey::Receiver, receiver_ty),
            };
            store(receiver_ty, receiver, clinit);
            self.slots.insert(0, (receiver, receiver_ty));
        }
        let first = clinit.line_marks().len();
        self.render_initializer_boundaries = true;
        self.emit(body, clinit);
        self.render_initializer_boundaries = false;
        let marks = clinit.line_marks()[first..]
            .iter()
            .map(|&(pc, line)| (pc, u32::from(line)));
        clinit_lines.extend(marks);
        clinit_lines.dedup();
    }
}

/// Whether `function` is the compiler-default companion getter synthesized over hoisted `static_`.
/// The identity is recorded on the property; no accessor name is reconstructed here.
fn is_hoisted_default_getter(ir: &IrFile, function: u32, static_: u32) -> bool {
    ir.classes.iter().any(|class| {
        class
            .properties
            .iter()
            .enumerate()
            .any(|(property, declaration)| {
                !declaration.modifiers.declared_getter
                    && declaration.getter == Some(function)
                    && ir.jvm_companion_property_static(class.fq_name, property as u32)
                        == Some(static_)
            })
    })
}

/// kotlinc gives a compiler-default `lateinit` getter an early-return shape: the initialized path
/// returns immediately, while the missing path leaves `null` for the getter's enclosing IR return.
fn emit_lateinit_getter_guard(
    name: &str,
    ty: Ty,
    null_tail_line: Option<u32>,
    code: &mut CodeBuilder,
    cw: &mut ClassWriter,
) {
    code.dup();
    let missing = code.new_label();
    code.ifnull(missing);
    emit_return(ty, code);
    code.bind(missing);
    code.pop();
    code.push_string(name, cw);
    let throw = cw.methodref(
        "kotlin/jvm/internal/Intrinsics",
        "throwUninitializedPropertyAccessException",
        "(Ljava/lang/String;)V",
    );
    code.invokestatic(throw, 1, 0);
    if let Some(line) = null_tail_line {
        code.mark_line(line);
    }
    code.aconst_null();
}

impl Emitter<'_> {
    /// Straight-line expressions, except a raw `lateinit` read that inlines its guard.
    pub(super) fn static_expr_branches(&self, expr: u32, node: &crate::ir::IrExpr) -> bool {
        let crate::ir::IrExpr::GetStatic(index) = node else {
            return false;
        };
        self.static_lateinit_read_branches(*index, expr)
    }

    /// Whether this `GetStatic` inlines the uninitialized guard. A public getter hides the branch;
    /// a private read, including the one after `access$get<X>$p`, does not. An initialization probe
    /// loads the field and does not branch here.
    pub(super) fn static_lateinit_read_branches(&self, index: u32, expr: u32) -> bool {
        let declaration = &self.ir.statics[index as usize];
        if !declaration.is_lateinit || self.ir.lateinit_initialization_probes.contains(&expr) {
            return false;
        }
        let calls_owner_getter = declaration.owner.is_some_and(|owner| {
            self.static_owner != Some(StaticOwner::Class(owner))
                && companion_blocks::getter_owned(self.ir, index)
        });
        !calls_owner_getter
            && (declaration.owner.is_some() || !self.ir.has_jvm_default_static_getter(index))
    }

    /// Read static `index` from wherever its storage lives, as seen from the class being emitted.
    pub(super) fn emit_get_static(&mut self, i: u32, expr: u32, code: &mut CodeBuilder) {
        let s = &self.ir.statics[i as usize];
        let jt = jvm_declared_ty(&s.ty);
        let name = s.name.clone();
        let is_const = s.is_const;
        let is_lateinit = s.is_lateinit;
        let is_private = s.visibility.is_private();
        let facade = self.facade.clone();
        let probe = self.ir.lateinit_initialization_probes.contains(&expr);
        let calls_getter = is_lateinit && !probe && self.ir.has_jvm_default_static_getter(i);
        // A PRIVATE property's field, or the backing field of a source-declared getter, read from
        // another class, goes through its owner's accessor. Calling the declared getter would
        // re-enter it: the read is inside that getter.
        if let Some(storage) =
            crate::jvm::private_static_access::bridged_getter_read(self.ir, i, expr)
                .or_else(|| crate::jvm::private_static_access::declared_backing_getter(self.ir, i))
                .filter(|storage| {
                    self.static_owner != Some(storage.owner) || self.export_private_calls
                })
        {
            let m = static_accessors::static_methodref(
                self.cw,
                self.ir,
                &facade,
                storage.owner,
                &format!("access${}$p", property_getter_name(&name)),
                &format!("(){}", type_descriptor(jt)),
            );
            code.invokestatic(m, 0, slot_words(jt) as i32);
            // `access$get<X>$p` is a raw `getstatic`. The use site throws; `isInitialized` does not.
            if is_lateinit && !probe {
                self.emit_lateinit_use_guard(&name, code);
            }
            return;
        }
        // A static declaring an OWNER lives on that class, not the facade. Within the owner
        // read the (private) field directly; from any other class — the companion's
        // delegating accessors — go through the owner's PUBLIC synthetic `access$get<X>$cp`
        // bridge, kotlinc's hoisted-companion-property access shape. A `@JvmField` static
        // is a PUBLIC field with no bridges: every reader goes `getstatic` directly.
        if let Some(owner) = self.ir.statics[i as usize].owner {
            let owner_name = owner.render();
            let emitted_by_owner = self.static_owner == Some(StaticOwner::Class(owner));
            // A `companion { … }` block property is read like a top-level one, with its
            // class in the facade's place: another class calls its public getter.
            if !emitted_by_owner && companion_blocks::getter_owned(self.ir, i) {
                let m = self.cw.methodref(
                    &owner_name,
                    &self.ir.statics[i as usize].default_getter_jvm_name(),
                    &format!("(){}", type_descriptor(jt)),
                );
                code.invokestatic(m, 0, slot_words(jt) as i32);
                return;
            } else if emitted_by_owner
                || !self.ir.is_jvm_companion_hoisted_static(i)
                || self.ir.is_jvm_field_static(i)
                || hoisted_lateinit_field_visibility(self.ir, i).is_some_and(|visibility| {
                    matches!(
                        visibility,
                        crate::types::Visibility::Public
                            | crate::types::Visibility::Internal
                            | crate::types::Visibility::Protected
                    )
                })
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
            // Owner bridges and direct owner reads both expose raw storage. A normal `lateinit`
            // use guards that value here; only the initialization-probe identity suppresses it.
            if is_lateinit && !probe {
                if self
                    .function
                    .is_some_and(|function| is_hoisted_default_getter(self.ir, function, i))
                {
                    let null_tail_line = self
                        .ir
                        .jvm_companion_static_declaration(i)
                        .map(|(_, property)| property)
                        // A visibility-only setter keeps the default implementation, but its
                        // written header extends the property declaration: the getter's null tail
                        // sits on that declaration's last line. The source header plus the
                        // declaration-owned body flag identify that exact shape.
                        .filter(|property| {
                            property.accessor_lines.setter != 0
                                && !property.modifiers.declared_setter
                                && property.accessor_lines.setter != property.decl_line
                        })
                        .map(|property| property.accessor_lines.declaration_end);
                    emit_lateinit_getter_guard(&name, jt, null_tail_line, code, self.cw);
                } else {
                    self.emit_lateinit_use_guard(&name, code);
                }
            }
        }
        // Within the facade (or a `const val`, which is public) read the field directly; from
        // another class a plain top-level property is private, so go through `getX()` — kotlinc's
        // cross-file property-access compilation. A public `lateinit` calls `getX()` even from the
        // facade, so the throw lives in the getter. `isInitialized` still `getstatic`s the public
        // field. A private `lateinit` inlines the guard on the raw read.
        else if (self.static_owner == Some(StaticOwner::Facade)
            || is_const
            || (probe && !is_private))
            && !calls_getter
        {
            let fref = self.cw.fieldref(
                &facade,
                self.ir.static_field_jvm_name(i),
                &type_descriptor(jt),
            );
            code.getstatic(fref, slot_words(jt) as i32);
            if is_lateinit && !probe {
                self.emit_lateinit_use_guard(&name, code);
            }
        } else {
            let m = self.cw.methodref(
                &facade,
                &self.ir.statics[i as usize].default_getter_jvm_name(),
                &format!("(){}", type_descriptor(jt)),
            );
            code.invokestatic(m, 0, slot_words(jt) as i32);
        }
    }

    /// Store `value` into static `index`, through the storage boundary visible from this class.
    pub(super) fn emit_set_static(
        &mut self,
        statement: crate::ir::ExprId,
        index: u32,
        value: crate::ir::ExprId,
        code: &mut CodeBuilder,
    ) {
        let s = &self.ir.statics[index as usize];
        let initializer_line = self
            .ir
            .property_initializer_stores
            .contains(&statement)
            .then(|| self.ir.expr_lines.get(&statement).copied())
            .flatten();
        let jt = jvm_declared_ty(&s.ty);
        let name = s.name.clone();
        let is_const = s.is_const;
        let directly_writable_lateinit = hoisted_lateinit_field_visibility(self.ir, index)
            .is_some_and(|visibility| {
                matches!(
                    visibility,
                    crate::types::Visibility::Public | crate::types::Visibility::Internal
                )
            });
        let facade = self.facade.clone();
        // A source assignment retained in the companion initializer still dispatches through the
        // public/internal companion setter. Ordinary writes may use the public field directly;
        // this declaration-owned initializer preserves kotlinc's receiver and setter boundary.
        let initializer_setter = (self.render_initializer_boundaries && directly_writable_lateinit)
            .then(|| {
                let (companion, property) = self.ir.jvm_companion_static_declaration(index)?;
                let setter = property.setter?;
                let companion_name = companion.fq_name();
                let setter_name = property
                    .setter_jvm_name
                    .clone()
                    .unwrap_or_else(|| self.ir.functions[setter as usize].name.clone());
                Some((
                    companion_name,
                    crate::jvm::companion_storage::field_name(
                        s.owner
                            .expect("a hoisted companion static has an outer owner"),
                        companion.fq_name,
                    )
                    .to_string(),
                    setter_name,
                ))
            })
            .flatten();
        if let Some((companion_name, companion_field_name, setter_name)) = initializer_setter {
            let outer_name = s
                .owner
                .expect("a hoisted companion static has an outer owner")
                .render();
            let companion_field = self.cw.fieldref(
                &outer_name,
                &companion_field_name,
                &format!("L{companion_name};"),
            );
            code.getstatic(companion_field, 1);
            self.emit_value(value, code);
            if self.diverges(value) {
                return;
            }
            super::debug_lines::restore_initializer_store_line(initializer_line, code);
            let method = self.cw.methodref(
                &companion_name,
                &setter_name,
                &format!("({})V", type_descriptor(jt)),
            );
            code.invokevirtual(method, slot_words(jt) as i32, 0);
            return;
        }
        self.emit_value(value, code);
        if self.diverges(value) {
            return;
        }
        super::debug_lines::restore_initializer_store_line(initializer_line, code);
        // An inlined value leaves its own line in effect; kotlinc marks the statement's line
        // again at the store, as an instance field store does.
        if self.ir.expr_source_lines.contains_key(&value) {
            if let Some(&statement_line) = self.ir.expr_lines.get(&statement) {
                self.mark_expression_line(statement, statement_line, code);
            }
        }
        // A PRIVATE property's field, or the backing field of a source-declared setter, written
        // from another class, goes through its owner's accessor.
        if let Some(storage) = crate::jvm::private_static_access::bridged_setter(self.ir, index)
            .or_else(|| crate::jvm::private_static_access::declared_backing_setter(self.ir, index))
            .filter(|storage| self.static_owner != Some(storage.owner) || self.export_private_calls)
        {
            let m = static_accessors::static_methodref(
                self.cw,
                self.ir,
                &facade,
                storage.owner,
                &format!("access${}$p", property_setter_name(&name)),
                &format!("({})V", type_descriptor(jt)),
            );
            code.invokestatic(m, slot_words(jt) as i32, 0);
            return;
        }
        // Within the facade write the field directly; from another class go through `setX()`.
        // A static declaring an OWNER lives on that class (a companion property is a static
        // field on the outer class). Within the owner write the (private) field directly;
        // from another class — the companion's delegating setter — go through the owner's
        // PUBLIC synthetic `access$set<X>$cp` bridge (kotlinc's hoisted-companion shape).
        // A `@JvmField` static is a PUBLIC field with no bridges: every writer goes
        // `putstatic` directly.
        if let Some(owner) = self.ir.statics[index as usize].owner {
            let owner_name = owner.render();
            let emitted_by_owner = self.static_owner == Some(StaticOwner::Class(owner));
            if !emitted_by_owner && companion_blocks::setter_owned(self.ir, index) {
                let setter = self.ir.statics[index as usize].default_setter_jvm_name();
                let m =
                    self.cw
                        .methodref(&owner_name, &setter, &format!("({})V", type_descriptor(jt)));
                code.invokestatic(m, slot_words(jt) as i32, 0);
            } else if emitted_by_owner
                || !self.ir.is_jvm_companion_hoisted_static(index)
                || self.ir.is_jvm_field_static(index)
                || directly_writable_lateinit
                || hoisted_lateinit_field_visibility(self.ir, index)
                    == Some(crate::types::Visibility::Protected)
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
        } else if (self.static_owner == Some(StaticOwner::Facade) || is_const)
            && !self.calls_facade_lateinit_setter(index)
        {
            let fref = self.cw.fieldref(
                &facade,
                self.ir.static_field_jvm_name(index),
                &type_descriptor(jt),
            );
            code.putstatic(fref, slot_words(jt) as i32);
        } else {
            let m = self.cw.methodref(
                &facade,
                &self.ir.statics[index as usize].default_setter_jvm_name(),
                &format!("({})V", type_descriptor(jt)),
            );
            code.invokestatic(m, slot_words(jt) as i32, 0);
        }
    }

    /// kotlinc keeps a `lateinit` property's default accessors even inside the facade that owns
    /// its field (`JvmPropertiesLowering` never replaces them with field access), so a write in
    /// that facade calls `setX` just as a read calls `getX`.
    fn calls_facade_lateinit_setter(&self, index: u32) -> bool {
        self.ir.statics[index as usize].is_lateinit && self.ir.has_jvm_default_static_setter(index)
    }

    /// Emit one checked static initializer through its declared JVM storage boundary. Facade,
    /// class/object, interface, and enum `<clinit>` paths share this operation so boxing and carrier
    /// selection cannot diverge by owner shape.
    pub(super) fn emit_static_initializer_store(
        &mut self,
        owner: &str,
        static_index: u32,
        init: crate::ir::ExprId,
        code: &mut CodeBuilder,
    ) {
        let field = &self.ir.statics[static_index as usize];
        self.emit_value(init, code);
        if self.diverges(init) {
            return;
        }
        let physical = jvm_value_ty(&field.ty);
        self.adapt_physical_operand_for(init, self.value_ty(init), physical, code);
        // Static storage is identified by its place in the file's static table.
        let reference = self.cw.fieldref(
            owner,
            self.ir.static_field_jvm_name(static_index),
            &type_descriptor(physical),
        );
        // An inlined initializer resets the caller's line. The store owns its source boundary,
        // just as an instance initializer's putfield does after evaluating its value.
        super::debug_lines::restore_initializer_store_line(
            Some(initializer_line(self.ir, field, init)),
            code,
        );
        code.putstatic(reference, slot_words(physical) as i32);
    }
}

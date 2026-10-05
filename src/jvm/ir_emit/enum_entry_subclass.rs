//! The subclass an enum entry with a body becomes.
//!
//! `Enum$ENTRY extends Enum`: a package-private `final` class whose one constructor takes the
//! constant's name and ordinal and calls the enum's constructor with the constant's arguments,
//! plus the entry's overriding methods.

use super::bridge_emission::emit_bridges;
use super::frame_map::FrameKey;
use super::*;

/// Emit a synthesized enum-entry subclass (`Enum$ENTRY extends Enum`) for an entry with a body: a
/// package-private `final` class with one constructor `(String name, int ordinal)V` that evaluates
/// the constant's arguments and calls the enum's constructor with them, plus the entry's overriding
/// methods.
pub(super) fn emit_enum_entry_subclass(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let superclass = c.superclass();
    let fq_name = c.fq_name();
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let mut cw = new_writer(&fq_name, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)
    env.inner_classes.register(&mut cw);

    // Entry-body PROPERTIES are private backing fields (read via synthesized getters, like kotlinc).
    for field in c.fields.iter() {
        let acc = 0x0002 | if field.is_final() { 0x0010 } else { 0 };
        cw.add_field(acc, &field.name, &ir_type_desc(&field.ty));
    }

    // Constructor: `(String, int)V` → `super(name, ordinal, <the constant's arguments>)`, then the
    // property initializers (`this.<prop> = <init>`, from `init_body`). The enum's constructors
    // are private, so the call goes through its `$default` overload when the constant omits an
    // argument and through the marker accessor otherwise, as any subclass's `super(…)` does.
    let enum_prefix = [Ty::String, Ty::Int];
    let ctor_desc = method_descriptor(&enum_prefix, Ty::Unit);
    let prefix_words: u16 = enum_prefix.iter().map(|ty| slot_words(*ty)).sum();
    // kotlinc visits the name, descriptor and signature before the body's constants.
    cw.reserve_method_name("<init>");
    cw.reserve_descriptor(&ctor_desc);
    cw.reserve_descriptor(ENTRY_CONSTRUCTOR_SIGNATURE);
    let mut ctor = CodeBuilder::new(1 + prefix_words);
    let ctor_max = {
        let mut e = Emitter::new(
            ir,
            &mut cw,
            env,
            Some(StaticOwner::Class(c.fq_name)),
            &fq_name,
            facade,
            Ty::Unit,
            c.super_arg_prelude
                .iter()
                .chain(&c.super_args)
                .copied()
                .chain(c.init_body),
        );
        e.this_uninitialized = true;
        let receiver = e.frame.enter(FrameKey::Receiver, Ty::obj_name(c.fq_name));
        e.slots.insert(0, (receiver, Ty::obj_name(c.fq_name))); // `this`
        for (index, &ty) in enum_prefix.iter().enumerate() {
            e.frame.enter(FrameKey::Parameter(index as u16), ty);
        }
        for &statement in &c.super_arg_prelude {
            e.emit(statement, &mut ctor);
        }
        e.emit_constructor_delegation_arguments(
            &c.super_args,
            &c.super_ctor_params,
            &enum_prefix,
            &mut ctor,
        );
        let mut super_params = enum_prefix.to_vec();
        super_params.extend(jvm_tys(&c.super_ctor_params));
        let super_defaults = ir
            .super_constructor_default_arguments
            .get(&c.fq_name_id())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let marker = Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker");
        if !super_defaults.is_empty() {
            for mask in constructor_default_masks(super_defaults, c.super_ctor_params.len()) {
                ctor.push_int(mask, e.cw);
                super_params.push(Ty::Int);
            }
            ctor.aconst_null();
            super_params.push(marker);
        } else if super::constructor_accessors::reached_through_accessor(
            c.super_ctor,
            Some(c.fq_name_id()),
            c.superclass,
        ) {
            ctor.aconst_null();
            super_params.push(marker);
        }
        let super_init = e.cw.methodref(
            &superclass,
            "<init>",
            &method_descriptor(&super_params, Ty::Unit),
        );
        let argw: i32 = super_params.iter().map(|t| slot_words(*t) as i32).sum();
        ctor.invokespecial(super_init, argw, 0);
        e.this_uninitialized = false;
        if let Some(init_body) = c.init_body {
            e.render_initializer_boundaries = true;
            e.emit(init_body, &mut ctor);
            e.render_initializer_boundaries = false;
        }
        e.frame.max()
    };
    ctor.ret_void();
    ctor.ensure_locals(ctor_max);
    ctor.link();
    // Its only parameters are the enum's synthetic `(name, ordinal)` prefix, so the generic
    // `Signature` has no source parameters.
    let ctor_signature = super::constructor_signatures::enum_entry_constructor_signature(
        &signature_formatter,
        &[],
        &[],
    );
    cw.add_method_sig(
        0x0000,
        "<init>",
        &ctor_desc,
        &ctor,
        ctor_signature.as_deref(),
    );
    // The constructor maps to its entry's line and lists the receiver and the enum prefix.
    let locals = [
        ("this".to_string(), format!("L{fq_name};"), 0),
        (
            "$enum$name".to_string(),
            "Ljava/lang/String;".to_string(),
            1,
        ),
        ("$enum$ordinal".to_string(), "I".to_string(), 2),
    ];
    let line = ir
        .class_id_by_name(c.superclass)
        .and_then(|enum_class| {
            ir.classes[enum_class as usize]
                .enum_entries
                .iter()
                .find(|entry| entry.subclass == Some(c.fq_name))
        })
        .map(|entry| entry.decl_line)
        .filter(|&line| line != 0)
        .map(|line| (0, line));
    cw.set_method_debug("<init>", &ctor_desc, line, &locals);

    // The overriding methods + synthesized property getters.
    emit_declared_property_accessors(
        ir,
        c,
        &mut cw,
        &PropertyAccessorEmit {
            fq_name: &fq_name,
            facade,
            formatter: &signature_formatter,
            param_assertions: opts.param_assertions,
            env,
        },
    );
    let markers = member_schedule::property_annotation_marker_fids(ir, c);
    for &fid in &c.methods {
        if markers.contains(&fid) || standalone_method_is_elided(ir, fid, env) {
            continue; // already emitted beside its property's accessors
        }
        // Lambda implementation helpers reparented into an enum-entry subclass remain static; only
        // source member overrides consume an instance receiver. The ordinary class/enum writers
        // already honor this IR bit, and entry subclasses must use the same rule.
        let function = &ir.functions[fid as usize];
        emit_method(
            ir,
            fid,
            StaticOwner::Class(c.fq_name),
            &fq_name,
            facade,
            &mut cw,
            !function.is_static,
            env,
        );
        if ir.function_reference_access_bridges.contains(&fid) {
            super::access_bridges::emit_function_reference_access_bridge(
                ir,
                fid,
                &fq_name,
                &mut cw,
                false,
                c.decl_line,
            );
        }
        if let Some(defaults) = ir.param_defaults(fid) {
            if function.is_static {
                emit_facade_default_stub(
                    ir,
                    fid,
                    StaticOwner::Class(c.fq_name),
                    &fq_name,
                    &mut cw,
                    defaults,
                    env,
                    Ty::obj("java/lang/Object"),
                );
            } else {
                emit_default_stub(
                    ir,
                    fid,
                    Some(StaticOwner::Class(c.fq_name)),
                    &fq_name,
                    facade,
                    &mut cw,
                    defaults,
                    env,
                    false,
                );
            }
        }
    }
    // Entry-body override edges are checked and frozen in Pass 1 like ordinary class overrides.
    // Their anonymous subclass still needs the JVM descriptor adapters derived from those edges
    // (for example `apply(Object)` forwarding to `apply(String)`).
    emit_bridges(ir, c, &mut cw, env);
    if let Some(m) = opts
        .emit_class_metadata
        .then(|| build_class_metadata(ir, c, opts, env))
        .flatten()
    {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    env.run.finish_class(cw)
}

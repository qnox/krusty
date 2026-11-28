//! The synthesized private primary constructor of an `enum class`.
//!
//! An enum declaring only secondary constructors has no primary constructor: every entry names one
//! of the secondaries, and each secondary interns its own header and locals where the declared
//! members schedule emits it. Registering a synthesized primary anyway would collide with a
//! no-argument secondary (both are `(String, int)V`) and intern its header and locals ahead of
//! them.

use super::*;

pub(super) struct EnumPrimaryConstructor<'a> {
    ir: &'a IrFile,
    c: &'a krusty::ir::IrClass,
    env: &'a EmitEnv<'a>,
    facade: &'a str,
    /// The enum's JVM name.
    owner: &'a str,
    /// Every source parameter's JVM type, after the synthetic name and ordinal.
    pub(super) all_param_tys: Vec<Ty>,
    ctor_params: Vec<Ty>,
    pub(super) descriptor: String,
    signature: String,
    parameters: Vec<crate::method_parameters::MethodParameter>,
}

impl<'a> EnumPrimaryConstructor<'a> {
    pub(super) fn new(
        ir: &'a IrFile,
        c: &'a krusty::ir::IrClass,
        env: &'a EmitEnv<'a>,
        facade: &'a str,
        owner: &'a str,
        signature_formatter: &JvmSignatureFormatter,
    ) -> Self {
        let all_param_tys = class_ctor_jvm_tys(ir, c);
        let ctor_params: Vec<Ty> = [Ty::String, Ty::Int]
            .into_iter()
            .chain(all_param_tys.iter().copied())
            .collect();
        let descriptor = method_descriptor(&ctor_params, Ty::Unit);
        let signature =
            constructor_signatures::enum_constructor_signature(signature_formatter, ir, c)
                .expect("an enum constructor always signs its source parameters");
        let parameters = if env.java_parameters {
            crate::method_parameters::enum_constructor(c)
        } else {
            Vec::new()
        };
        EnumPrimaryConstructor {
            ir,
            c,
            env,
            facade,
            owner,
            all_param_tys,
            ctor_params,
            descriptor,
            signature,
            parameters,
        }
    }

    /// Whether the enum has a primary constructor to emit.
    pub(super) fn is_emitted(&self) -> bool {
        self.c.has_primary_ctor || self.c.secondary_ctors.is_empty()
    }

    /// kotlinc visits the whole constructor before the entry constants: name, descriptor, its
    /// generic `Signature` (the two synthetic `Enum` params are erased, leaving `()V`), then its
    /// local-variable strings. `add_field` would otherwise claim those slots for the first entry.
    pub(super) fn reserve_header(&self, cw: &mut ClassWriter) {
        if !self.is_emitted() {
            return;
        }
        let c = self.c;
        let (ctor_desc, ctor_sig, ctor_parameters) =
            (&self.descriptor, &self.signature, &self.parameters);
        cw.reserve_method_pool_with_annotations(
            "<init>",
            ctor_desc,
            Some(ctor_sig),
            &[],
            &c.primary_ctor_annotations,
            ctor_parameters,
        );
        // The source parameters' annotations follow the constructor header, before its body. A private
        // enum constructor carries no nullability annotations.
        cw.reserve_method_parameter_annotation_pool(None, &c.ctor_param_annotations, &[], false);
        // The ctor BODY's `super(name, ordinal)` call resolves before its LocalVariableTable strings.
        cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
    }

    /// Private constructor `(Ljava/lang/String;I<user params>)V` → `super(name, ordinal)` then store
    /// the property params / run the body-property initializers. The user params are ALL
    /// primary-ctor params (from `ctor_args`): a `val`/`var` param backs a field, a plain param is an
    /// argument only (in scope for a body-property initializer), so `all_param_tys` can be wider
    /// than the property fields.
    pub(super) fn emit(&self, self_desc: &str, cw: &mut ClassWriter) {
        let Self {
            ir,
            c,
            env,
            facade,
            owner,
            all_param_tys,
            ctor_params,
            descriptor: ctor_desc,
            signature: ctor_sig,
            parameters: ctor_parameters,
        } = self;
        let (ir, c, env, facade) = (*ir, *c, *env, *facade);
        let fq = owner;
        if !self.is_emitted() {
            return;
        }
        let ctor_words: u16 = ctor_params.iter().map(|t| slot_words(*t)).sum();
        let mut ctor = CodeBuilder::new(1 + ctor_words);
        ctor.aload(0);
        ctor.aload(1);
        load(Ty::Int, 2, &mut ctor);
        let super_init = cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
        ctor.invokespecial(super_init, 2, 0);
        // kotlinc maps the `super(name, ordinal)` call to where the declaration starts (annotations
        // included), then maps each property store to that property's own declaration line.
        let mut store_lines = initializer_lines::ConstructorInitializerLines::default();
        let mut max_locals = 1 + ctor_words;
        // Store constructor-property parameters unless lowering already represented those stores in the
        // init body. The existence of a body property is not that contract: an init body can contain
        // only body-property stores while leaving `explicit_param_stores` false.
        if !c.explicit_param_stores {
            let mut slot = 3u16;
            let mut field_i = 0usize;
            for (argument, ty) in c.ctor_args.iter().zip(all_param_tys) {
                if argument.is_field {
                    let name = instance_field_jvm_name(ir, env.run, c, field_i);
                    if let Some(line) = property_line(ir, c, field_i as u32) {
                        store_lines.push((ctor.bytes.len() as u16, line));
                    }
                    ctor.aload(0);
                    load(*ty, slot, &mut ctor);
                    let field = cw.fieldref(fq, &name, &type_descriptor(*ty));
                    ctor.putfield(field, slot_words(*ty) as i32);
                    field_i += 1;
                }
                slot += slot_words(*ty);
            }
        }
        // Emit body-property stores and `init` statements through the standard IR emitter, mapping value
        // ids onto the enum's slot layout: `this` at 0, then every user parameter at slots 3+ after the
        // synthetic name and ordinal. A pure SetField block retains each store's source line.
        if let Some(init_body) = c.init_body {
            let marks_before = ctor.line_marks().len();
            let mut e = Emitter::new(
                ir,
                cw,
                env,
                EmitterOwner {
                    static_owner: Some(StaticOwner::Class(c.fq_name)),
                    owner: fq,
                    facade,
                },
                Ty::Unit,
                [init_body],
            );
            let receiver = e.frame.enter(FrameKey::Receiver, Ty::obj_name(c.fq_name));
            e.slots.insert(0, (receiver, Ty::obj_name(c.fq_name)));
            // The synthetic name and ordinal come first; no semantic value names them.
            e.frame.enter(FrameKey::Parameter(0), Ty::String);
            e.frame.enter(FrameKey::Parameter(1), Ty::Int);
            for (i, t) in all_param_tys.iter().enumerate() {
                let value = i as u32 + 1;
                let s = e.frame.enter(FrameKey::Value(value), *t);
                e.slots.insert(value, (s, *t));
            }
            e.render_initializer_boundaries = true;
            e.emit_constructor_init_body(c, init_body, &mut ctor, &mut store_lines);
            e.render_initializer_boundaries = false;
            max_locals = max_locals.max(e.frame.max());
            store_lines.record_restored(&ctor, marks_before);
        }
        // The `return` maps to the primary constructor's start; a pending closing `init` line owns a
        // `nop` before that transition.
        let return_line = primary_constructor_line(ir, c);
        if return_line != 0 && !store_lines.is_empty() {
            ctor.mark_line(return_line);
        }
        let ctor_return_pc = ctor.bytes.len() as u16;
        ctor.ret_void();
        ctor.ensure_locals(max_locals);
        ctor.link();
        // An enum's ctor is `private`: an entry subclass reaches it via its marker accessor or `$default`.
        let base_ctor_acc = 0x0002;
        // kotlinc's `Signature` on the enum ctor lists only the USER params, without the synthetic
        // leading `(String, int)`: `()V` for a plain enum, `(I)V` for `E(val n: Int)`.
        seed_enum_constructor_locals(ir, c, self_desc, cw);
        cw.add_method_sig(
            base_ctor_acc | method_access::primary_constructor_varargs(c),
            "<init>",
            ctor_desc,
            &ctor,
            Some(ctor_sig),
        );
        cw.set_method_parameters("<init>", ctor_desc, ctor_parameters);
        // The synthetic `(String, int)` prefix is not annotable: the attributes count only the
        // source parameters.
        cw.set_annotable_method_param_annotations("<init>", ctor_desc, &c.ctor_param_annotations);
        function_annotations::emit_constructor_declared(cw, &c.primary_ctor_annotations, ctor_desc);
        let header = c.decl_line;
        let start = if c.decl_start_line == 0 {
            header
        } else {
            c.decl_start_line
        };
        if header != 0 {
            // Each retained store maps to its property's line and the `return` to the constructor
            // start. A missing line is not reconstructed in the backend.
            let entries = store_lines.method_entries(
                (0, start),
                (ctor_return_pc, primary_constructor_line(ir, c)),
            );
            cw.set_method_lines("<init>", ctor_desc, &entries);
        }
    }
}

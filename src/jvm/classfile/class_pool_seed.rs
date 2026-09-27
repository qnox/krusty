//! Seeding a class's constant pool with the primary constructor header and tail that kotlinc's
//! writer interns before krusty emits the members that use them, in ASM visit order.

use super::*;

impl ClassWriter {
    /// Pre-intern the primary constructor's HEADER in kotlinc/ASM's visit order: name, descriptor,
    /// generic `Signature`, its own annotations, then its parameter annotations. ASM interns a
    /// method's header before its body, while krusty builds the body first; the body itself then
    /// interns naturally, since the constructor is the first method krusty emits. Call BEFORE any
    /// `add_field`/`add_method` for the class.
    pub fn seed_plain_class_pool(
        &mut self,
        ctor_desc: &str,
        parameters: &[SeedCtorParameter],
        // Per-member generic `Signature`s (parameterized-type ctor/accessor/field members).
        sigs: &MemberSignatures,
        // The primary constructor's DECLARED annotations (`class C @Mark constructor(…)`), visible
        // then invisible — interned at the constructor's own annotation visit.
        ctor_annotations: &[crate::ir::AppliedAnnotation],
    ) {
        // Primary constructor: name + descriptor are interned at method entry, before its body.
        self.cp.utf8("<init>");
        self.cp.utf8(ctor_desc);
        // The ctor's generic Signature (`(Ljava/util/List<Ljava/lang/String;>;)V`) — right after the desc.
        if let Some(s) = sigs.ctor {
            self.cp.utf8(s);
        }
        // The constructor's OWN annotations, before the parameter ones: ASM visits `visitAnnotation`
        // ahead of `visitParameterAnnotation`, so `class C @Mark constructor(val x: Int)` interns
        // `Lp/Mark;` right after `(I)V` and before the body's `()V`.
        for annotation in ctor_annotations {
            let _ = self.encode_annotation(annotation);
        }
        // The constructor's PARAMETER annotations, which kotlinc visits before the body. The whole
        // `RuntimeVisibleParameterAnnotations` attribute is written first, so every parameter's
        // RUNTIME-retained USER annotation type interns ahead of anything invisible.
        for parameter in parameters {
            for ty in &parameter.visible_ann_types {
                self.cp.utf8(ty);
            }
        }
        // Then `RuntimeInvisibleParameterAnnotations`, parameter by parameter: the BINARY-retained USER
        // types, then that parameter's synthesized `@NotNull`/`@Nullable`. The nullability types are
        // reused by every getter return / setter parameter annotation and guard.
        let mut seeded_notnull = false;
        let mut seeded_nullable = false;
        for parameter in parameters {
            for ty in &parameter.invisible_ann_types {
                self.cp.utf8(ty);
            }
            let kind = if self.nullability_annotations {
                parameter.ann_kind
            } else {
                0
            };
            if kind == 1 && !seeded_notnull {
                self.cp.utf8("Lorg/jetbrains/annotations/NotNull;");
                seeded_notnull = true;
            } else if kind == 2 && !seeded_nullable {
                self.cp.utf8("Lorg/jetbrains/annotations/Nullable;");
                seeded_nullable = true;
            }
        }
    }

    /// Seed what follows the primary constructor's body: its LocalVariableTable strings (`this`
    /// and its type, then each named parameter), then the header descriptor of the `$default`
    /// overload kotlinc writes right after it. The overload's body interns its own entries when it
    /// is emitted, which is right after the primary.
    pub fn seed_plain_constructor_tail(
        &mut self,
        this_internal: &str,
        parameter_locals: &[(String, String)],
        default_marker_desc: Option<&str>,
    ) {
        self.cp.utf8("this");
        self.cp.utf8(&format!("L{this_internal};"));
        for (name, desc) in parameter_locals {
            self.cp.utf8(name);
            self.cp.utf8(desc);
        }
        if let Some(desc) = default_marker_desc {
            self.cp.utf8(desc);
        }
    }
}

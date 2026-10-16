//! A method's own annotation attributes: the declared ones and kotlinc's return nullability.

use super::{
    annotation_values::{NOT_NULL, NULLABLE},
    ClassWriter, ConstPool,
};

impl ClassWriter {
    /// Attach kotlinc's non-null annotations to a previously-added method (matched by name+descriptor):
    /// `@org.jetbrains.annotations.NotNull` / `@Nullable` on the return (a method-level
    /// `RuntimeInvisibleAnnotations`) and/or on individual parameters (`RuntimeInvisibleParameterAnnotations`).
    /// `ret` is the return annotation's type descriptor (e.g. `Lorg/jetbrains/annotations/NotNull;`) or
    /// `None`; `params` gives each parameter's annotation type or `None`, in parameter order. Interning
    /// the annotation types here fixes their constant-pool position. No-op if the method isn't found.
    pub fn set_method_nullability(
        &mut self,
        name: &str,
        desc: &str,
        ret: Option<&str>,
        params: &[Option<&str>],
    ) {
        if !self.nullability_annotations {
            return;
        }
        // Resolve WITHOUT interning first, like `set_method_debug`: describing a method that was never
        // emitted (the accessors of a `private` property, which are read straight from the field) must
        // not perturb the constant pool — the name and descriptor would be orphan entries.
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if !self.methods.iter().any(|m| m.name == n && m.desc == d) {
            return;
        }
        // A parameterless annotation is `type_index(u2) + num_element_value_pairs(u2 = 0)`.
        let empty_ann = |cp: &mut ConstPool, ty: &str| -> Vec<u8> {
            let ti = cp.utf8(ty);
            vec![(ti >> 8) as u8, ti as u8, 0, 0]
        };
        let invisible_anns: Vec<Vec<u8>> = ret
            .map(|t| vec![empty_ann(&mut self.cp, t)])
            .unwrap_or_default();
        let has_param_ann = params.iter().any(|p| p.is_some());
        let param_anns: Vec<Vec<Vec<u8>>> = if has_param_ann {
            params
                .iter()
                .map(|p| {
                    p.map(|t| vec![empty_ann(&mut self.cp, t)])
                        .unwrap_or_default()
                })
                .collect()
        } else {
            Vec::new()
        };
        // The compiler's own return nullability replaces an earlier one, never the method's
        // declared annotations, which stay first.
        let nullability = [NOT_NULL, NULLABLE].map(|ty| self.cp.lookup_utf8(ty));
        if let Some(m) = self.methods.iter_mut().find(|m| m.name == n && m.desc == d) {
            m.invisible_anns.retain(|annotation| {
                let ty = u16::from_be_bytes([annotation[0], annotation[1]]);
                !nullability.contains(&Some(ty))
            });
            m.invisible_anns.extend(invisible_anns);
            m.param_anns = param_anns;
        }
    }

    /// Attach USER annotations to a previously-added method (matched by name+descriptor), split by
    /// retention: RUNTIME → `RuntimeVisibleAnnotations`, BINARY → `RuntimeInvisibleAnnotations` —
    /// the method analogue of [`Self::set_last_field_annotations`]. Interning the annotation types
    /// here fixes their constant-pool position. No-op if the method isn't found.
    pub fn set_method_annotations(
        &mut self,
        name: &str,
        desc: &str,
        annotations: &crate::ir::DeclarationAnnotations,
    ) {
        // Resolve WITHOUT interning first (as `set_method_nullability` does): describing a method
        // that was never emitted must not leave orphan name/descriptor entries in the pool.
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if !self.methods.iter().any(|m| m.name == n && m.desc == d) {
            return;
        }
        let (vis, invis) = self.encode_method_annotations(annotations);
        if let Some(m) = self.methods.iter_mut().find(|m| m.name == n && m.desc == d) {
            m.visible_anns = vis;
            // A DECLARED annotation precedes the compiler's own `@NotNull`/`@Nullable` on the
            // return, whichever order the two setters ran in — kotlinc writes the user's first.
            m.invisible_anns.splice(0..0, invis);
        }
    }

    /// Encode a method's retained annotations: every visible one, then every invisible one. kotlinc
    /// buffers a method in a tree node that replays its annotations attribute by attribute, so the
    /// constants intern in that order rather than in declaration order as a class's do.
    pub(super) fn encode_method_annotations(
        &mut self,
        annotations: &crate::ir::DeclarationAnnotations,
    ) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        use crate::types::AnnotationRetention::{Binary, Source};
        let mut encode = |visible: bool| -> Vec<Vec<u8>> {
            annotations
                .iter()
                .filter(|retained| {
                    retained.retention != Source && (retained.retention == Binary) != visible
                })
                .map(|retained| self.encode_annotation(&retained.annotation))
                .collect()
        };
        let visible = encode(true);
        (visible, encode(false))
    }
}

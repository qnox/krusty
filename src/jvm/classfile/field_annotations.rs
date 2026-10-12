//! A field's declared and synthesized annotation attributes, including their constant-pool visit
//! order.

use super::ClassWriter;

impl ClassWriter {
    /// Attach user annotations to the most recently added field. The JVM representation boundary
    /// maps semantic retention onto visible/invisible class-file attributes.
    pub fn set_last_field_annotations(&mut self, annotations: &crate::ir::DeclarationAnnotations) {
        let (visible, invisible) = self.encode_declaration_annotations(annotations);
        if let Some(field) = self.fields.last_mut() {
            field.visible_anns = visible;
            field.invisible_anns = invisible;
        }
    }

    /// Attach `@NotNull` / `@Nullable` (a `RuntimeInvisibleAnnotations`) to a previously-added
    /// field by name. The field's name and descriptor intern before the annotation, as kotlinc
    /// visits the field. No-op if nullability annotations are disabled or the field is not found.
    pub fn set_field_nullability(&mut self, name: &str, desc: &str, ann_type: &str) {
        if !self.nullability_annotations {
            return;
        }
        let name = self.cp.utf8(name);
        let desc = self.cp.utf8(desc);
        let annotation_type = self.cp.utf8(ann_type);
        let annotation = vec![(annotation_type >> 8) as u8, annotation_type as u8, 0, 0];
        if let Some(field) = self
            .fields
            .iter_mut()
            .find(|field| field.name == name && field.desc == desc)
        {
            if !field.invisible_anns.contains(&annotation) {
                field.invisible_anns.push(annotation);
            }
        }
    }
}

//! Fields whose constant-pool entries intern at the field-table visit, after every method, as
//! kotlinc's writer visits them.

use super::{split_declaration_annotations, ClassWriter, FieldInfo};

/// A field whose constant-pool interning is DEFERRED to the field-table visit: kotlinc's writer
/// visits methods first and fields last, so a field entry the method bodies never introduced (a
/// `const val`'s name + `ConstantValue`, a facade backing field) interns AFTER every method window.
/// Realized into a [`FieldInfo`] (appended after the eagerly-added fields) by `intern_late_fields`.
pub(super) struct LateField {
    placement: LateFieldPlacement,
    access: u16,
    name: String,
    desc: String,
    signature: Option<String>,
    /// The `ConstantValue` payload, interned at realization (`None` for a `<clinit>`-initialized field).
    const_value: Option<crate::ir::IrConst>,
    /// BINARY-retention nullability annotation type descriptor (`Lorg/jetbrains/annotations/NotNull;`).
    ann: Option<String>,
    /// USER annotations on the field (`@Target(FIELD)` on the property), split by retention. Held
    /// unencoded so their types intern in the field-table window, where kotlinc interns them —
    /// before the class's own `@Metadata`, not after the methods.
    user_visible: Vec<crate::ir::AppliedAnnotation>,
    user_invisible: Vec<crate::ir::AppliedAnnotation>,
}

/// Position in the finished field table, independent from the constant-pool interning window.
enum LateFieldPlacement {
    Trailing,
    Leading,
}

impl LateField {
    fn new(
        access: u16,
        name: &str,
        desc: &str,
        signature: Option<&str>,
        const_value: Option<crate::ir::IrConst>,
        ann: Option<&str>,
        placement: LateFieldPlacement,
    ) -> Self {
        Self {
            placement,
            access,
            name: name.to_string(),
            desc: desc.to_string(),
            signature: signature.map(str::to_string),
            const_value,
            ann: ann.map(str::to_string),
            user_visible: Vec::new(),
            user_invisible: Vec::new(),
        }
    }
}

impl ClassWriter {
    /// Declare a field whose pool entries intern at the FIELD-TABLE visit (after every method) —
    /// kotlinc's writer order. Use for a field the method bodies don't introduce; a field whose
    /// name/descriptor the bodies DO intern can use either form (the table interning dedups).
    pub fn add_field_late(
        &mut self,
        access: u16,
        name: &str,
        desc: &str,
        const_value: Option<crate::ir::IrConst>,
        ann: Option<&str>,
    ) {
        self.add_field_late_sig(access, name, desc, None, const_value, ann);
    }

    /// Deferred field declaration with an optional generic `Signature` value.
    pub fn add_field_late_sig(
        &mut self,
        access: u16,
        name: &str,
        desc: &str,
        signature: Option<&str>,
        const_value: Option<crate::ir::IrConst>,
        ann: Option<&str>,
    ) {
        self.late_fields.push(LateField::new(
            access,
            name,
            desc,
            signature,
            const_value,
            ann,
            LateFieldPlacement::Trailing,
        ));
    }

    /// [`add_field_late_sig`], but the realized field LEADS the field table, in declaration order
    /// (kotlinc's `$$delegatedProperties` and `Companion` come before the instance fields).
    pub fn add_field_late_leading(
        &mut self,
        (access, name, desc): (u16, &str, &str),
        signature: Option<&str>,
        ann: Option<&str>,
    ) {
        let (lead, fields) = (LateFieldPlacement::Leading, &mut self.late_fields);
        fields.push(LateField::new(
            access, name, desc, signature, None, ann, lead,
        ));
    }

    /// Realize the deferred fields that lead the field table now: they intern before the
    /// annotations of the eagerly declared fields after them, which the emitter attaches before
    /// `finish` (a class's `$$delegatedProperties` and `Companion` ahead of its instance fields).
    pub(in crate::jvm) fn realize_leading_late_fields(&mut self) {
        let late = std::mem::take(&mut self.late_fields);
        let (leading, trailing) = late
            .into_iter()
            .partition(|field| matches!(field.placement, LateFieldPlacement::Leading));
        self.late_fields = leading;
        self.intern_late_fields();
        self.late_fields = trailing;
    }

    /// Realize the deferred fields NOW rather than at `finish`. An enum's leading fields carry a
    /// generic `Signature`, and kotlinc interns that string BEFORE the class's own annotation
    /// descriptors — so the emitter forces the field visit before queuing those. Draining, so a
    /// second call (from `finish`) is a no-op.
    pub(in crate::jvm) fn realize_late_fields(&mut self) {
        self.intern_late_fields();
    }

    pub(super) fn intern_late_fields(&mut self) {
        let mut lead_at = 0usize;
        // Fields intern in table order, so the leading ones go first whenever they were declared.
        let mut late = std::mem::take(&mut self.late_fields);
        late.sort_by_key(|field| matches!(field.placement, LateFieldPlacement::Trailing));
        for lf in late {
            let n = self.cp.utf8(&lf.name);
            let d = self.cp.utf8(&lf.desc);
            let signature = lf.signature.as_ref().map(|value| self.cp.utf8(value));
            let cv = lf.const_value.as_ref().and_then(|c| {
                use crate::ir::IrConst;
                Some(match c {
                    IrConst::Boolean(b) => self.const_int(*b as i32),
                    IrConst::Byte(v) => self.const_int(*v as i32),
                    IrConst::Short(v) => self.const_int(*v as i32),
                    IrConst::Int(v) => self.const_int(*v),
                    // The JVM carries `UByte`/`UShort` in `B`/`S`, so the pool entry is the
                    // value read as that signed primitive: 200u is the byte -56.
                    IrConst::UByte(v) => self.const_int(i32::from(*v as i8)),
                    IrConst::UShort(v) => self.const_int(i32::from(*v as i16)),
                    IrConst::UInt(v) => self.const_int(*v as i32),
                    IrConst::ULong(v) => self.const_long(*v as i64),
                    IrConst::Char(ch) => self.const_int(*ch as i32),
                    IrConst::Long(v) => self.const_long(*v),
                    IrConst::Float(v) => self.const_float(*v),
                    IrConst::Double(v) => self.const_double(*v),
                    IrConst::String(s) => self.const_string_kt(s),
                    IrConst::Null => return None,
                })
            });
            // A user annotation interns before the nullability one, matching the attribute order
            // (`RuntimeVisibleAnnotations` precedes `RuntimeInvisibleAnnotations`).
            let visible_anns: Vec<Vec<u8>> = lf
                .user_visible
                .iter()
                .map(|annotation| self.encode_annotation(annotation))
                .collect();
            let mut invisible_anns: Vec<Vec<u8>> = lf
                .user_invisible
                .iter()
                .map(|annotation| self.encode_annotation(annotation))
                .collect();
            let nullability = lf.ann.as_deref().and_then(|a| self.written_annotation(a));
            invisible_anns.extend(nullability.map(|a| {
                let ti = self.cp.utf8(a);
                vec![(ti >> 8) as u8, ti as u8, 0, 0]
            }));
            let info = FieldInfo {
                access: lf.access,
                name: n,
                desc: d,
                signature,
                const_value: cv,
                visible_anns,
                invisible_anns,
            };
            match lf.placement {
                LateFieldPlacement::Trailing => self.fields.push(info),
                LateFieldPlacement::Leading => {
                    self.fields.insert(lead_at, info);
                    lead_at += 1;
                }
            }
        }
    }

    /// Attach user annotations to the most recently DEFERRED field ([`Self::add_field_late_sig`]),
    /// which realizes them when the field table interns.
    pub fn set_last_late_field_annotations(
        &mut self,
        annotations: &crate::ir::DeclarationAnnotations,
    ) {
        // Kept UNENCODED: a deferred field's annotation types must intern in the field-table window,
        // which `intern_late_fields` opens, not here. Only the retention → attribute split happens now.
        let (visible, invisible) = split_declaration_annotations(annotations);
        if let Some(field) = self.late_fields.last_mut() {
            field.user_visible = visible;
            field.user_invisible = invisible;
        }
    }
}

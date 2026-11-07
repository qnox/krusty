//! The class's `@kotlin.Metadata` annotation: a file facade's, a multifile part's (`k=5`, with
//! `xs` naming its facade) and a multifile facade's (`k=4`, listing its parts).

use super::{u2, ClassWriter, MetadataStamp};

impl ClassWriter {
    /// Attach a `@kotlin.Metadata` annotation (RuntimeVisibleAnnotations) describing the file facade.
    /// `d1`/`d2` are the encoded protobuf payload + string table.
    pub fn set_kotlin_metadata(
        &mut self,
        k: i32,
        stamp: MetadataStamp,
        xi: i32,
        d1: &[String],
        d2: &[String],
    ) {
        let has_payload = !d1.is_empty() || !d2.is_empty();
        self.set_kotlin_metadata_elements(
            (k, stamp, xi),
            has_payload.then_some(d1),
            has_payload.then_some(d2),
            None,
        );
    }

    /// `@kotlin.Metadata` for a multifile class part (`k=5`): the package payload, then `xs`, the
    /// internal name of the facade the part belongs to.
    pub fn set_multifile_part_metadata(
        &mut self,
        stamp: MetadataStamp,
        xi: i32,
        (d1, d2): (&[String], &[String]),
        facade: &str,
    ) {
        self.set_kotlin_metadata_elements((5, stamp, xi), Some(d1), Some(d2), Some(facade));
    }

    /// `@kotlin.Metadata` for a multifile class facade (`k=4`): `d1` lists its parts' internal
    /// names, and there is no string table.
    pub fn set_multifile_facade_metadata(
        &mut self,
        stamp: MetadataStamp,
        xi: i32,
        parts: &[String],
    ) {
        self.set_kotlin_metadata_elements((4, stamp, xi), Some(parts), None, None);
    }

    fn set_kotlin_metadata_elements(
        &mut self,
        (k, stamp, xi): (i32, MetadataStamp, i32),
        d1: Option<&[String]>,
        d2: Option<&[String]>,
        xs: Option<&str>,
    ) {
        // The field-table visit precedes the class annotations in kotlinc's writer.
        self.intern_late_fields();
        // kotlinc interns each element's KEY immediately before that element's VALUE constants (mv key
        // then the mv integers, then k key then its integer, …) rather than all keys up front — so the
        // constant pool interleaves keys and values. Match that by interning each key inline.
        let anno_type = self.cp.utf8("Lkotlin/Metadata;");
        // One `annotation` structure (type_index + element_value_pairs) — appended to the shared list so
        // `finish` writes a single `RuntimeVisibleAnnotations` attribute even alongside user annotations.
        let mut body = Vec::new();
        u2(&mut body, anno_type);
        let elements = 3 + [d1.is_some(), d2.is_some(), xs.is_some()]
            .into_iter()
            .filter(|&present| present)
            .count() as u16;
        u2(&mut body, elements);
        let n_mv = self.cp.utf8("mv");
        u2(&mut body, n_mv);
        self.ev_int_array(&mut body, &stamp.version);
        let n_k = self.cp.utf8("k");
        u2(&mut body, n_k);
        self.ev_int(&mut body, k);
        let n_xi = self.cp.utf8("xi");
        u2(&mut body, n_xi);
        self.ev_int(&mut body, stamp.xi(xi));
        if let Some(d1) = d1 {
            let n_d1 = self.cp.utf8("d1");
            u2(&mut body, n_d1);
            self.ev_str_array(&mut body, d1);
        }
        if let Some(d2) = d2 {
            let n_d2 = self.cp.utf8("d2");
            u2(&mut body, n_d2);
            self.ev_str_array(&mut body, d2);
        }
        if let Some(xs) = xs {
            let n_xs = self.cp.utf8("xs");
            u2(&mut body, n_xs);
            self.ev_str(&mut body, xs);
        }
        self.runtime_annotations.push(body);
    }
}

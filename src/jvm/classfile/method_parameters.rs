//! Method parameter annotations and Java reflection names.

use super::{descriptor_param_count, split_declaration_annotations, ClassWriter};

impl ClassWriter {
    /// Attach source parameter annotations, split by retention and padded to physical JVM arity.
    pub fn set_method_param_annotations(
        &mut self,
        name: &str,
        desc: &str,
        params: &[crate::ir::DeclarationAnnotations],
    ) {
        if params
            .iter()
            .all(crate::ir::DeclarationAnnotations::is_empty)
        {
            return;
        }
        let arity = descriptor_param_count(desc).max(params.len());
        let (Some(name_index), Some(desc_index)) =
            (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc))
        else {
            return;
        };
        if !self
            .methods
            .iter()
            .any(|method| method.name == name_index && method.desc == desc_index)
        {
            return;
        }
        let per_parameter = (0..arity)
            .map(|index| match params.get(index) {
                Some(annotations) => split_declaration_annotations(annotations),
                None => (Vec::new(), Vec::new()),
            })
            .collect::<Vec<_>>();
        let visible = per_parameter
            .iter()
            .map(|(annotations, _)| {
                annotations
                    .iter()
                    .map(|annotation| self.encode_annotation(annotation))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let invisible = per_parameter
            .iter()
            .map(|(_, annotations)| {
                annotations
                    .iter()
                    .map(|annotation| self.encode_annotation(annotation))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let method = self
            .methods
            .iter_mut()
            .find(|method| method.name == name_index && method.desc == desc_index)
            .expect("the parameter-annotation target was checked above");
        if visible.iter().any(|parameter| !parameter.is_empty()) {
            method.visible_param_anns = visible;
        }
        if invisible.iter().any(|parameter| !parameter.is_empty()) {
            method.user_invisible_param_anns = invisible;
        }
    }

    /// Attach one exact reflection name/flag pair per physical descriptor parameter.
    pub fn set_method_parameters(
        &mut self,
        name: &str,
        desc: &str,
        params: &[(Option<String>, u16)],
    ) {
        if params.is_empty() {
            return;
        }
        assert_eq!(
            params.len(),
            descriptor_param_count(desc),
            "MethodParameters must describe every physical descriptor parameter"
        );
        assert!(
            params.len() <= u8::MAX as usize,
            "too many MethodParameters entries"
        );
        let name_index = self
            .cp
            .lookup_utf8(name)
            .expect("MethodParameters target name must be reserved");
        let desc_index = self
            .cp
            .lookup_utf8(desc)
            .expect("MethodParameters target descriptor must be reserved");
        let method = self
            .methods
            .iter()
            .position(|method| method.name == name_index && method.desc == desc_index)
            .expect("MethodParameters must target an emitted method");
        let entries = params
            .iter()
            .map(|(parameter, flags)| {
                (
                    parameter
                        .as_deref()
                        .map_or(0, |parameter| self.cp.utf8(parameter)),
                    *flags,
                )
            })
            .collect();
        assert!(
            self.methods[method].method_parameters.is_empty(),
            "MethodParameters may be attached only once"
        );
        self.methods[method].method_parameters = entries;
    }
}

#[cfg(test)]
mod tests {
    use super::ClassWriter;
    use crate::jvm::classfile::CodeBuilder;

    #[test]
    fn preserves_an_unnamed_parameter_as_name_index_zero() {
        let mut writer = ClassWriter::new("Example", "java/lang/Object");
        writer.reserve_method_pool("f", "(II)V", None, &[]);
        let mut code = CodeBuilder::new(2);
        code.ret_void();
        writer.add_method(0x0009, "f", "(II)V", &code);

        writer.set_method_parameters(
            "f",
            "(II)V",
            &[(None, 0), (Some("named".to_string()), 0x1000)],
        );

        assert_eq!(writer.methods.len(), 1);
        assert_eq!(writer.methods[0].method_parameters.len(), 2);
        assert_eq!(writer.methods[0].method_parameters[0], (0, 0));
        assert_ne!(writer.methods[0].method_parameters[1].0, 0);
        assert_eq!(writer.methods[0].method_parameters[1].1, 0x1000);
    }

    #[test]
    #[should_panic(expected = "MethodParameters must describe every physical descriptor parameter")]
    fn rejects_partial_physical_parameter_lists() {
        let mut writer = ClassWriter::new("Example", "java/lang/Object");
        writer.set_method_parameters("f", "(II)V", &[(Some("first".to_string()), 0)]);
    }

    #[test]
    #[should_panic(expected = "MethodParameters must target an emitted method")]
    fn rejects_a_reserved_but_missing_method() {
        let mut writer = ClassWriter::new("Example", "java/lang/Object");
        writer.reserve_method_pool("f", "(I)V", None, &[]);
        writer.set_method_parameters("f", "(I)V", &[(Some("value".to_string()), 0)]);
    }
}

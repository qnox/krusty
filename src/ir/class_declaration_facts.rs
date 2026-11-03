//! Declaration facts a file records about its classes: synthetic and deprecated marks, constructor
//! defaults, generic and field signatures. Lowering records them once; backends read them by class.

use super::{ExprId, IrClass, IrFile, IrGenericSig, IrTypeParameter};
use crate::types::{Ty, TypeName};

impl IrFile {
    pub(crate) fn record_declared_class_self_type(&mut self, classifier: TypeName, self_type: Ty) {
        assert_eq!(self_type.kotlin_class_internal(), Some(classifier));
        let previous = self.declared_class_self_types.insert(classifier, self_type);
        assert!(
            previous.is_none_or(|previous| previous == self_type),
            "a class declaration cannot change its checked self application"
        );
    }

    pub(crate) fn declared_class_self_type(&self, classifier: TypeName) -> Option<Ty> {
        self.declared_class_self_types.get(&classifier).copied()
    }

    pub fn mark_synthetic_class(&mut self, internal: TypeName) {
        self.synthetic_classes.insert(internal);
    }

    pub fn is_synthetic_class(&self, internal: TypeName) -> bool {
        self.synthetic_classes.contains(&internal)
    }

    pub(crate) fn mark_declarationless_class(&mut self, internal: TypeName) {
        self.declarationless_classes.insert(internal);
    }

    pub(crate) fn is_declarationless_class(&self, internal: TypeName) -> bool {
        self.declarationless_classes.contains(&internal)
    }

    pub fn mark_deprecated_class(&mut self, internal: TypeName) {
        self.deprecated_classes.insert(internal);
    }

    pub fn is_deprecated_class(&self, internal: TypeName) -> bool {
        self.deprecated_classes.contains(&internal)
    }

    pub fn insert_class_ctor_defaults(&mut self, internal: &str, defaults: Vec<Option<u32>>) {
        self.insert_class_ctor_defaults_name(crate::types::type_name(internal), defaults);
    }

    pub fn insert_class_ctor_defaults_name(
        &mut self,
        internal: TypeName,
        defaults: Vec<Option<u32>>,
    ) {
        self.class_ctor_defaults.insert(internal, defaults);
    }

    pub fn class_ctor_defaults(&self, internal: &str) -> Option<&Vec<Option<u32>>> {
        self.class_ctor_defaults_name(crate::types::type_name(internal))
    }

    pub fn class_ctor_defaults_name(&self, internal: TypeName) -> Option<&Vec<Option<u32>>> {
        self.class_ctor_defaults.get(&internal)
    }

    /// Checked primary-constructor default expressions, each an independently emitted value root.
    pub(crate) fn class_ctor_default_roots(&self) -> impl Iterator<Item = ExprId> + '_ {
        self.class_ctor_defaults
            .values()
            .flat_map(|defaults| defaults.iter().flatten().copied())
    }

    pub fn take_class_ctor_defaults_name(
        &mut self,
        internal: TypeName,
    ) -> Option<Vec<Option<u32>>> {
        self.class_ctor_defaults.remove(&internal)
    }

    pub fn insert_class_signature(&mut self, internal: &str, sig: IrGenericSig) {
        self.insert_class_signature_name(crate::types::type_name(internal), sig);
    }

    pub fn insert_class_signature_name(&mut self, internal: TypeName, sig: IrGenericSig) {
        self.class_signatures.insert(internal, sig);
    }

    pub fn class_signature(&self, internal: &str) -> Option<&IrGenericSig> {
        self.class_signatures
            .get(&crate::types::type_name(internal))
    }

    pub fn class_signature_name(&self, internal: crate::types::TypeName) -> Option<&IrGenericSig> {
        self.class_signatures.get(&internal)
    }

    /// Record the signature a plugin's IR lowering gives a class it generated, when it differs from
    /// the declared one (kotlinx.serialization's bounded `$serializer` type parameters).
    pub(crate) fn insert_lowered_class_signature(&mut self, internal: TypeName, sig: IrGenericSig) {
        self.lowered_class_signatures.insert(internal, sig);
    }

    /// The signature the class's classfile is written from: its lowered one, else its declaration.
    pub(crate) fn lowered_class_signature(&self, internal: TypeName) -> Option<&IrGenericSig> {
        self.lowered_class_signatures
            .get(&internal)
            .or_else(|| self.class_signatures.get(&internal))
    }

    /// `class` applied to its own type parameters (`C<T>`): the type of its `this`.
    pub(crate) fn class_type(&self, class: &IrClass) -> Ty {
        let arguments: Vec<Ty> = self
            .class_signature_name(class.fq_name)
            .map(|signature| {
                signature
                    .type_params
                    .iter()
                    .map(IrTypeParameter::ty)
                    .collect()
            })
            .unwrap_or_default();
        Ty::obj_args_name(class.fq_name, &arguments)
    }

    pub fn insert_field_signatures(&mut self, internal: &str, sigs: Vec<(String, String)>) {
        self.field_signatures
            .insert(crate::types::type_name(internal), sigs);
    }

    pub fn field_signatures(&self, internal: &str) -> Option<&Vec<(String, String)>> {
        self.field_signatures
            .get(&crate::types::type_name(internal))
    }
}

//! Declaration facts a file records about its classes: synthetic and deprecated marks, constructor
//! defaults, generic and field signatures. Lowering records them once; backends read them by class.

use super::{IrClass, IrFile, IrGenericSig, IrTypeParameter};
use crate::types::{Ty, TypeName};

impl IrFile {
    pub fn mark_synthetic_class(&mut self, internal: TypeName) {
        self.synthetic_classes.insert(internal);
    }

    pub fn is_synthetic_class(&self, internal: TypeName) -> bool {
        self.synthetic_classes.contains(&internal)
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

    /// Whether the class's declared type parameter `name` admits `null` — an unbounded `<T>` (implicitly
    /// `Any?`) or one whose every declared upper bound is nullable. kotlinc treats a value typed by a
    /// NON-null-bounded parameter as an ordinary non-null reference, so its field, accessors and
    /// constructor parameter carry `@NotNull` and its parameters are null-checked.
    ///
    /// Reads the RESOLVED bounds recorded in the class's generic signature; `Ty::upper_bound_admits_null`
    /// walks a bound that is itself a parameter (`<A : Cargo, B : A>`). `true` when the class declares no
    /// generic signature — a non-generic class has no such parameter to ask about.
    pub fn class_type_param_admits_null(&self, internal: &str, name: &str) -> bool {
        self.class_signatures
            .get(&crate::types::type_name(internal))
            .and_then(|signature| {
                signature
                    .type_params
                    .iter()
                    .find(|parameter| parameter.name == name)
            })
            .is_none_or(|parameter| {
                !parameter
                    .bounds
                    .iter()
                    .any(|(bound, _)| !bound.upper_bound_admits_null())
            })
    }
}

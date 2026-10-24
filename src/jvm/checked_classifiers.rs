//! One checked classifier inventory for JVM representation decisions.
//!
//! Executable-local and generated declarations belong to this IR file; all other declarations
//! belong to the frozen module/dependency snapshot. Neither consumer reopens source resolution.

use crate::backend::BackendClassifierSource;
use crate::libraries::ClassifierAccess;
use crate::types::TypeName;

pub(super) struct CheckedClassifiers<'a> {
    stable: &'a dyn BackendClassifierSource,
    file: crate::fx_hash::HashMap<TypeName, ClassifierFact>,
}

#[derive(Clone)]
pub(super) struct ClassifierFact {
    pub(super) nameable: bool,
    pub(super) interface: bool,
    pub(super) value_class: bool,
    pub(super) supertypes: Box<[TypeName]>,
}

impl<'a> CheckedClassifiers<'a> {
    pub(super) fn new(ir: &crate::ir::IrFile, stable: &'a dyn BackendClassifierSource) -> Self {
        let mut file = crate::fx_hash::HashMap::default();
        let mut published_by = crate::fx_hash::HashMap::default();
        for (class_id, class) in ir.classes.iter().enumerate().filter(|(_, class)| {
            !class.is_source_declared || crate::metadata::local_classifiers::is_local(ir, class)
        }) {
            let identity = class.fq_name_id();
            let mut supertypes = Vec::new();
            let mut publish = |supertype: TypeName| {
                if supertype != identity && !supertypes.contains(&supertype) {
                    supertypes.push(supertype);
                }
            };
            publish(class.superclass);
            for supertype in class
                .supertypes
                .iter()
                .filter_map(|ty| ty.non_null().obj_internal())
            {
                publish(supertype);
            }
            for interface in class.interfaces.iter_ids() {
                publish(interface);
            }
            let fact = ClassifierFact {
                nameable: !matches!(
                    ir.class_visibilities.get(&identity),
                    Some(crate::types::Visibility::PackagePrivate)
                ),
                interface: class.is_interface || class.is_annotation,
                value_class: class.is_value,
                supertypes: supertypes.into_boxed_slice(),
            };
            if let Some(previous) = published_by.insert(identity, class_id) {
                panic!(
                    "one common-IR file may publish one classifier per semantic identity: {identity} belongs to class {previous} and class {class_id}"
                );
            }
            file.insert(identity, fact);
        }
        Self { stable, file }
    }

    pub(super) fn classifier(&self, classifier: TypeName) -> Option<ClassifierFact> {
        if let Some(fact) = self.file.get(&classifier) {
            return Some(fact.clone());
        }
        self.stable
            .classifier(classifier)
            .map(|fact| ClassifierFact {
                nameable: fact.access != ClassifierAccess::PackagePrivate,
                interface: fact.is_interface(),
                value_class: fact.value_underlying.is_some(),
                supertypes: fact.supertypes.clone(),
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MissingClassifier(pub TypeName);

impl std::fmt::Display for MissingClassifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "missing backend classifier fact for {}", self.0)
    }
}

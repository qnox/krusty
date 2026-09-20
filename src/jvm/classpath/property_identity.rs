//! Declaration edges retained while classpath metadata is normalized into provider identities.

use super::Classpath;

/// JVM-owned realization of one provider-normalized Kotlin property. FIR carries only its opaque
/// identity; callers read the semantic name and independently interned physical accessors here.
#[derive(Clone, Debug)]
pub(crate) struct ExternalPropertyRealization {
    pub name: String,
    pub getter: crate::fir::ExternalCallableId,
    pub setter: Option<crate::fir::ExternalCallableId>,
    /// The provider-normalized declaration is its value class's underlying storage property.
    pub declares_value_class_storage: bool,
}

impl Classpath {
    /// Whether this exact getter realizes its value class's underlying storage property.
    ///
    /// Kotlin metadata publishes the property edge by string-table identity. At the provider
    /// boundary we attach that fact to the already-interned physical accessor identity once; later
    /// selection and realization never compare the property or reference-site spelling.
    pub(super) fn getter_declares_value_class_storage(
        &self,
        getter: crate::fir::ExternalCallableId,
    ) -> bool {
        self.external_callable(getter)
            .and_then(|realization| {
                let callable = realization.callable;
                self.find_name(callable.owner).map(|class| {
                    crate::jvm::metadata::class_properties(&class)
                        .iter()
                        .any(|property| {
                            property.is_inline_underlying
                                && property.getter.as_ref().is_some_and(|accessor| {
                                    accessor.name == callable.name
                                        && accessor.desc == callable.descriptor
                                })
                        })
                })
            })
            .unwrap_or(false)
    }
}

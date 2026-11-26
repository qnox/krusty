//! Recognition of a call that reads a value class's underlying property.

use krusty::fir::{ExternalCallableId, ResolvedPropertyOverrideTarget};
use krusty::fx_hash::{HashMap, HashSet};
use krusty::ir::{IrFile, IrVirtualTarget};
use krusty::jvm::names::property_getter_name;
use krusty::types::TypeName;

/// The getters that read each value class's underlying property, which is the carrier itself
/// rather than a member of the boxed class. A source class's getter is its plain spelling. A
/// dependency getter is the exact declaration its provider normalized as the class's storage,
/// because a getter typed as another value class is mangled with a hash, and an ordinary function
/// or a context-parameter getter mangles to the same prefix.
pub(super) struct UnderlyingGetters<'a> {
    plain: HashMap<TypeName, String>,
    storage: &'a HashSet<ExternalCallableId>,
}

impl<'a> UnderlyingGetters<'a> {
    pub(super) fn new(
        ir: &IrFile,
        external: &HashMap<TypeName, String>,
        storage: &'a HashSet<ExternalCallableId>,
    ) -> Self {
        let mut plain: HashMap<TypeName, String> = ir
            .classes
            .iter()
            .filter(|class| class.is_value)
            .filter_map(|class| {
                class
                    .fields
                    .first()
                    .map(|field| (class.fq_name, property_getter_name(&field.name)))
            })
            .collect();
        plain.extend(
            external
                .iter()
                .map(|(owner, property)| (*owner, property_getter_name(property))),
        );
        Self { plain, storage }
    }

    /// Whether the virtual call `owner.name` selected by `target` reads the underlying property.
    pub(super) fn reads(
        &self,
        owner: &TypeName,
        name: &str,
        target: Option<IrVirtualTarget>,
    ) -> bool {
        self.plain.get(owner).is_some_and(|getter| getter == name)
            || (self.plain.contains_key(owner)
                && matches!(
                    target,
                    Some(IrVirtualTarget::PropertyGetter(
                        ResolvedPropertyOverrideTarget::External(id)
                    )) if self.storage.contains(&id)
                ))
    }
}

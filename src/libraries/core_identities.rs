//! Stable dependency identities for the core provider's declarations.
//!
//! The core provider ([`super::EmptySymbolSource`]) publishes real declarations: `Any`'s members,
//! `String.length`, `Array.size`, `Enum.name`, and the few callables beside them. Checked FIR
//! carries every selected dependency declaration as an opaque [`ExternalCallableId`], so this
//! provider assigns one to each declaration it publishes, exactly as a classpath or KLIB provider
//! does, and answers for it at the backend boundary. Without an identity a selection of one of
//! these declarations could only travel as a special operation, which is what FIR must not hold.
//!
//! The provider itself is stateless and rebuilds a declaration on every query, so its registry is
//! shared by the process: the same declaration key always receives the same identity, and an
//! identity is only ever answered by this provider.

use std::sync::{LazyLock, Mutex};

use crate::fir::{ExternalCallableId, ExternalPropertyId};
use crate::fx_hash::HashMap;
use crate::types::{SemanticCallableOwner, Ty, TypeName};

use super::{
    Callables, ExternalCallableKind, ExternalCallableRealization, ExternalPropertyRealization,
    FnKind, LibraryCallable, LibraryType, PropKind, PropertyInfo,
};

/// The exact declaration a core identity names: its owner, physical shape, kind, and the
/// semantic namespace that declares it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CoreCallableKey {
    owner: TypeName,
    name: String,
    physical_params: Vec<Ty>,
    physical_ret: Ty,
    kind: ExternalCallableKind,
    declaration_owner: Option<SemanticCallableOwner>,
}

#[derive(Default)]
struct CoreRegistry {
    callables: Vec<ExternalCallableRealization>,
    callable_ids: HashMap<CoreCallableKey, ExternalCallableId>,
    properties: Vec<ExternalPropertyRealization>,
    property_ids: HashMap<(ExternalCallableId, Option<ExternalCallableId>), ExternalPropertyId>,
}

static REGISTRY: LazyLock<Mutex<CoreRegistry>> = LazyLock::new(Mutex::default);

fn registry() -> std::sync::MutexGuard<'static, CoreRegistry> {
    REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl CoreRegistry {
    fn callable(
        &mut self,
        callable: &LibraryCallable,
        kind: ExternalCallableKind,
        declaration_owner: Option<SemanticCallableOwner>,
    ) -> ExternalCallableId {
        let key = CoreCallableKey {
            owner: callable.owner,
            name: callable.physical_name().to_string(),
            physical_params: callable.physical_params.clone(),
            physical_ret: callable.physical_ret,
            kind,
            declaration_owner,
        };
        if let Some(identity) = self.callable_ids.get(&key) {
            return *identity;
        }
        let identity = ExternalCallableId::from_raw(
            u32::try_from(self.callables.len())
                .expect("the core provider declares fewer than u32::MAX callables"),
        );
        let mut stored = callable.clone();
        stored.external_identity = Some(identity);
        stored.declaration_owner = declaration_owner;
        self.callables.push(ExternalCallableRealization {
            callable: stored,
            kind,
            declaration_owner,
            parameter_identities: Box::new([]),
            declaration_signature: None,
            dependency_callable: None,
            direct_c_function: None,
            klib_body_companion_receiver: None,
            klib_body_type_parameters: Box::new([]),
            klib_body_declaration_modifiers: None,
            klib_body_class_contract: None,
            klib_body_accessor: None,
            klib_body_vararg_index: None,
            klib_body_parameter_defaults: None,
        });
        self.callable_ids.insert(key, identity);
        identity
    }

    fn property(
        &mut self,
        name: &str,
        getter: ExternalCallableId,
        setter: Option<ExternalCallableId>,
        compile_time_constant: Option<super::LibraryConst>,
    ) -> ExternalPropertyId {
        if let Some(identity) = self.property_ids.get(&(getter, setter)) {
            return *identity;
        }
        let identity = ExternalPropertyId::from_raw(
            u32::try_from(self.properties.len())
                .expect("the core provider declares fewer than u32::MAX properties"),
        );
        self.properties.push(ExternalPropertyRealization {
            name: name.to_string(),
            getter,
            setter,
            declares_value_class_storage: false,
            compile_time_constant,
        });
        // The accessors' own realizations name the property they belong to, as the accessors
        // the provider publishes do.
        for accessor in std::iter::once(getter).chain(setter) {
            self.callables[accessor.raw() as usize]
                .callable
                .external_property_identity = Some(identity);
        }
        self.property_ids.insert((getter, setter), identity);
        identity
    }
}

fn callable_kind(kind: FnKind) -> ExternalCallableKind {
    match kind {
        FnKind::TopLevel => ExternalCallableKind::TopLevel,
        FnKind::Extension => ExternalCallableKind::Extension,
        FnKind::Member => ExternalCallableKind::Member,
    }
}

fn property_kind(kind: PropKind) -> ExternalCallableKind {
    match kind {
        PropKind::TopLevel => ExternalCallableKind::TopLevel,
        PropKind::Extension => ExternalCallableKind::Extension,
        PropKind::Member | PropKind::MemberExtension => ExternalCallableKind::Member,
    }
}

fn register_property(
    registry: &mut CoreRegistry,
    property: &mut PropertyInfo,
    declaration_owner: SemanticCallableOwner,
) {
    let kind = property_kind(property.kind);
    let owner = Some(declaration_owner);
    let getter = registry.callable(&property.getter, kind, owner);
    property.getter.external_identity = Some(getter);
    property.getter.declaration_owner = owner;
    let setter = property.setter.as_mut().map(|setter| {
        let identity = registry.callable(setter, kind, owner);
        setter.external_identity = Some(identity);
        setter.declaration_owner = owner;
        identity
    });
    let identity = registry.property(
        &property.name,
        getter,
        setter,
        property.compile_time_constant.clone(),
    );
    property.getter.external_property_identity = Some(identity);
    if let Some(setter) = &mut property.setter {
        setter.external_property_identity = Some(identity);
    }
}

fn register_callables(
    registry: &mut CoreRegistry,
    callables: Callables,
    declaration_owner: SemanticCallableOwner,
) -> Callables {
    let (mut functions, mut properties) = callables.into_parts();
    for function in &mut functions.overloads {
        let owner = Some(declaration_owner);
        let identity = registry.callable(&function.callable, callable_kind(function.kind), owner);
        function.callable.external_identity = Some(identity);
        function.callable.declaration_owner = owner;
    }
    for property in &mut properties.overloads {
        register_property(registry, property, declaration_owner);
    }
    Callables::from_parts(functions, properties)
}

/// Assign identities to the callables a core package declares.
pub(super) fn register_package_callables(package: TypeName, callables: Callables) -> Callables {
    register_callables(
        &mut registry(),
        callables,
        SemanticCallableOwner::Package(package),
    )
}

/// Assign identities to every callable a core classifier declares.
pub(super) fn register_classifier(classifier: &mut LibraryType, internal: TypeName) {
    let mut registry = registry();
    let owner = SemanticCallableOwner::Classifier(internal);
    for callables in classifier.declared_callables.values_mut() {
        *callables = register_callables(&mut registry, std::mem::take(callables), owner);
    }
}

/// The realization of a core declaration this provider identified.
pub(super) fn external_callable(
    identity: ExternalCallableId,
) -> Option<ExternalCallableRealization> {
    registry().callables.get(identity.raw() as usize).cloned()
}

/// The property a core identity names.
pub(super) fn external_property(
    identity: ExternalPropertyId,
) -> Option<ExternalPropertyRealization> {
    registry().properties.get(identity.raw() as usize).cloned()
}

//! How a source property was materialized in common IR: its storage and accessor declarations.

use super::{ClassId, FunId};
use crate::types::{Ty, TypeName};

/// Common-IR declaration layout for a source property. This records which semantic storage and
/// accessor declarations were materialized, but deliberately does not choose how an ordinary
/// property access uses them. That choice belongs to the target realization pass.
#[derive(Clone, Debug)]
pub enum IrLocalPropertyLayout {
    TopLevelStorage {
        storage: u32,
        getter: Option<FunId>,
        setter: Option<FunId>,
        /// Semantic singleton qualifier for a classifier-associated constant. `None` denotes a
        /// genuinely receiverless package property.
        qualifier: Option<TypeName>,
    },
    TopLevelAccessor {
        getter: FunId,
        setter: Option<FunId>,
        receiver: Option<Ty>,
        context_parameters: Vec<Ty>,
        /// The `x$delegate` static a delegated property's accessors read, which its
        /// `JvmPropertySignature` names as the property's field. `None` for a computed property.
        delegate: Option<u32>,
    },
    Member {
        class: ClassId,
        owner: TypeName,
        backing_field: Option<u32>,
        getter: Option<FunId>,
        setter: Option<FunId>,
        interface: bool,
        name: String,
        ty: Ty,
        mutable: bool,
        private: bool,
        context_parameters: Vec<Ty>,
        property: u32,
    },
    MemberExtension {
        owner: TypeName,
        interface: bool,
        name: String,
        getter: FunId,
        setter: Option<FunId>,
        receiver: Ty,
        ty: Ty,
        context_parameters: Vec<Ty>,
    },
}

impl IrLocalPropertyLayout {
    /// The accessor functions this layout materialized.
    pub(crate) fn accessor_functions(&self) -> impl Iterator<Item = FunId> {
        let (getter, setter) = match self {
            Self::TopLevelStorage { getter, setter, .. } | Self::Member { getter, setter, .. } => {
                (*getter, *setter)
            }
            Self::TopLevelAccessor { getter, setter, .. }
            | Self::MemberExtension { getter, setter, .. } => (Some(*getter), *setter),
        };
        getter.into_iter().chain(setter)
    }
}

impl super::IrFile {
    /// The selected declaration's layout at this operation's recorded dispatch classifier.
    /// An anonymous-class specialization preserves property order and clones its accessors;
    /// its source chain authenticates that projection without resolving a property by spelling.
    pub(crate) fn property_layout_at(
        &self,
        operation: super::ExprId,
        target: crate::fir::PropertyId,
    ) -> Result<Option<IrLocalPropertyLayout>, crate::fir::PropertyId> {
        let Some(layout) = self.local_property_layouts.get(&target).cloned() else {
            return Ok(None);
        };
        if !matches!(layout, IrLocalPropertyLayout::Member { .. }) {
            return Ok(Some(layout));
        }
        let Some(dispatch) = self.dispatch_classes.get(&operation) else {
            return Ok(Some(layout));
        };
        let actual = self.class_id_by_name(*dispatch).ok_or(target)?;
        self.property_layout_for_class(target, actual)
    }

    /// The same projection while a copied construction publishes its dispatch-class fact.
    pub(crate) fn property_layout_for_class(
        &self,
        target: crate::fir::PropertyId,
        actual: super::ClassId,
    ) -> Result<Option<IrLocalPropertyLayout>, crate::fir::PropertyId> {
        let Some(mut layout) = self.local_property_layouts.get(&target).cloned() else {
            return Ok(None);
        };
        let IrLocalPropertyLayout::Member {
            class, property, ..
        } = &layout
        else {
            return Ok(Some(layout));
        };
        let (declared, index) = (*class, *property);
        self.classes.get(actual as usize).ok_or(target)?;
        if actual != declared && !self.specialized_anonymous_classes.contains_key(&actual) {
            // An ordinary inherited property retains its selected declaration's layout.
            return Ok(Some(layout));
        }
        let mut current = actual;
        let mut seen = crate::fx_hash::HashSet::default();
        while current != declared {
            if !seen.insert(current) {
                return Err(target);
            }
            let Some(copy) = self.specialized_anonymous_classes.get(&current) else {
                return if self.property_owner_is_supertype(current, declared) {
                    Ok(Some(layout))
                } else {
                    Err(target)
                };
            };
            self.classes.get(copy.source as usize).ok_or(target)?;
            current = copy.source;
        }
        if actual == declared {
            return Ok(Some(layout));
        }
        let class = self.classes.get(actual as usize).ok_or(target)?;
        let copied = class.properties.get(index as usize).ok_or(target)?;
        let IrLocalPropertyLayout::Member {
            class: owner_class,
            owner,
            backing_field,
            getter,
            setter,
            name,
            ty,
            mutable,
            private,
            ..
        } = &mut layout
        else {
            unreachable!()
        };
        *owner_class = actual;
        *owner = class.fq_name;
        *backing_field = copied.backing_field;
        *getter = copied.getter;
        *setter = copied.setter;
        *name = copied.name.clone();
        *ty = copied.ty;
        *mutable = copied.is_var;
        *private = copied.is_private;
        Ok(Some(layout))
    }

    fn property_owner_is_supertype(
        &self,
        source: super::ClassId,
        declared: super::ClassId,
    ) -> bool {
        let mut pending = vec![source];
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some(class) = pending.pop() {
            if class == declared {
                return true;
            }
            if !seen.insert(class) {
                continue;
            }
            let Some(class) = self.classes.get(class as usize) else {
                continue;
            };
            pending.extend(
                std::iter::once(class.superclass)
                    .chain(class.interfaces.iter())
                    .chain(
                        class
                            .supertypes
                            .iter()
                            .copied()
                            .filter_map(Ty::obj_internal),
                    )
                    .filter_map(|name| self.class_id_by_name(name)),
            );
        }
        false
    }
}

#[cfg(test)]
mod tests;

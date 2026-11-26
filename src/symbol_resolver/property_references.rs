//! The callable-reference view of a selected property: its accessors, reflection owner and value
//! type, built from a member or extension property declaration.

use super::*;

#[derive(Clone, Debug)]
pub struct ResolvedPropertyRef {
    pub name: String,
    pub getter: LibraryCallable,
    pub setter: Option<LibraryCallable>,
    pub getter_visibility: crate::types::Visibility,
    pub setter_visibility: crate::types::Visibility,
    /// Semantic receiver written at the reference site. The physical accessor may be inherited from
    /// a different `getter.owner`; any platform reflection carrier is an emission concern.
    pub reflection_owner: Ty,
    pub prop_ty: Ty,
    pub source_key: Option<(u32, u32)>,
    /// Stable declaration identity for a property from the current compilation. Parser-arena
    /// source keys remain a transient resolver detail and must not cross into checked FIR.
    pub stable_declaration: Option<crate::fir::DeclarationId>,
    /// `None` for an instance property; `Some(None)` for a same-file extension property;
    /// `Some(Some(owner))` for an extension property emitted on another facade.
    pub extension_facade: Option<Option<TypeName>>,
    /// Leading context parameters of the declaration's accessors. A reference to a declaration
    /// that has them can only be formed where `CallableReferencesToContextual` binds them.
    pub context_count: usize,
    /// Source names of those context parameters, for diagnostics only.
    pub context_names: Vec<String>,
    /// A Java bean property derived from a getter: it ranks below the member functions of its name.
    pub java_synthetic: bool,
}

impl ResolvedPropertyRef {
    /// The declared types of the leading context parameters. An extension property's accessors
    /// list their receiver first in the semantic model, the contexts after it.
    pub fn context_types(&self) -> &[Ty] {
        let start = usize::from(self.extension_facade.is_some());
        self.getter
            .params
            .get(start..start + self.context_count)
            .unwrap_or_default()
    }
}

pub(super) fn select_extension_property_ref(property: PropertyInfo) -> Option<ResolvedPropertyRef> {
    let name = property.name;
    let source_key = property.source_key;
    let getter_visibility = property.visibility;
    let setter_visibility = property.setter_visibility;
    let getter = property.getter;
    crate::trace_compiler!(
        "callable_ref",
        "extension property target getter={}.{}{} params={:?} suspend={} default_call={} source={source_key:?}",
        getter.owner,
        getter.name,
        getter.descriptor,
        getter.params,
        getter.suspend,
        getter.default_call,
    );
    let context_count = property.context_count;
    let context_names = property.context_param_names;
    if getter.suspend || getter.default_call || getter.params.len() != context_count + 1 {
        return None;
    }
    let setter = property.setter.filter(|setter| {
        setter.params.len() == context_count + 2 && setter.physical_ret == Ty::Unit
    });
    let prop_ty = getter.ret;
    let reflection_owner = *getter.params.first()?;
    let facade = (!getter.owner.matches("")).then_some(getter.owner);
    Some(ResolvedPropertyRef {
        name,
        extension_facade: Some(facade),
        getter,
        setter: setter.map(|setter| *setter),
        getter_visibility,
        setter_visibility,
        reflection_owner,
        prop_ty,
        source_key,
        stable_declaration: property.stable_declaration,
        context_count,
        context_names,
        java_synthetic: false,
    })
}

pub(super) fn build_property_reference_from_declaration(
    recv: Ty,
    name: &str,
    property: &PropertyInfo,
) -> Option<ResolvedPropertyRef> {
    if property.getter.suspend {
        crate::trace_compiler!(
            "resolve",
            "property ref rejected {recv:?}.{name}: suspend getter"
        );
        return None;
    }
    let prop_ty = property.ty;
    let getter_visibility = property.visibility;
    let setter_visibility = property.setter_visibility;
    let getter = property.getter.clone();
    let setter = property.setter.clone().filter(|setter| {
        setter.params.len() == property.context_count + 1 && setter.physical_ret == Ty::Unit
    });
    if getter.params.len() != property.context_count {
        crate::trace_compiler!(
            "resolve",
            "property ref rejected {recv:?}.{name}: getter {}.{}{} is not a plain zero-argument accessor",
            getter.owner.render(),
            getter.name,
            getter.descriptor,
        );
        return None;
    }
    Some(ResolvedPropertyRef {
        name: property.name.clone(),
        getter,
        setter: setter.map(|setter| *setter),
        getter_visibility,
        setter_visibility,
        reflection_owner: recv,
        prop_ty,
        source_key: property.source_key,
        stable_declaration: property.stable_declaration,
        extension_facade: None,
        context_count: property.context_count,
        context_names: property.context_param_names.clone(),
        java_synthetic: property.accessor_derived(),
    })
}

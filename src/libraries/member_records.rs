//! The flat, call-facing member view of a classifier record.
//!
//! A classifier publishes its member functions in [`LibraryType::declared_callables`]. A provider
//! that decodes flat member records first (a classfile provider) also keeps those records in
//! [`LibraryType::members`]. A provider that builds declarations directly publishes each method
//! once, as its declaration, and sets [`LibraryType::methods_as_declarations`]; the flat record of
//! such a method is projected from the declaration when a consumer asks for it, so the record set
//! never holds a second copy of every method.

use std::borrow::Cow;

use super::{member_record, FnKind, FunctionInfo, LibraryMember, LibraryType};

impl LibraryType {
    /// Every flat member record: the stored [`Self::members`], then the ordinary methods that live
    /// only as declarations, projected in declaration order. A deprecated-hidden declaration stays
    /// out of the flat view, as it does for every provider.
    pub fn member_records(&self) -> impl Iterator<Item = Cow<'_, LibraryMember>> {
        let projected = self
            .methods_as_declarations
            .then(|| {
                self.declared_callable_order.iter().flat_map(|name| {
                    self.declared_callables
                        .get(name)
                        .into_iter()
                        .flat_map(|callables| callables.functions())
                })
            })
            .into_iter()
            .flatten()
            .filter(|function| projected_method(function))
            .map(|function| Cow::Owned(member_record(function)));
        self.members.iter().map(Cow::Borrowed).chain(projected)
    }

    /// The flat member records spelled `name`; see [`Self::member_records`].
    pub fn member_records_named<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = Cow<'a, LibraryMember>> {
        let projected = self
            .methods_as_declarations
            .then(|| self.declared_callables.get(name))
            .flatten()
            .into_iter()
            .flat_map(|callables| callables.functions())
            .filter(|function| projected_method(function))
            .map(|function| Cow::Owned(member_record(function)));
        self.members
            .iter()
            .filter(move |member| member.name == name)
            .map(Cow::Borrowed)
            .chain(projected)
    }

    /// Whether a flat member record is spelled `name`, by its source or physical name. Answers from
    /// the declarations without projecting a record.
    pub fn has_member_record_spelled(&self, name: &str) -> bool {
        let spelled =
            |source: &str, physical: Option<&str>| source == name || physical == Some(name);
        self.members
            .iter()
            .any(|member| spelled(&member.name, member.physical_name.as_deref()))
            || (self.methods_as_declarations
                && self
                    .declared_callables
                    .values()
                    .flat_map(|callables| callables.functions())
                    .filter(|function| projected_method(function))
                    .any(|function| {
                        spelled(
                            &function.callable.name,
                            function.callable.physical_name.as_deref(),
                        )
                    }))
    }
}

/// An ordinary method a declaration-publishing provider keeps only as its declaration. Member
/// extensions keep their stored record: their flat shape carries the second receiver.
fn projected_method(function: &FunctionInfo) -> bool {
    function.kind == FnKind::Member && !function.flags.deprecated_hidden
}

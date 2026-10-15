//! Source availability of decoded JVM declarations.
//!
//! Binary-compatibility declarations remain in declaration-facing records, while ordinary lookup
//! withholds `@Deprecated(HIDDEN)` and declarations newer than the selected Kotlin API version.

use super::{mapped_builtin_member_status, JvmLibraries, MappedBuiltinMemberStatus};
use crate::language_version::LanguageVersion;
use crate::types::TypeName;

impl JvmLibraries {
    /// Select which `@SinceKotlin` declarations this compilation may call.
    pub fn with_api_version(mut self, api_version: LanguageVersion) -> Self {
        self.api_version = api_version;
        self
    }

    /// Both forms are absent from overload resolution. Kotlinc reports the missing callable as an
    /// unresolved reference and omits the receiver type when it was the applicable declaration.
    pub(super) fn hides_callable(
        &self,
        deprecated_hidden: bool,
        hidden_since: Option<LanguageVersion>,
        since_kotlin: Option<LanguageVersion>,
    ) -> bool {
        self.deprecated_hidden_at_api(deprecated_hidden, hidden_since)
            || self.since_kotlin_withheld(since_kotlin)
    }

    pub(super) fn hides_function(&self, function: &crate::jvm::metadata::MetaFn) -> bool {
        self.function_hidden_at_api(function) || self.since_kotlin_withheld(function.since_kotlin())
    }

    pub(super) fn function_hidden_at_api(&self, function: &crate::jvm::metadata::MetaFn) -> bool {
        self.deprecated_hidden_at_api(function.deprecated_hidden(), function.hidden_since())
    }

    pub(in crate::jvm) fn property_hidden_at_api(
        &self,
        property: &crate::jvm::metadata::MetaProp,
    ) -> bool {
        self.deprecated_hidden_at_api(property.deprecated_hidden, property.hidden_since)
    }

    pub(super) fn constructor_hidden_at_api(
        &self,
        constructor: &crate::jvm::metadata::MetaConstructor,
    ) -> bool {
        self.deprecated_hidden_at_api(constructor.deprecated_hidden, constructor.hidden_since)
    }

    pub(super) fn visible_constructor_params(
        &self,
        constructors: &[crate::jvm::metadata::MetaConstructor],
    ) -> Vec<crate::libraries::ParamList> {
        constructors
            .iter()
            .filter(|constructor| !self.constructor_hidden_at_api(constructor))
            .map(|constructor| constructor.params.clone())
            .collect()
    }

    pub(super) fn deprecated_hidden_at_api(
        &self,
        deprecated_hidden: bool,
        hidden_since: Option<LanguageVersion>,
    ) -> bool {
        deprecated_hidden || hidden_since.is_some_and(|since| since <= self.api_version)
    }

    /// Remove hidden properties while retaining the declaration name for import resolution.
    pub(super) fn withhold_hidden_declarations(
        &self,
        package: Option<TypeName>,
        name: &str,
        properties: &mut Vec<crate::libraries::PropertyInfo>,
    ) -> bool {
        let hidden_property = properties.iter().any(|property| property.deprecated_hidden);
        properties.retain(|property| !property.deprecated_hidden);
        let hidden_function = package.is_some_and(|package| {
            self.cp
                .package_facades_name(package)
                .into_iter()
                .any(|facade| {
                    self.cp.meta_functions_name(facade).iter().any(|function| {
                        function.kotlin_name == name
                            && self.function_hidden_at_api(function)
                            && !self.since_kotlin_withheld(function.since_kotlin())
                    })
                })
        });
        hidden_property || hidden_function
    }

    pub(super) fn since_kotlin_withheld(&self, since_kotlin: Option<LanguageVersion>) -> bool {
        since_kotlin.is_some_and(|since| since > self.api_version)
    }

    pub(super) fn note_api_withheld(&self, owner: TypeName, name: &str) {
        self.api_withheld
            .borrow_mut()
            .entry(owner)
            .or_default()
            .insert(name.to_string());
    }

    /// Names retained only for override/binary-compatibility matching on this classifier.
    pub(super) fn hidden_deprecated_callables(
        &self,
        internal: TypeName,
        class: &crate::jvm::classreader::ClassInfo,
        functions: &[crate::jvm::metadata::MetaFn],
        members: &[crate::libraries::LibraryMember],
        kotlin_scope_is_authoritative: bool,
    ) -> crate::fx_hash::HashSet<String> {
        let mut hidden = functions
            .iter()
            .filter(|declaration| {
                self.function_hidden_at_api(declaration)
                    && !self.since_kotlin_withheld(declaration.since_kotlin())
            })
            .map(|declaration| declaration.kotlin_name.clone())
            .collect::<crate::fx_hash::HashSet<_>>();
        hidden.extend(
            crate::jvm::metadata::class_properties(class)
                .iter()
                .filter(|property| self.property_hidden_at_api(property))
                .map(|property| property.name.clone()),
        );
        if kotlin_scope_is_authoritative {
            hidden.extend(
                members
                    .iter()
                    .filter(|member| {
                        mapped_builtin_member_status(internal, class.this_class, member)
                            == MappedBuiltinMemberStatus::DeprecatedHidden
                    })
                    .map(|member| member.name.clone()),
            );
        }
        hidden
    }
}

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
        since_kotlin: Option<LanguageVersion>,
    ) -> bool {
        deprecated_hidden || self.since_kotlin_withheld(since_kotlin)
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
        functions: &crate::jvm::classpath::MetaFns,
        members: &[crate::libraries::LibraryMember],
        kotlin_scope_is_authoritative: bool,
    ) -> std::collections::HashSet<String> {
        let mut hidden = functions
            .iter()
            .filter(|declaration| {
                declaration.deprecated_hidden()
                    && !self.since_kotlin_withheld(declaration.since_kotlin())
            })
            .map(|declaration| declaration.kotlin_name.clone())
            .collect::<std::collections::HashSet<_>>();
        hidden.extend(
            crate::jvm::metadata::class_properties(class)
                .iter()
                .filter(|property| property.deprecated_hidden)
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

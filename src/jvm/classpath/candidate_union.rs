//! Composition of callable candidates contributed by multiple classpath entries.
//!
//! Each entry normalizes its own declarations into [`ExtCandidate`]. This boundary preserves
//! classpath/package declaration order while admitting each physical JVM callable once, even when
//! the same library is present through repeated or distinct paths.

use std::collections::HashSet;

use crate::types::TypeName;

use super::{descriptor_parts, Classpath, ExtCandidate};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PhysicalCallableIdentity {
    owner: TypeName,
    name: String,
    descriptor: String,
}

#[derive(Default)]
struct CallableUnion {
    seen: HashSet<PhysicalCallableIdentity>,
    candidates: Vec<ExtCandidate>,
}

impl CallableUnion {
    fn push(&mut self, candidate: ExtCandidate) {
        let identity = PhysicalCallableIdentity {
            owner: candidate.owner,
            name: candidate.name.clone(),
            descriptor: candidate.descriptor.clone(),
        };
        if self.seen.insert(identity) {
            self.candidates.push(candidate);
        }
    }

    fn extend(&mut self, candidates: impl IntoIterator<Item = ExtCandidate>) {
        for candidate in candidates {
            self.push(candidate);
        }
    }

    fn finish(self) -> Vec<ExtCandidate> {
        self.candidates
    }
}

pub(super) fn extensions_in_scope(
    classpath: &Classpath,
    recv_desc: &str,
    jvm_name: &str,
    packages: &[TypeName],
) -> Vec<ExtCandidate> {
    let tree = classpath.package_tree();
    let mut union = CallableUnion::default();
    let mut seen_packages = HashSet::new();
    for &package in packages {
        if !seen_packages.insert(package) {
            continue;
        }
        let Some(node) = tree.node_for_name(package) else {
            continue;
        };
        for &entry in &node.jars {
            if classpath.entries.get(entry).is_none() {
                continue;
            }
            let members = classpath.jar_pkg_members_name(entry, package);
            let Some(indices) = members.by_jvm.get(jvm_name) else {
                continue;
            };
            for &index in indices {
                let Some(candidate) = members.candidates.get(index) else {
                    continue;
                };
                if descriptor_parts(&candidate.descriptor)
                    .and_then(|(first_parameter, _)| first_parameter)
                    .as_deref()
                    == Some(recv_desc)
                {
                    union.push(candidate.render(&members.owner_names));
                }
            }
        }
    }
    union.finish()
}

pub(super) fn functions_in_scope(
    classpath: &Classpath,
    name: &str,
    packages: &[TypeName],
) -> Vec<ExtCandidate> {
    let tree = classpath.package_tree();
    let mut union = CallableUnion::default();
    let mut seen_packages = HashSet::new();
    for &package in packages {
        if !seen_packages.insert(package) {
            continue;
        }
        let Some(node) = tree.node_for_name(package) else {
            continue;
        };
        for &entry in &node.jars {
            if classpath.entries.get(entry).is_none() {
                continue;
            }
            let members = classpath.jar_pkg_members_name(entry, package);
            if let Some(indices) = members.by_source.get(name) {
                union.extend(members.render_indices(indices));
            }
        }
    }
    union.finish()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::types::type_name;

    use super::{CallableUnion, Classpath, ExtCandidate};

    fn candidate(owner: &str, name: &str, descriptor: &str) -> ExtCandidate {
        ExtCandidate {
            owner: type_name(owner),
            name: name.to_string(),
            descriptor: descriptor.to_string(),
            ret_desc: descriptor
                .split_once(')')
                .expect("test descriptor")
                .1
                .to_string(),
            signature: None,
            public: true,
        }
    }

    #[test]
    fn physical_identity_deduplicates_without_reordering() {
        let first = candidate("sample/AKt", "pick", "(I)I");
        let second = candidate("sample/BKt", "pick", "(I)I");
        let mut union = CallableUnion::default();
        union.extend([first.clone(), first, second.clone(), second.clone()]);

        let candidates = union.finish();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].owner, type_name("sample/AKt"));
        assert_eq!(candidates[1].owner, type_name("sample/BKt"));
    }

    #[test]
    fn overloads_and_distinct_owners_remain_distinct() {
        let mut union = CallableUnion::default();
        union.extend([
            candidate("sample/AKt", "pick", "(I)I"),
            candidate("sample/AKt", "pick", "(J)I"),
            candidate("sample/BKt", "pick", "(I)I"),
        ]);

        let candidates = union.finish();
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].descriptor, "(I)I");
        assert_eq!(candidates[1].descriptor, "(J)I");
        assert_eq!(candidates[2].owner, type_name("sample/BKt"));
    }

    fn identities(candidates: Vec<ExtCandidate>) -> Vec<(crate::types::TypeName, String, String)> {
        candidates
            .into_iter()
            .map(|candidate| (candidate.owner, candidate.name, candidate.descriptor))
            .collect()
    }

    #[test]
    fn copied_entry_matches_the_single_entry_candidate_sequences() {
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let stdlib = crate::toolchain::stdlib_jar().expect("test requires the Kotlin stdlib");
        let directory = std::env::temp_dir().join(format!(
            "krusty-callable-union-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).expect("create copied-entry test directory");
        let copied = directory.join("kotlin-stdlib-copy.jar");
        std::fs::copy(&stdlib, &copied).expect("copy stdlib to a distinct classpath path");

        let single = Classpath::new(vec![stdlib.clone()]);
        let duplicated = Classpath::new(vec![stdlib, copied]);
        let packages = [type_name("kotlin/collections")];
        let single_dependency =
            identities(single.functions_in_scope("throwIndexOverflow", &packages));
        let duplicated_dependency =
            identities(duplicated.functions_in_scope("throwIndexOverflow", &packages));
        let single_extension = identities(single.extensions_in_scope(
            "Ljava/lang/Iterable;",
            "forEachIndexed",
            &packages,
        ));
        let duplicated_extension = identities(duplicated.extensions_in_scope(
            "Ljava/lang/Iterable;",
            "forEachIndexed",
            &packages,
        ));
        std::fs::remove_dir_all(directory).expect("remove copied-entry test directory");

        assert!(!single_dependency.is_empty());
        assert!(!single_extension.is_empty());
        assert_eq!(duplicated_dependency, single_dependency);
        assert_eq!(duplicated_extension, single_extension);
    }
}

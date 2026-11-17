use crate::TypeName;
use krusty_names::type_name;

/// The classifier identities a qualified name written as `a.b.C.D` can denote. The written form
/// does not say where the package ends, and kotlinc compares it with each annotation's qualified
/// name, so every package/classifier split is a candidate (`a/b/C/D`, `a/b/C$D`, …). A candidate
/// no declaration has matches nothing. kotlinc's annotation-driven plugins share this reading of
/// their `annotation` option.
pub fn classifier_identities(qualified: &str) -> Vec<TypeName> {
    let segments = qualified.split('.').collect::<Vec<_>>();
    (1..=segments.len())
        .map(|package_len| {
            let (package, classifiers) = segments.split_at(package_len - 1);
            let mut path = package.join("/");
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(classifiers[0]);
            classifiers[1..]
                .iter()
                .fold(type_name(&path), |owner, nested| owner.nested_child(nested))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_qualified_name_denotes_every_package_and_nested_classifier_split() {
        assert_eq!(
            classifier_identities("a.B.C"),
            vec![
                type_name("a").nested_child("B").nested_child("C"),
                type_name("a/B").nested_child("C"),
                type_name("a/B/C"),
            ]
        );
    }
}

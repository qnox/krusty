use crate::{PluginOption, TypeName};
use krusty_core::type_name;

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

/// A plugin's named presets: each is a list of qualified annotation names
/// (kotlinc's `SUPPORTED_PRESETS`).
pub type Presets = &'static [(&'static str, &'static [&'static str])];

/// The annotations a compilation's options configure for an annotation-driven plugin, in either
/// option syntax: every `annotation=<fqname>`, then the annotations of every `preset=<name>`. Like
/// kotlinc, an unknown preset name contributes nothing.
pub fn annotations_from_options(options: &[PluginOption], presets: Presets) -> Vec<TypeName> {
    let mut qualified = Vec::new();
    let mut chosen = Vec::new();
    for option in options {
        match option.key.as_str() {
            "annotation" => qualified.push(option.value.as_str()),
            "preset" => chosen.push(option.value.as_str()),
            _ => {}
        }
    }
    for preset in chosen {
        if let Some((_, names)) = presets.iter().find(|(name, _)| *name == preset) {
            qualified.extend(names.iter().copied());
        }
    }
    qualified
        .into_iter()
        .flat_map(classifier_identities)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(key: &str, value: &str) -> PluginOption {
        PluginOption {
            id: "test".to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn annotations_come_from_the_option_then_the_known_presets_in_order() {
        const PRESETS: Presets = &[("p", &["x.Preset"])];
        assert_eq!(
            annotations_from_options(
                &[
                    option("preset", "p"),
                    option("annotation", "a.A"),
                    option("preset", "unknown"),
                    option("other", "ignored"),
                ],
                PRESETS,
            ),
            [
                classifier_identities("a.A"),
                classifier_identities("x.Preset")
            ]
            .concat()
        );
    }

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

//! Exact descriptor-text mention indexing for `InnerClasses` retention.

/// Record every name a `contains("L<name>;")` search over `value` could have matched.
///
/// This is deliberately not a JVM descriptor parser. It preserves the literal predicate used by
/// `InnerClasses` retention, including runs that begin at an `L` inside another name and text that
/// is not itself a valid descriptor.
pub(super) fn record_mentioned_names(
    value: &str,
    names: &mut crate::name_tree::FxHashMap<String, ()>,
) {
    for (index, byte) in value.as_bytes().iter().enumerate() {
        if *byte != b'L' {
            continue;
        }
        let rest = &value[index + 1..];
        if let Some(end) = rest.find(';') {
            names.insert(rest[..end].to_string(), ());
        }
    }
}

pub(super) struct DescriptorMentionCache {
    fields: usize,
    methods: usize,
    pool_entries: usize,
    pub(super) names: crate::name_tree::FxHashMap<String, ()>,
}

impl DescriptorMentionCache {
    pub(super) fn new(
        sizes: (usize, usize, usize),
        names: crate::name_tree::FxHashMap<String, ()>,
    ) -> Self {
        Self {
            fields: sizes.0,
            methods: sizes.1,
            pool_entries: sizes.2,
            names,
        }
    }

    pub(super) fn matches(&self, sizes: (usize, usize, usize)) -> bool {
        (self.fields, self.methods, self.pool_entries) == sizes
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct NameId(pub(crate) u32);

#[derive(Clone, Debug)]
struct NameNode {
    parent: Option<NameId>,
    first_child: Option<NameId>,
    next_sibling: Option<NameId>,
    sep: u8,
    segment: String,
}

/// A compact internal-name tree. Names are inserted as slash-separated segments and retained structures
/// store `NameId` (`u32`) handles instead of cloning full internal-name strings.
#[derive(Clone, Debug)]
pub struct NameTree {
    nodes: Vec<NameNode>,
}

impl Default for NameTree {
    fn default() -> Self {
        NameTree {
            nodes: vec![NameNode {
                parent: None,
                first_child: None,
                next_sibling: None,
                sep: 0,
                segment: String::new(),
            }],
        }
    }
}

impl NameTree {
    pub const ROOT: NameId = NameId(0);

    pub fn insert(&mut self, internal: &str) -> NameId {
        if internal.is_empty() {
            return Self::ROOT;
        }
        let mut parent = Self::ROOT;
        let mut sep = 0;
        for segment in internal.split('/') {
            parent = self.child_or_insert(parent, sep, segment);
            sep = b'/';
        }
        parent
    }

    pub fn get(&self, internal: &str) -> Option<NameId> {
        if internal.is_empty() {
            return Some(Self::ROOT);
        }
        let mut parent = Self::ROOT;
        let mut sep = 0;
        for segment in internal.split('/') {
            parent = self.child(parent, sep, segment)?;
            sep = b'/';
        }
        Some(parent)
    }

    pub fn insert_from(&mut self, other: &NameTree, id: NameId) -> NameId {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != Self::ROOT {
            let node = other.node(cur);
            parts.push((node.sep, node.segment.as_str()));
            cur = node.parent.expect("non-root name node has a parent");
        }
        let mut parent = Self::ROOT;
        for (sep, segment) in parts.into_iter().rev() {
            parent = self.child_or_insert(parent, sep, segment);
        }
        parent
    }

    pub fn render(&self, id: NameId) -> String {
        if id == Self::ROOT {
            return String::new();
        }
        let mut parts = Vec::new();
        let mut len = 0usize;
        let mut cur = id;
        while cur != Self::ROOT {
            let node = self.node(cur);
            len += node.segment.len() + usize::from(node.sep != 0);
            parts.push((node.sep, node.segment.as_str()));
            cur = node.parent.expect("non-root name node has a parent");
        }
        let mut out = String::with_capacity(len);
        for (sep, segment) in parts.into_iter().rev() {
            if sep != 0 {
                out.push(sep as char);
            }
            out.push_str(segment);
        }
        out
    }

    pub fn starts_with(&self, id: NameId, prefix: &str) -> bool {
        if prefix.is_empty() {
            return true;
        }
        let mut matched = 0usize;
        for b in self.path_bytes(id) {
            if matched == prefix.len() {
                return true;
            }
            if prefix.as_bytes()[matched] != b {
                return false;
            }
            matched += 1;
        }
        matched == prefix.len()
    }

    pub fn strip_prefix(&self, id: NameId, prefix: &str) -> Option<String> {
        let prefix = prefix.as_bytes();
        let mut matched = 0usize;
        let mut suffix = Vec::new();
        for b in self.path_bytes(id) {
            if matched < prefix.len() {
                if prefix[matched] != b {
                    return None;
                }
                matched += 1;
            } else {
                suffix.push(b);
            }
        }
        (matched == prefix.len()).then(|| {
            String::from_utf8(suffix).expect("name-tree segments are inserted from UTF-8 strings")
        })
    }

    pub fn unsigned_suffix_after_prefix(&self, id: NameId, prefix: &str) -> Option<usize> {
        let prefix = prefix.as_bytes();
        let mut matched = 0usize;
        let mut value = None;
        for b in self.path_bytes(id) {
            if matched < prefix.len() {
                if prefix[matched] != b {
                    return None;
                }
                matched += 1;
            } else if b.is_ascii_digit() {
                let digit = usize::from(b - b'0');
                value = Some(
                    value
                        .unwrap_or(0usize)
                        .checked_mul(10)?
                        .checked_add(digit)?,
                );
            } else {
                return None;
            }
        }
        (matched == prefix.len()).then_some(value).flatten()
    }

    pub fn contains(&self, id: NameId, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let n = needle.as_bytes();
        let mut pi = vec![0usize; n.len()];
        for i in 1..n.len() {
            let mut j = pi[i - 1];
            while j > 0 && n[i] != n[j] {
                j = pi[j - 1];
            }
            if n[i] == n[j] {
                j += 1;
            }
            pi[i] = j;
        }
        let mut j = 0usize;
        for b in self.path_bytes(id) {
            while j > 0 && b != n[j] {
                j = pi[j - 1];
            }
            if b == n[j] {
                j += 1;
            }
            if j == n.len() {
                return true;
            }
        }
        false
    }

    pub fn qualifier_matches(&self, id: NameId, qualifier: &str) -> bool {
        self.get(qualifier) == Some(id) || self.node(id).segment == qualifier
    }

    pub fn package_matches(&self, id: NameId, package: &str) -> bool {
        let Some(parent) = self.parent(id) else {
            return package.is_empty();
        };
        self.path_eq(parent, package)
    }

    pub fn package(&self, id: NameId) -> String {
        self.parent(id)
            .map_or_else(String::new, |parent| self.render(parent))
    }

    pub fn nested_separator_matches(&self, left: NameId, right: NameId) -> bool {
        let (left_len, left_nested_start) = self.path_len_and_nested_start(left);
        let (right_len, right_nested_start) = self.path_len_and_nested_start(right);
        if left_len != right_len || left_nested_start != right_nested_start {
            return false;
        }
        self.path_bytes(left)
            .zip(self.path_bytes(right))
            .enumerate()
            .all(|(idx, (a, b))| {
                a == b || (idx >= left_nested_start && matches!((a, b), (b'.' | b'$', b'.' | b'$')))
            })
    }

    pub fn parent(&self, id: NameId) -> Option<NameId> {
        self.node(id).parent
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn node(&self, id: NameId) -> &NameNode {
        &self.nodes[id.0 as usize]
    }

    fn child_or_insert(&mut self, parent: NameId, sep: u8, segment: &str) -> NameId {
        if let Some(id) = self.child(parent, sep, segment) {
            return id;
        }

        let id = NameId(
            u32::try_from(self.nodes.len()).expect("name tree node count exceeded u32 capacity"),
        );
        let next_sibling = self.nodes[parent.0 as usize].first_child;
        self.nodes.push(NameNode {
            parent: Some(parent),
            first_child: None,
            next_sibling,
            sep,
            segment: segment.to_string(),
        });
        self.nodes[parent.0 as usize].first_child = Some(id);
        id
    }

    fn child(&self, parent: NameId, sep: u8, segment: &str) -> Option<NameId> {
        let mut cur = self.node(parent).first_child;
        while let Some(id) = cur {
            let node = self.node(id);
            if node.sep == sep && node.segment == segment {
                return Some(id);
            }
            cur = node.next_sibling;
        }
        None
    }

    fn path_eq(&self, id: NameId, path: &str) -> bool {
        let mut matched = 0usize;
        for b in self.path_bytes(id) {
            if matched == path.len() || path.as_bytes()[matched] != b {
                return false;
            }
            matched += 1;
        }
        matched == path.len()
    }

    fn path_len_and_nested_start(&self, id: NameId) -> (usize, usize) {
        let mut len = 0usize;
        let mut nested_start = 0usize;
        for b in self.path_bytes(id) {
            len += 1;
            if b == b'/' {
                nested_start = len;
            }
        }
        (len, nested_start)
    }

    fn path_bytes(&self, id: NameId) -> impl Iterator<Item = u8> + '_ {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != Self::ROOT {
            let node = self.node(cur);
            parts.push((node.sep, node.segment.as_bytes()));
            cur = node.parent.expect("non-root name node has a parent");
        }
        parts
            .into_iter()
            .rev()
            .flat_map(|(sep, segment)| std::iter::once(sep).chain(segment.iter().copied()))
            .filter(|b| *b != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::NameTree;

    #[test]
    fn compares_paths_without_rendering() {
        let mut names = NameTree::default();
        let map = names.insert("kotlin/collections/Map");
        let entry = names.insert("kotlin/collections/Map$Entry");

        assert!(names.starts_with(map, "kotlin/collections/"));
        assert!(names.starts_with(map, "kotlin/collections/Ma"));
        assert_eq!(
            names.strip_prefix(map, "kotlin/collections/"),
            Some("Map".to_string())
        );
        assert_eq!(names.strip_prefix(map, "kotlin/List"), None);
        let function = names.insert("kotlin/jvm/functions/Function12");
        assert_eq!(
            names.unsigned_suffix_after_prefix(function, "kotlin/jvm/functions/Function"),
            Some(12)
        );
        assert_eq!(
            names.unsigned_suffix_after_prefix(function, "kotlin/jvm/functions/Function12"),
            None
        );
        assert_eq!(
            names.unsigned_suffix_after_prefix(map, "kotlin/jvm/functions/Function"),
            None
        );
        assert!(names.contains(map, "collections/Map"));
        assert!(names.qualifier_matches(map, "Map"));
        assert!(names.qualifier_matches(map, "kotlin/collections/Map"));
        assert!(!names.qualifier_matches(map, "Entry"));
        assert!(names.package_matches(map, "kotlin/collections"));
        assert!(!names.package_matches(entry, "kotlin"));
        assert_eq!(names.package(entry), "kotlin/collections");
    }
}

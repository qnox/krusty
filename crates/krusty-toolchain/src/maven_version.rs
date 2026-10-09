//! Maven's version order (`org.apache.maven.artifact.versioning.ComparableVersion`), which the
//! toolchain uses wherever it compares versions: `1.0-alpha` < `1.0-rc1` < `1.0` < `1.0-sp`, and
//! numbers compare as numbers.

use std::cmp::Ordering;

/// The qualifiers in release order; anything else sorts after them, alphabetically.
const QUALIFIERS: [&str; 7] = ["alpha", "beta", "milestone", "rc", "snapshot", "", "sp"];
/// The index of the release qualifier (`""`), as `ComparableVersion` writes it.
const RELEASE: &str = "5";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    /// Digits without leading zeros.
    Int(String),
    String(String),
    List(Vec<Item>),
}

/// A parsed version, ordered as Maven orders versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparableVersion(Vec<Item>);

fn qualifier(value: &str) -> String {
    match QUALIFIERS.iter().position(|known| *known == value) {
        Some(index) => index.to_string(),
        None => format!("{}-{value}", QUALIFIERS.len()),
    }
}

fn string_item(value: &str, followed_by_digit: bool) -> Item {
    let value = match value {
        "a" if followed_by_digit => "alpha",
        "b" if followed_by_digit => "beta",
        "m" if followed_by_digit => "milestone",
        "ga" | "final" | "release" => "",
        "cr" => "rc",
        other => other,
    };
    Item::String(value.to_string())
}

fn parse_item(digits: bool, text: &str) -> Item {
    if digits {
        let trimmed = text.trim_start_matches('0');
        Item::Int(if trimmed.is_empty() { "0" } else { trimmed }.to_string())
    } else {
        string_item(text, false)
    }
}

fn is_null(item: &Item) -> bool {
    match item {
        Item::Int(digits) => digits == "0",
        Item::String(value) => value.is_empty(),
        Item::List(items) => items.is_empty(),
    }
}

/// Drop null items from the end, looking past lists (`ListItem.normalize`): `1.0-1` keeps
/// `[1, [1]]`.
fn normalize(items: &mut Vec<Item>) {
    let mut index = items.len();
    while index > 0 {
        index -= 1;
        if is_null(&items[index]) {
            items.remove(index);
        } else if !matches!(items[index], Item::List(_)) {
            break;
        }
    }
}

fn compare_ints(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// `item.compareTo(null)`.
fn compare_to_none(item: &Item) -> Ordering {
    match item {
        Item::Int(digits) => {
            if digits == "0" {
                Ordering::Equal
            } else {
                Ordering::Greater
            }
        }
        Item::String(value) => qualifier(value).as_str().cmp(RELEASE),
        Item::List(items) => items.first().map_or(Ordering::Equal, compare_to_none),
    }
}

fn compare(a: &Item, b: &Item) -> Ordering {
    match (a, b) {
        (Item::Int(a), Item::Int(b)) => compare_ints(a, b),
        (Item::Int(_), _) => Ordering::Greater,
        (Item::String(_), Item::Int(_)) => Ordering::Less,
        (Item::String(a), Item::String(b)) => qualifier(a).cmp(&qualifier(b)),
        (Item::String(_), Item::List(_)) => Ordering::Less,
        (Item::List(_), Item::Int(_)) => Ordering::Less,
        (Item::List(_), Item::String(_)) => Ordering::Greater,
        (Item::List(a), Item::List(b)) => compare_lists(a, b),
    }
}

fn compare_lists(a: &[Item], b: &[Item]) -> Ordering {
    let length = a.len().max(b.len());
    for index in 0..length {
        let result = match (a.get(index), b.get(index)) {
            (None, None) => Ordering::Equal,
            (None, Some(right)) => compare_to_none(right).reverse(),
            (Some(left), None) => compare_to_none(left),
            (Some(left), Some(right)) => compare(left, right),
        };
        if result != Ordering::Equal {
            return result;
        }
    }
    Ordering::Equal
}

impl ComparableVersion {
    pub fn new(version: &str) -> Self {
        let version = version.to_lowercase();
        let characters: Vec<char> = version.chars().collect();
        // A stack of open lists; the last is the one receiving items. Each closed list is
        // normalized and appended to its parent when the version ends.
        let mut stack: Vec<Vec<Item>> = vec![Vec::new()];
        let mut digits = false;
        let mut start = 0;
        let text = |from: usize, to: usize| characters[from..to].iter().collect::<String>();
        for (index, &c) in characters.iter().enumerate() {
            if c == '.' {
                let item = if index == start {
                    Item::Int("0".to_string())
                } else {
                    parse_item(digits, &text(start, index))
                };
                stack.last_mut().expect("a list is open").push(item);
                start = index + 1;
            } else if c == '-' {
                let item = if index == start {
                    Item::Int("0".to_string())
                } else {
                    parse_item(digits, &text(start, index))
                };
                stack.last_mut().expect("a list is open").push(item);
                start = index + 1;
                stack.push(Vec::new());
            } else if c.is_ascii_digit() {
                if !digits && index > start {
                    stack
                        .last_mut()
                        .expect("a list is open")
                        .push(string_item(&text(start, index), true));
                    start = index;
                    stack.push(Vec::new());
                }
                digits = true;
            } else {
                if digits && index > start {
                    stack
                        .last_mut()
                        .expect("a list is open")
                        .push(parse_item(true, &text(start, index)));
                    start = index;
                    stack.push(Vec::new());
                }
                digits = false;
            }
        }
        if characters.len() > start {
            stack
                .last_mut()
                .expect("a list is open")
                .push(parse_item(digits, &text(start, characters.len())));
        }
        while stack.len() > 1 {
            let mut list = stack.pop().expect("more than one list is open");
            normalize(&mut list);
            stack
                .last_mut()
                .expect("a list is open")
                .push(Item::List(list));
        }
        let mut root = stack.pop().expect("the root list");
        normalize(&mut root);
        ComparableVersion(root)
    }
}

impl PartialOrd for ComparableVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ComparableVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_lists(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::ComparableVersion;

    fn assert_order(versions: &[&str]) {
        for pair in versions.windows(2) {
            assert!(
                ComparableVersion::new(pair[0]) < ComparableVersion::new(pair[1]),
                "{} < {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn orders_as_maven_does() {
        assert_order(&[
            "1",
            "1.0.1",
            "1.1",
            "1.2-alpha-1",
            "1.2-alpha2",
            "1.2-beta",
            "1.2-m1",
            "1.2-rc1",
            "1.2-snapshot",
            "1.2",
            "1.2-sp",
            "1.2-abc",
            "1.2.1",
            "2.0.0-RC1",
            "2.0.0",
            "2.4.0",
            "2.4.20",
            "10",
        ]);
    }

    #[test]
    fn equal_spellings_compare_equal() {
        for (a, b) in [
            ("1", "1.0.0"),
            ("1.0-ga", "1"),
            ("1.0-final", "1.0"),
            ("1-cr1", "1-rc1"),
        ] {
            assert_eq!(
                ComparableVersion::new(a),
                ComparableVersion::new(b),
                "{a} = {b}"
            );
        }
    }
}

//! Behavior of particular reference kotlinc releases that no `LanguageFeature` carries.
//!
//! kotlinc releases differ in more than their feature tables: a release fixes a bug, or changes an
//! output detail, without any `-XXLanguage:` switch or `-language-version` reproducing the old
//! behavior. krusty reproduces each supported release exactly, so it models every such difference
//! as one [`Quirk`]: a named behavior with the range of releases that have it.
//!
//! The releases a quirk spans are recorded once, in the `quirks!` table below, and nowhere else. A
//! release's whole profile is resolved once into a [`Quirks`] bit set ([`Quirks::of_release`]);
//! the process reproduces [`Quirks::target`], and consumers test one bit and never compare
//! versions themselves.
//!
//! - **A new quirk** is one entry in that table and one `has` test at the place that implements
//!   the old behavior. Adding a release to the `kotlin-versions` manifest needs no edit here.
//! - **Dropping an old release** from the manifest leaves quirks that no supported release has
//!   anymore. `every_quirk_affects_a_supported_release` names them; deleting the entry makes the
//!   compiler point at every consumer, whose quirk branch is then dead and goes with it.

use super::KotlinVersion;

/// The releases that have a quirk: from `since` (`None`: every release krusty knows) up to, but
/// excluding, `fixed_in` (`None`: still present in the newest supported release). `fixed_in`
/// may name a release the manifest does not list yet.
#[derive(Clone, Copy, Debug)]
struct Affected {
    since: Option<KotlinVersion>,
    fixed_in: Option<KotlinVersion>,
}

impl Affected {
    const fn contains(self, release: KotlinVersion) -> bool {
        let after_since = match self.since {
            Some(since) => !precedes(release, since),
            None => true,
        };
        let before_fix = match self.fixed_in {
            Some(fixed_in) => precedes(release, fixed_in),
            None => true,
        };
        after_since && before_fix
    }
}

/// `left < right` in release order, usable in a `const` profile.
const fn precedes(left: KotlinVersion, right: KotlinVersion) -> bool {
    if left.major != right.major {
        return left.major < right.major;
    }
    if left.minor != right.minor {
        return left.minor < right.minor;
    }
    left.patch < right.patch
}

/// Declares every [`Quirk`] with the releases that have it. Each entry documents the behavior the
/// affected releases show, which is what a `has` test selects.
macro_rules! quirks {
    ($(
        $(#[$doc:meta])*
        $name:ident { since: $since:expr, fixed_in: $fixed_in:expr $(,)? }
    )*) => {
        /// One release-specific kotlinc behavior. See the module documentation.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Quirk {
            $($(#[$doc])* $name,)*
        }

        impl Quirk {
            /// Every quirk, in declaration order.
            pub const ALL: &'static [Quirk] = &[$(Quirk::$name),*];

            const fn affected(self) -> Affected {
                match self {
                    $(Quirk::$name => Affected { since: $since, fixed_in: $fixed_in },)*
                }
            }
        }
    };
}

quirks! {
    /// A delegate whose `getValue`/`setValue` resolves nowhere names only the candidates of the
    /// earliest convention rung that contributed any (members, then member extensions, then
    /// extensions). Later releases name every rung's candidates, members first.
    DelegateFailureNamesFirstRungOnly { since: None, fixed_in: Some(KotlinVersion::V2_4_20) }
    /// `+UnitConversionsOnArbitraryExpressions` is known but converts no function value whose
    /// result is not `Unit` (KT-84393).
    UnitConversionRejectsNonUnitResult { since: None, fixed_in: Some(KotlinVersion::V2_4_20) }
    /// A suspend function declared to return `Unit` that forwards a tail call returns the callee's
    /// result as it is, instead of adapting it to `Unit`.
    SuspendUnitTailCallKeepsCalleeResult { since: None, fixed_in: Some(KotlinVersion::V2_4_20) }
    /// An anonymous context parameter is unnamed in the IR, so it gets no local-variable row; its
    /// label reaches only reflection and null assertions.
    AnonymousContextParameterIsNotLocal { since: None, fixed_in: Some(KotlinVersion::V2_4_20) }
    /// A repeated anonymous context label is numbered after `#` (`$context-String#1`) instead of
    /// `$` (`$context-String$1`).
    AnonymousContextOrdinalAfterHash { since: None, fixed_in: Some(KotlinVersion::V2_4_20) }
    /// `@Metadata.xi` of a `k=3` synthetic class holds the IR flags alone, without the class's
    /// normalized visibility in bits 8–10.
    SyntheticClassXiOmitsVisibility { since: None, fixed_in: Some(KotlinVersion::V2_4_20) }
    /// A synthetic annotation implementation stamps nullability on its constructor, `equals` and
    /// `toString`.
    AnnotationImplCarriesNullability { since: None, fixed_in: Some(KotlinVersion::V2_4_20) }
    /// A library compiled below language 2.3 stamps KLIB `metadata_version` `1.4.1` instead of the
    /// language level's own (read off `kotlinc-native -p library -Xmetadata-klib`).
    KlibLegacyMetadataVersionBelowLanguage23 {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
    }
}

const _: () = assert!(Quirk::ALL.len() <= u64::BITS as usize);

/// The quirks one release has: its whole profile, resolved once. Testing a member is one bit test.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Quirks(u64);

impl Quirks {
    /// No quirks at all: the behavior of the newest kotlinc krusty models.
    pub const NONE: Self = Self(0);

    /// Every quirk `release` has.
    pub const fn of_release(release: KotlinVersion) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < Quirk::ALL.len() {
            let quirk = Quirk::ALL[index];
            if quirk.affected().contains(release) {
                bits |= Self::bit(quirk);
            }
            index += 1;
        }
        Self(bits)
    }

    /// The profile of the reference release this process reproduces ([`super::target`]),
    /// resolved on first use. The reference release is process-wide, so this is the profile every
    /// phase reproduces; a hot path may hoist the copy, but reading it is one load and a bit test.
    pub fn target() -> Self {
        static TARGET: std::sync::OnceLock<Quirks> = std::sync::OnceLock::new();
        *TARGET.get_or_init(|| Self::of_release(super::target()))
    }

    /// Whether this profile has `quirk`.
    pub const fn has(self, quirk: Quirk) -> bool {
        self.0 & Self::bit(quirk) != 0
    }

    /// This profile with `quirk` on or off, so a test of a rule that takes a profile can compare
    /// the quirk's behavior with the fixed one.
    pub const fn with(self, quirk: Quirk, present: bool) -> Self {
        if present {
            Self(self.0 | Self::bit(quirk))
        } else {
            Self(self.0 & !Self::bit(quirk))
        }
    }

    /// The quirks in this profile, in declaration order.
    pub fn iter(self) -> impl Iterator<Item = Quirk> {
        Quirk::ALL
            .iter()
            .copied()
            .filter(move |&quirk| self.has(quirk))
    }

    const fn bit(quirk: Quirk) -> u64 {
        1 << quirk as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_quirk_affects_a_supported_release() {
        let supported = KotlinVersion::supported();
        let stale: Vec<Quirk> = Quirk::ALL
            .iter()
            .copied()
            .filter(|&quirk| {
                !supported
                    .iter()
                    .any(|&release| Quirks::of_release(release).has(quirk))
            })
            .collect();
        assert!(
            stale.is_empty(),
            "no supported release has {stale:?} anymore: delete the quirk and the branches that \
             test it"
        );
    }

    #[test]
    fn a_quirk_spans_since_up_to_its_fix() {
        let affected = Affected {
            since: Some(KotlinVersion::V2_4_10),
            fixed_in: Some(KotlinVersion::V2_4_21),
        };
        assert!(!affected.contains(KotlinVersion::V2_4_0));
        assert!(affected.contains(KotlinVersion::V2_4_10));
        assert!(affected.contains(KotlinVersion::V2_4_20));
        assert!(!affected.contains(KotlinVersion::V2_4_21));
        assert!(!affected.contains(KotlinVersion::new(2, 10, 0)));
        let open = Affected {
            since: None,
            fixed_in: None,
        };
        assert!(open.contains(KotlinVersion::new(0, 0, 0)));
        assert!(open.contains(KotlinVersion::new(99, 0, 0)));
    }

    #[test]
    fn a_profile_holds_exactly_its_releases_quirks() {
        for release in KotlinVersion::supported() {
            let profile = Quirks::of_release(release);
            for &quirk in Quirk::ALL {
                assert_eq!(
                    profile.has(quirk),
                    quirk.affected().contains(release),
                    "{quirk:?} on {release}"
                );
            }
            assert_eq!(
                profile.iter().collect::<Vec<_>>(),
                Quirk::ALL
                    .iter()
                    .copied()
                    .filter(|&quirk| profile.has(quirk))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_profile_toggles_one_quirk() {
        let quirk = Quirk::ALL[0];
        let on = Quirks::NONE.with(quirk, true);
        assert!(on.has(quirk));
        assert_eq!(on.iter().collect::<Vec<_>>(), [quirk]);
        assert_eq!(on.with(quirk, false), Quirks::NONE);
    }

    #[test]
    fn precedes_orders_numerically() {
        let releases = [
            KotlinVersion::new(1, 9, 25),
            KotlinVersion::V2_4_0,
            KotlinVersion::new(2, 4, 9),
            KotlinVersion::V2_4_10,
            KotlinVersion::new(2, 10, 0),
            KotlinVersion::new(3, 0, 0),
        ];
        for (i, &left) in releases.iter().enumerate() {
            for (j, &right) in releases.iter().enumerate() {
                assert_eq!(precedes(left, right), i < j, "{left} < {right}");
                assert_eq!(precedes(left, right), left < right);
            }
        }
    }
}

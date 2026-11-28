//! Behavior of particular reference kotlinc releases that no `LanguageFeature` carries.
//!
//! kotlinc releases differ in more than their feature tables: a release fixes a bug, or changes an
//! output detail, without any `-XXLanguage:` switch or `-language-version` reproducing the old
//! behavior. krusty reproduces each supported release exactly, so it models every such difference
//! as one [`Quirk`]: a named behavior with the range of releases that have it.
//!
//! The releases a quirk spans are recorded once, in the `quirks!` table below, and nowhere else. A
//! release's whole profile is resolved once into a [`Quirks`] bit set ([`Quirks::of_release`]);
//! a compilation reproduces its [`super::ReferenceProfile::quirks`], and consumers test one bit and never compare
//! versions themselves.
//!
//! - **A new quirk** is one entry in that table and one `has` test at the place that implements
//!   the old behavior. Adding a release to the `kotlin-versions` manifest needs no edit here.
//! - **A defect** is a quirk that makes kotlinc compile a program wrongly: it rejects valid code,
//!   computes a wrong value, or binds a call the language rules bind otherwise. Every defect has a
//!   program in `tests/kotlinc_defects_e2e.rs` that shows kotlinc still has it in the releases the
//!   table names and that the fix mode fixes it. krusty reproduces
//!   it by default, as it reproduces every quirk, and fixes it only when the compilation asks for
//!   [`super::KotlincDefects::Fix`], which compiles as if the release did not have it: the behavior
//!   of the release that fixed it, or of the language rule when none has yet. Output details
//!   (metadata stamps, debug names, diagnostic wording) are quirks, not defects, and stay as the
//!   release has them in either mode.
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

/// Declares every [`Quirk`] with the releases that have it and whether it is a defect. Each entry
/// documents the behavior the affected releases show, which is what a `has` test selects.
macro_rules! quirks {
    ($(
        $(#[$doc:meta])*
        $name:ident { since: $since:expr, fixed_in: $fixed_in:expr, defect: $defect:expr $(,)? }
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

            /// Whether this quirk is a defect, which [`super::KotlincDefects::Fix`] fixes.
            pub const fn is_defect(self) -> bool {
                match self {
                    $(Quirk::$name => $defect,)*
                }
            }
        }
    };
}

quirks! {
    /// A delegate whose `getValue`/`setValue` resolves nowhere names only the candidates of the
    /// earliest convention rung that contributed any (members, then member extensions, then
    /// extensions). Later releases name every rung's candidates, members first.
    DelegateFailureNamesFirstRungOnly {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// Once no visible candidate applies, the candidates the site cannot see still compete scope
    /// level by scope level, nearest first, instead of as one visibility-error group across the
    /// tower: under a suppressed visibility error an explicitly imported `Any.tag` takes the call
    /// from a star-imported `String.tag`.
    InvisibleCandidatesKeepScopeLevels {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// `+UnitConversionsOnArbitraryExpressions` is known but converts no function value whose
    /// result is not `Unit` (KT-84393).
    UnitConversionRejectsNonUnitResult {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: true,
    }
    /// A suspend function declared to return `Unit` that forwards a tail call returns the callee's
    /// result as it is, instead of adapting it to `Unit`.
    SuspendUnitTailCallKeepsCalleeResult {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// An anonymous context parameter is unnamed in the IR, so it gets no local-variable row; its
    /// label reaches only reflection and null assertions.
    AnonymousContextParameterIsNotLocal {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// A context parameter passed on as a context argument is read at the offsets of its
    /// declaration, so the load marks the line the declaration starts on. Later releases read it
    /// with no offsets, which marks nothing.
    ContextParameterLoadMarksDeclarationLine {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// A repeated anonymous context label is numbered after `#` (`$context-String#1`) instead of
    /// `$` (`$context-String$1`).
    AnonymousContextOrdinalAfterHash {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// `@Metadata.xi` of a `k=3` synthetic class holds the IR flags alone, without the class's
    /// normalized visibility in bits 8–10.
    SyntheticClassXiOmitsVisibility {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// A synthetic annotation implementation stamps nullability on its constructor, `equals` and
    /// `toString`.
    AnnotationImplCarriesNullability {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// A `null` function value suspend-converted to a nullable suspend function type converts to an
    /// adapter whose `invoke` yields `null`, instead of staying `null` (KT-89451).
    NullFunctionValueConvertsToAdapter {
        since: None,
        fixed_in: Some(KotlinVersion::V2_5_0),
        defect: true,
    }
    /// A function value held by a type parameter with a nullable function bound, passed where a
    /// suspend function type is expected, is not converted at all: the value is cast to the suspend
    /// function's class, so `null` stays `null` and any other value throws `ClassCastException`.
    TypeParameterFunctionValueSkipsSuspendConversion {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: true,
    }
    /// A `null` function value held by a type parameter with a nullable function bound, passed
    /// where a suspend function type is expected, converts to an adapter that invokes it without a
    /// null check: calling it throws `NullPointerException`. Earlier releases do not convert the
    /// value ([`Quirk::TypeParameterFunctionValueSkipsSuspendConversion`]).
    NullTypeParameterFunctionValueIsDereferenced {
        since: Some(KotlinVersion::V2_4_20),
        fixed_in: Some(KotlinVersion::V2_5_0),
        defect: true,
    }
    /// An interface's `$DefaultImpls` forwarder for a default it only inherits maps its body to
    /// the interface's declaration line; later releases give it no line.
    InheritedHolderForwarderMapsInterfaceLine {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// A library compiled below language 2.3 stamps KLIB `metadata_version` `1.4.1` instead of the
    /// language level's own (read off `kotlinc-native -p library -Xmetadata-klib`).
    KlibLegacyMetadataVersionBelowLanguage23 {
        since: None,
        fixed_in: Some(KotlinVersion::V2_4_20),
        defect: false,
    }
    /// A callable reference to a `vararg` function takes an expected input that the enclosing call
    /// declares as one of its own type variables (`f: (T) -> Int`) as one collected element, even
    /// when the variable is an array of the vararg's own classifier: `arr.let(::count)` counts one
    /// element (KT-90169). A concrete or array-shaped input still takes the whole array.
    VarargReferenceTakesTypeVariableInputAsElement {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: true,
    }
    /// An explicit context argument of a contextual extension call is evaluated before the
    /// extension receiver, in the order the parameters are passed, although it is written after it.
    /// From 2.5 kotlinc evaluates operands in the order they are written.
    ContextArgumentEvaluatedBeforeExtensionReceiver {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: true,
    }
    /// The JVM optimizer's constant-condition elimination leaves a `goto` that carries a known
    /// `int` to an `if<cond>` jumping there, instead of sending it straight to the branch that
    /// constant takes: a materialized Boolean is tested again (`iconst_1; goto L; …; L: ifne`).
    ConstantGotoNotThreadedToComparison {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: false,
    }
    /// A function reference class's constructor numbers its bound receiver parameter
    /// (`receiver0`). From 2.5 kotlinc passes at most one bound receiver and names it `receiver`.
    FunctionReferenceNumbersBoundReceiver {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: false,
    }
    /// A class's static state initializes with its `companion object` first: the companion is
    /// constructed, then every `companion { }` block property of the class is stored, then the
    /// companion's initializer body runs. From 2.5 kotlinc initializes in program order: the block
    /// properties written before the `companion object`, the companion, then the ones after it.
    ClassStaticsInitializeBeforeCompanionBody {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: false,
    }
    /// `-jvm-target` accepts `1.8` and `9` through `26`; `27` and `28` are unknown targets. From
    /// 2.5 kotlinc also accepts `27` and `28`.
    JvmTargetsEndAt26 {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: false,
    }
    /// A not-null operator (`!!`, the left of `?:`, the receiver of `?.`) applied to a nullable
    /// intersection fixed below an upper type (`pick(a, b)!!` with `S : Base`) narrows it to a new
    /// intersection that publishes `Any`. From 2.5 kotlinc keeps the upper type, so the result still
    /// approximates to the bound; a smart cast (`if (x != null) x`) still publishes `Any`.
    NotNullOperatorDropsIntersectionApproximation {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: false,
    }
    /// A `tailrec` function's recursive infix call or operator expression is reported at the infix
    /// name or the operator token. From 2.5 the position is the start of the whole expression,
    /// as wide as the function's name.
    TailrecDotlessCallReportedAtOperator {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: false,
    }
    /// In a `tailrec` function a recursive call followed by `return Unit` is not a tail call:
    /// reading the `Unit` object is an instruction after it. From 2.5 it is not.
    TailrecUnitObjectReturnFollowsCall {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: false,
    }
    /// In a `tailrec` function with a non-null result, a recursive call that is the left operand of
    /// `?:` in tail position is itself a tail call. From 2.5 an elvis operand never is.
    TailrecElvisLeftOperandIsTail {
        since: None,
        fixed_in: Some(KotlinVersion::new(2, 5, 0)),
        defect: false,
    }
    /// The Kotlin/Native and Kotlin/Wasm standard library's `ArrayList.addAll` makes room for the
    /// new elements before it reads the argument, so a range of the list being appended to is
    /// invalidated first: `list.addAll(list.subList(0, 2))` raises
    /// `ConcurrentModificationException` and leaves the list grown by two nulls. The JVM's reads
    /// the argument first and appends the range. A library quirk, which only those platforms'
    /// backends test.
    ArrayListAddAllGrowsBeforeReadingItsArgument {
        since: None,
        fixed_in: None,
        defect: true,
    }
    /// A Kotlin subclass of a Java class that realizes a builtin under another JVM name and also
    /// declares a method under the builtin's Kotlin name (`charAt(int)` beside its own
    /// `get(int)`) gets the builtin's entry under the Kotlin name, delegating to the renamed
    /// method. The entry overrides the unrelated Java method, so a call to it reaches `charAt`, and
    /// a class over a final one fails to load. The Java method is not the builtin's override, so a
    /// subclass inherits it unchanged.
    JavaRenamedBuiltinEntryOverridesJavaMember {
        since: None,
        fixed_in: None,
        defect: true,
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

    /// This profile as a compilation under `defects` reproduces it: unchanged, or without its
    /// defects.
    pub const fn under(self, defects: super::KotlincDefects) -> Self {
        match defects {
            super::KotlincDefects::Reproduce => self,
            super::KotlincDefects::Fix => Self(self.0 & !Self::DEFECTS.0),
        }
    }

    /// Every defect in the table.
    const DEFECTS: Self = {
        let mut bits = 0;
        let mut index = 0;
        while index < Quirk::ALL.len() {
            if Quirk::ALL[index].is_defect() {
                bits |= Self::bit(Quirk::ALL[index]);
            }
            index += 1;
        }
        Self(bits)
    };

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
    fn fixing_defects_drops_exactly_the_defects() {
        use super::super::KotlincDefects;
        for release in KotlinVersion::supported() {
            let profile = Quirks::of_release(release);
            assert_eq!(
                profile.under(KotlincDefects::Reproduce),
                profile,
                "{release}"
            );
            let fixed = profile.under(KotlincDefects::Fix);
            for &quirk in Quirk::ALL {
                assert_eq!(
                    fixed.has(quirk),
                    profile.has(quirk) && !quirk.is_defect(),
                    "{quirk:?} on {release}"
                );
            }
        }
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

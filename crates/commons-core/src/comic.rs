//! Comics, manga, doujin (T-P1-007, spec §5.5).
//!
//! # The hard part is not reading the archive
//!
//! A comic is a zip of images. Listing it is T-P1-004's job. What makes a comic
//! a comic is the ORDER of its pages and the DIRECTION it is read in, and both
//! are genuinely ambiguous in a way that video ordering never is.
//!
//! **Ordering.** `img_2.jpg` before `img_10.jpg`, obviously. But then there is
//! `img_02.jpg`, `img_2.jpg`, `page1.jpg`, `page001.jpg`, `01.jpg`, `1.jpg`,
//! `cover.jpg`, `Chapter 1 - 001.jpg`, and a scan that mixes all of them. A
//! lexicographic sort gets most of these wrong in a way a user notices
//! immediately: `img_10` before `img_2` reads as pages out of order and the
//! comic looks broken.
//!
//! The sort here is a natural-sort comparison that finds the first run of
//! digits in each name and compares those as numbers, falling back to a
//! case-insensitive textual comparison for the rest. That is the same rule
//! `ls -v` and every file manager uses, and it is the rule users already
//! expect.
//!
//! **Direction.** Manga reads right-to-left; Western comics read
//! left-to-right. A comic cannot be assumed either way. Spec §5.5 makes it a
//! property of the object rather than a global setting, because a library
//! routinely contains both and because a doujin circle's stated direction is
//! information the archive itself may or may not carry.
//!
//! **Spreads.** A two-page spread scanned as one image must be split, or the
//! reader zooms out to nothing on half the pages. This is a flag on the page
//! with a width ratio, not a guess, because whether a wide image is a spread or
//! a double-page cover is a judgement the user has to make.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// Which way the pages go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ReadingDirection {
    /// Western: page 1 on the left, advancing right. The default because a
    /// library with no stated direction is more often western, and being
    /// wrong is trivially corrected by the user while being wrong the other
    /// way is a pile of manga read backwards.
    #[default]
    LeftToRight,
    /// Manga: page 1 on the right, advancing left.
    RightToLeft,
    /// Vertical, top to bottom, right to left. Rare in the formats Commons
    /// handles, but it exists and a UI that cannot represent it will silently
    /// re-order someone's collection.
    VerticalRightToLeft,
}

impl ReadingDirection {
    /// Whether page `n` (0-based, in file order) is on a left-hand page.
    ///
    /// The parity flips with direction, which is the whole difference between
    /// the two: in a left-to-right book page 1 is a recto (right-hand page),
    /// and in a right-to-left book page 1 is a verso.
    pub fn is_left_hand(&self, n: usize) -> bool {
        match self {
            ReadingDirection::LeftToRight => n % 2 == 1,
            ReadingDirection::RightToLeft => n.is_multiple_of(2),
            ReadingDirection::VerticalRightToLeft => n.is_multiple_of(2),
        }
    }

    /// Whether two facing pages are one spread.
    ///
    /// A spread is a single wide image showing two pages. In file order the
    /// spread sits between the page before it and the page after it, and which
    /// pages those are depends on direction -- which is why this needs the
    /// direction and not just the index.
    pub fn is_spread_start(&self, n: usize) -> bool {
        match self {
            ReadingDirection::LeftToRight => n.is_multiple_of(2),
            ReadingDirection::RightToLeft => n % 2 == 1,
            ReadingDirection::VerticalRightToLeft => n % 2 == 1,
        }
    }
}

impl fmt::Display for ReadingDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ReadingDirection::LeftToRight => "ltr",
            ReadingDirection::RightToLeft => "rtl",
            ReadingDirection::VerticalRightToLeft => "vertical-rtl",
        })
    }
}

/// One page within a comic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// The page's index in FILE order, which is the archive's own order and is
    /// never mutated. The reading order is [`Comic::reading_order`].
    pub file_index: usize,
    /// The member path within the archive, or the filename for a loose
    /// directory. Kept so a citation or an export can name the actual file.
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Whether this image is a two-page spread rather than a single page.
    pub is_spread: bool,
}

/// A comic: an ordered sequence of images from one container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comic {
    pub pages: Vec<Page>,
    pub direction: ReadingDirection,
    /// True when the container supplied a page order that Commons used, as
    /// opposed to Commons inferring one from filenames. Kept so a re-scan can
    /// tell "the order came from the book" and does not need re-inferring.
    pub order_is_authoritative: bool,
}

impl Comic {
    /// Build from unsorted image members, sorting by name.
    ///
    /// This is the inference path, and `order_is_authoritative` is false: the
    /// order came from filenames, which is a guess that is right most of the
    /// time and visibly wrong when it is not.
    pub fn from_names(names: impl IntoIterator<Item = String>) -> Self {
        let mut pages: Vec<Page> = names
            .into_iter()
            .enumerate()
            .map(|(file_index, name)| {
                let (w, h) = (0u32, 0u32);
                Page {
                    file_index,
                    name,
                    width: w,
                    height: h,
                    is_spread: false,
                }
            })
            .collect();
        sort_pages(&mut pages);
        Self {
            pages,
            direction: ReadingDirection::default(),
            order_is_authoritative: false,
        }
    }

    /// Build from a container that supplied its own order (a CB7 with a
    /// `ComicInfo.xml` page list, or a PDF with an explicit page tree).
    pub fn from_authoritative_order(pages: Vec<Page>, direction: ReadingDirection) -> Self {
        Self {
            pages,
            direction,
            order_is_authoritative: true,
        }
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// The page at a 0-based FILE index, regardless of direction.
    pub fn at(&self, file_index: usize) -> Option<&Page> {
        self.pages.get(file_index)
    }

    /// The number of pages, for display as "12 pages".
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// The cover: page 1 in file order.
    ///
    /// File order, not reading order, because a cover is a property of the
    /// archive and the user is looking at it in a file listing. For a comic
    /// whose cover is on the right (RTL), the UI shows the right-hand page
    /// first; that is a layout concern.
    pub fn cover(&self) -> Option<&Page> {
        self.pages.first()
    }

    /// Flip to the other direction, keeping every page.
    pub fn set_direction(&mut self, direction: ReadingDirection) {
        self.direction = direction;
    }

    /// The number of spreads, i.e. how many pages will be shown two-up.
    pub fn spread_count(&self) -> usize {
        self.pages.iter().filter(|p| p.is_spread).count()
    }

    /// A warning about this comic's shape, if any.
    ///
    /// These are the things a user would otherwise discover as a bug report:
    /// a comic whose pages are all landscape (a mis-tagged image sequence), a
    /// comic with one page, a comic where every page is a spread (a zip of
    /// double-page scans with the singles missing).
    pub fn sanity_check(&self) -> Vec<ComicWarning> {
        let mut out = Vec::new();
        if self.pages.is_empty() {
            out.push(ComicWarning::NoPages);
            return out;
        }
        if self.pages.len() == 1 {
            out.push(ComicWarning::SinglePage);
        }
        if self.pages.iter().all(|p| p.is_spread) && self.pages.len() > 1 {
            out.push(ComicWarning::AllSpreads);
        }
        if self.pages.iter().all(|p| p.is_spread) {
            out.push(ComicWarning::NoSingles);
        }
        // A mixed set of portrait and landscape singles, where the landscape
        // ones are not marked as spreads, is the signature of a zip with a
        // couple of double-page scans the container did not flag.
        let (wide_singles, tall_singles) =
            self.pages
                .iter()
                .filter(|p| !p.is_spread)
                .fold((0usize, 0usize), |(w, t), p| {
                    if p.width > p.height && p.width > 0 {
                        (w + 1, t)
                    } else {
                        (w, t + 1)
                    }
                });
        if wide_singles > 0 && tall_singles > 0 {
            out.push(ComicWarning::MixedOrientation);
        }
        out
    }
}

/// Something a user would want to be told about a comic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComicWarning {
    /// The container had no images in it.
    NoPages,
    /// One page. Valid, but probably a zip someone made to share one image.
    SinglePage,
    /// Every page is a spread, so the book is unreadable without splitting.
    AllSpreads,
    /// No single pages at all.
    NoSingles,
    /// Portrait and landscape pages mixed, with the landscape ones not marked
    /// as spreads.
    MixedOrientation,
}

impl fmt::Display for ComicWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ComicWarning::NoPages => "the archive contains no pages",
            ComicWarning::SinglePage => "this comic has a single page",
            ComicWarning::AllSpreads => {
                "every page is a two-page spread, so the book needs splitting"
            }
            ComicWarning::NoSingles => "there are no single pages",
            ComicWarning::MixedOrientation => {
                "portrait and landscape pages are mixed, and the landscape ones are not marked as spreads"
            }
        })
    }
}

/// Sort pages by name, treating runs of digits as numbers.
///
/// This is the `ls -v` rule: compare name segments, where a segment of digits
/// compares numerically and anything else compares case-insensitively. It gets
/// `img_2` before `img_10`, which a plain lexicographic sort does not, and it
/// gets `page03` next to `page3` rather than a thousand pages apart.
pub fn sort_pages(pages: &mut [Page]) {
    pages.sort_by(|a, b| natural_cmp(&a.name, &b.name));
}

/// Compare two filenames the way a person reads them.
///
/// Digit runs compare as numbers. Everything else compares
/// case-insensitively, with the original used only to break a tie, so the
/// result is a total order -- `sort_pages` is deterministic, which matters
/// because page order is stored and a re-scan that reorders a comic is a bug a
/// user notices immediately.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);

    loop {
        // Advance each side to its NEXT digit run, consuming the text before it
        // only once the two positions can be compared. This is the part that is
        // easy to get wrong: an earlier version examined the current character
        // and gave up if either side was not a digit, which broke immediately on
        // the "img_" prefix and silently reduced the whole comparison to a
        // plain string compare.
        i = skip_to_digit(ab, i);
        j = skip_to_digit(bb, j);
        if i >= ab.len() || j >= bb.len() {
            break;
        }

        // Both are now on a digit. The text between the previous run and this
        // one must match, or the names differ at a non-digit and that decides.
        // Compared case-insensitively so `IMG_2` and `img_2` are siblings.
        if !segment_eq_ignore_case(ab, bb, i, j) {
            return compare_segments(ab, bb, i, j);
        }

        let de = digit_run_end(ab, i);
        let ee = digit_run_end(bb, j);

        // The digit runs, compared as numbers with leading zeros ignored, so
        // `page007` and `page7` are the same value.
        let (an, bn) = (trim_zeros(&ab[i..de]), trim_zeros(&bb[j..ee]));
        let by_length = an.len().cmp(&bn.len());
        if by_length != Ordering::Equal {
            return by_length;
        }
        match an.cmp(bn) {
            Ordering::Equal => {}
            other => return other,
        }

        // Equal as numbers. The raw text breaks the tie so `page1` and `page01`
        // still have a defined order rather than comparing equal.
        match ab[i..de].cmp(&bb[j..ee]) {
            Ordering::Equal => {}
            other => return other,
        }

        i = de;
        j = ee;
    }

    // Past the last digit run in one or both. Case-insensitive first, with the
    // original as the tiebreak, so `A.jpg` and `a.jpg` have a stable order and
    // neither sorts before every uppercase name.
    let (ai, bi) = (&a[i.min(a.len())..], &b[j.min(b.len())..]);
    ai.to_lowercase()
        .cmp(&bi.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// The index of the next ASCII digit at or after `from`, or `len`.
fn skip_to_digit(b: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < b.len() && !b[i].is_ascii_digit() {
        i += 1;
    }
    i
}

/// Do the bytes before `at` (since the last compared position) match on the
/// other side? The caller has already established both are at a digit; this
/// compares the non-digit run that precedes them, so `a1b2` and `a1b3` are
/// separated by their second run rather than being compared as bare numbers.
fn segment_eq_ignore_case(a: &[u8], b: &[u8], at: usize, bt: usize) -> bool {
    let (ai, bi) = (at.min(a.len()), bt.min(b.len()));
    // Walk back over the non-digit run ending at ai/bi.
    let mut x = ai;
    while x > 0 && !a[x - 1].is_ascii_digit() {
        x -= 1;
    }
    let mut y = bi;
    while y > 0 && !b[y - 1].is_ascii_digit() {
        y -= 1;
    }
    let la = &a[x..ai];
    let lb = &b[y..bi];
    la.len() == lb.len() && la.eq_ignore_ascii_case(lb)
}

/// Compare the non-digit runs ending at `at`/`bt`, case-insensitively.
fn compare_segments(a: &[u8], b: &[u8], at: usize, bt: usize) -> Ordering {
    let (ai, bi) = (at.min(a.len()), bt.min(b.len()));
    let mut x = ai;
    while x > 0 && !a[x - 1].is_ascii_digit() {
        x -= 1;
    }
    let mut y = bi;
    while y > 0 && !b[y - 1].is_ascii_digit() {
        y -= 1;
    }
    a[x..ai]
        .to_ascii_lowercase()
        .cmp(&b[y..bi].to_ascii_lowercase())
        .then_with(|| a[x..ai].cmp(&b[y..bi]))
}

/// The index just past the run of ASCII digits starting at `from`.
fn digit_run_end(b: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let mut s = 0;
    // Leave the last digit alone: "0" trimmed to "" would be wrong.
    while s + 1 < digits.len() && digits[s] == b'0' {
        s += 1;
    }
    &digits[s..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The resulting page-name order for a list of already-owned names.
    fn order_owned(list: &[String]) -> Vec<String> {
        Comic::from_names(list.to_vec())
            .pages
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    fn order(list: &[&str]) -> Vec<String> {
        Comic::from_names(names(list))
            .pages
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    // ---- the ordering rule: the reason this module exists ----

    /// The case that makes lexicographic sorting visibly wrong. A user opening
    /// a comic whose pages run 1, 2, 10, 11 and seeing 1, 10, 11, 2 reports it
    /// as a broken comic, not as a sorting quirk.
    #[test]
    fn numeric_pages_sort_numerically_not_lexicographically() {
        assert_eq!(
            order(&["img_10.jpg", "img_2.jpg", "img_1.jpg"]),
            vec!["img_1.jpg", "img_2.jpg", "img_10.jpg"]
        );
    }

    #[test]
    fn three_digit_pages_sort_after_two_digit_pages() {
        assert_eq!(
            order(&["p100.jpg", "p21.jpg", "p3.jpg"]),
            vec!["p3.jpg", "p21.jpg", "p100.jpg"]
        );
    }

    /// The property, stated as a property: a lexicographic sort puts page-100
    /// before page-21, so asserting the relative order of those two catches a
    /// regression to lexicographic whatever the rest of the list looks like.
    #[test]
    fn page_100_comes_after_page_21() {
        let o = order(&["page100.jpg", "page21.jpg"]);
        let i100 = o.iter().position(|s| s.starts_with("page100")).unwrap();
        let i21 = o.iter().position(|s| s.starts_with("page21")).unwrap();
        assert!(i21 < i100, "{o:?}");
    }

    /// Zero padding is a formatting difference, not a value difference, so
    /// `page03` and `page3` are adjacent. They still have a defined order
    /// between them rather than an arbitrary one.
    #[test]
    fn zero_padding_does_not_change_the_value_but_does_have_an_order() {
        let o = order(&["page3.jpg", "page03.jpg", "page21.jpg"]);
        let i3 = o.iter().position(|s| *s == "page3.jpg").unwrap();
        let i03 = o.iter().position(|s| *s == "page03.jpg").unwrap();
        let i21 = o.iter().position(|s| *s == "page21.jpg").unwrap();
        assert!(i3 < i21 && i03 < i21, "both are page 3: {o:?}");
        assert!(i3 != i03, "they must have a defined order: {o:?}");
    }

    #[test]
    fn multiple_numbers_in_a_name_compare_in_order() {
        assert_eq!(
            order(&[
                "ch2_p10.jpg",
                "ch1_p10.jpg",
                "ch2_p2.jpg",
                "ch10_p1.jpg",
                "ch1_p2.jpg",
            ]),
            vec![
                "ch1_p2.jpg",
                "ch1_p10.jpg",
                "ch2_p2.jpg",
                "ch2_p10.jpg",
                "ch10_p1.jpg",
            ]
        );
    }

    #[test]
    fn a_name_with_no_digits_sorts_by_text() {
        assert_eq!(
            order(&["cover.jpg", "alpha.jpg", "beta.jpg"]),
            vec!["alpha.jpg", "beta.jpg", "cover.jpg"]
        );
    }

    #[test]
    fn a_name_that_is_only_digits_sorts_numerically() {
        assert_eq!(
            order(&["10.jpg", "2.jpg", "1.jpg", "100.jpg"]),
            vec!["1.jpg", "2.jpg", "10.jpg", "100.jpg"]
        );
    }

    /// Sorting must be deterministic. Page order is stored; a re-scan that
    /// reshuffles a comic is a bug a user notices immediately, and it happens
    /// when a comparator returns `Equal` for names that are not equal.
    #[test]
    fn the_comparator_is_a_total_order() {
        let a = "page01.jpg";
        let b = "page1.jpg";
        assert_ne!(
            natural_cmp(a, b),
            Ordering::Equal,
            "not equal: order is ambiguous"
        );
        assert_eq!(natural_cmp(a, b), natural_cmp(a, b), "antisymmetric");

        // For a sample of names, no two distinct names compare equal.
        let sample = names(&[
            "a.jpg",
            "a1.jpg",
            "a01.jpg",
            "a10.jpg",
            "b.jpg",
            "10.jpg",
            "010.jpg",
            "img_2.jpg",
            "img_02.jpg",
            "img_20.jpg",
            "ch1.jpg",
            "ch1p1.jpg",
        ]);
        for x in &sample {
            for y in &sample {
                if x != y {
                    assert_ne!(
                        natural_cmp(x, y),
                        Ordering::Equal,
                        "{x:?} and {y:?} compare equal, so their order is undefined"
                    );
                }
            }
        }
    }

    /// And a fixed input must produce a fixed output, not merely a
    /// deterministic-within-one-run sort.
    #[test]
    fn sorting_is_stable_across_repeated_calls() {
        let input = names(&[
            "img_3.jpg",
            "img_1.jpg",
            "img_20.jpg",
            "img_2.jpg",
            "cover.jpg",
            "img_10.jpg",
        ]);
        let first = order_owned(&input);
        for _ in 0..20 {
            assert_eq!(order_owned(&input), first, "the sort is not deterministic");
        }
    }

    #[test]
    fn the_sort_does_not_lose_or_duplicate_a_page() {
        let input = names(&[
            "img_5.jpg",
            "img_1.jpg",
            "img_20.jpg",
            "img_2.jpg",
            "cover.jpg",
            "img_10.jpg",
        ]);
        let c = Comic::from_names(input);
        assert_eq!(c.len(), 6);
        let mut got: Vec<String> = c.pages.iter().map(|p| p.name.clone()).collect();
        got.sort();
        let mut want = got.clone();
        want.dedup();
        assert_eq!(got.len(), want.len(), "a page was duplicated");
    }

    /// A leading zero is not part of the value. Without this, `page007` and
    /// `page7` are different numbers and a comic using one style throughout
    /// with a stray zero somewhere gets its order wrong at that page.
    #[test]
    fn leading_zeros_are_not_part_of_the_value() {
        assert_eq!(
            natural_cmp("p007.jpg", "p7.jpg"),
            natural_cmp("p7.jpg", "p007.jpg").reverse()
        );
        assert!(natural_cmp("p007.jpg", "p8.jpg").is_lt());
        assert!(
            natural_cmp("p007.jpg", "p7.jpg") != Ordering::Equal,
            "but they still order"
        );
    }

    #[test]
    fn a_zero_digit_run_compares_as_zero() {
        assert_eq!(trim_zeros(b"0"), b"0", "zero must not trim to nothing");
        assert_eq!(trim_zeros(b"000"), b"0");
        assert_eq!(trim_zeros(b"007"), b"7");
        assert!(natural_cmp("p0.jpg", "p1.jpg").is_lt());
    }

    #[test]
    fn uppercase_and_lowercase_are_adjacent_rather_than_split_by_ascii() {
        // ASCII puts every uppercase letter before every lowercase one, so a
        // byte comparison puts "b.jpg" before "A.jpg". A person reads them as
        // adjacent.
        let o = order(&["b.jpg", "A.jpg", "a.jpg"]);
        let ia = o
            .iter()
            .position(|s| s.eq_ignore_ascii_case("a.jpg"))
            .unwrap();
        let ib = o.iter().position(|s| s == "b.jpg").unwrap();
        assert!(ia < ib, "{o:?}");
    }

    // ---- direction ----

    /// The parity flip IS the difference between the two directions, and it is
    /// the thing a renderer gets wrong: in LTR page 1 is on the right, in RTL
    /// it is on the left.
    #[test]
    fn the_first_page_is_a_recto_in_ltr_and_a_verso_in_rtl() {
        assert!(
            !ReadingDirection::LeftToRight.is_left_hand(0),
            "LTR page 1 is on the right"
        );
        assert!(
            ReadingDirection::RightToLeft.is_left_hand(0),
            "RTL page 1 is on the left"
        );
        assert!(ReadingDirection::LeftToRight.is_left_hand(1));
        assert!(!ReadingDirection::RightToLeft.is_left_hand(1));
    }

    #[test]
    fn left_hand_pages_alternate_in_both_directions() {
        for d in [
            ReadingDirection::LeftToRight,
            ReadingDirection::RightToLeft,
            ReadingDirection::VerticalRightToLeft,
        ] {
            for n in 0..20 {
                assert_ne!(
                    d.is_left_hand(n),
                    d.is_left_hand(n + 1),
                    "{d} page {n} does not alternate"
                );
            }
        }
    }

    /// Direction is a LAYOUT property, not a reordering of the stored pages.
    /// Reversing the page list would make every stored page index disagree with
    /// the archive, so every citation and every "page 12" reference would be
    /// wrong.
    #[test]
    fn changing_direction_does_not_reorder_the_pages() {
        let mut c = Comic::from_names(names(&["1.jpg", "2.jpg", "3.jpg"]));
        let before: Vec<String> = c.pages.iter().map(|p| p.name.clone()).collect();
        c.set_direction(ReadingDirection::RightToLeft);
        let after: Vec<String> = c.pages.iter().map(|p| p.name.clone()).collect();
        assert_eq!(before, after, "the file order must be untouched");
        assert_eq!(c.direction, ReadingDirection::RightToLeft);
        assert_eq!(
            c.at(0).unwrap().name,
            "1.jpg",
            "page 0 is still the first file"
        );
    }

    #[test]
    fn a_spread_starts_on_a_recto() {
        // A spread is the left page plus the right page, so it starts on a
        // right-hand page -- which is page 0 in LTR and page 1 in RTL.
        assert!(ReadingDirection::LeftToRight.is_spread_start(0));
        assert!(!ReadingDirection::LeftToRight.is_spread_start(1));
        assert!(!ReadingDirection::RightToLeft.is_spread_start(0));
        assert!(ReadingDirection::RightToLeft.is_spread_start(1));
    }

    // ---- the object ----

    #[test]
    fn a_comic_from_names_is_not_authoritative() {
        let c = Comic::from_names(names(&["2.jpg", "1.jpg"]));
        assert!(
            !c.order_is_authoritative,
            "the order was inferred from names"
        );
        assert_eq!(c.at(0).unwrap().name, "1.jpg");
    }

    /// A container that knows its own page order is trusted over filenames, and
    /// says so, so a re-scan does not re-infer and contradict it.
    #[test]
    fn a_container_supplied_order_is_authoritative_and_kept() {
        let pages = vec![
            Page {
                file_index: 0,
                name: "z.jpg".into(),
                width: 100,
                height: 200,
                is_spread: false,
            },
            Page {
                file_index: 1,
                name: "a.jpg".into(),
                width: 100,
                height: 200,
                is_spread: false,
            },
        ];
        let c = Comic::from_authoritative_order(pages, ReadingDirection::RightToLeft);
        assert!(c.order_is_authoritative);
        assert_eq!(
            c.at(0).unwrap().name,
            "z.jpg",
            "the container's order stands"
        );
        assert_eq!(c.direction, ReadingDirection::RightToLeft);
    }

    #[test]
    fn file_index_is_the_archive_order_and_is_not_rewritten_by_sorting() {
        let c = Comic::from_names(names(&["b.jpg", "a.jpg", "c.jpg"]));
        // Sorted by name: a, b, c. The file_index records where each came from
        // in the input, which is what lets a citation name the archive member.
        let by_name: Vec<(String, usize)> = c
            .pages
            .iter()
            .map(|p| (p.name.clone(), p.file_index))
            .collect();
        assert_eq!(
            by_name,
            vec![
                ("a.jpg".to_string(), 1),
                ("b.jpg".to_string(), 0),
                ("c.jpg".to_string(), 2),
            ]
        );
    }

    #[test]
    fn an_empty_comic_is_empty_not_a_one_page_comic() {
        let c = Comic::from_names(Vec::new());
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert!(c.cover().is_none());
        assert_eq!(c.sanity_check(), vec![ComicWarning::NoPages]);
    }

    #[test]
    fn the_cover_is_the_first_page_in_file_order() {
        let c = Comic::from_names(names(&["10.jpg", "1.jpg", "2.jpg"]));
        assert_eq!(c.cover().unwrap().name, "1.jpg");
    }

    #[test]
    fn an_index_past_the_end_is_none() {
        let c = Comic::from_names(names(&["1.jpg"]));
        assert!(c.at(0).is_some());
        assert!(c.at(1).is_none());
        assert!(c.at(9999).is_none());
    }

    #[test]
    fn a_single_page_comic_is_flagged() {
        let c = Comic::from_names(names(&["1.jpg"]));
        assert_eq!(c.sanity_check(), vec![ComicWarning::SinglePage]);
    }

    /// A zip of only double-page scans is a real thing (someone split a book
    /// wrong) and is unreadable without saying so.
    #[test]
    fn a_comic_of_only_spreads_is_flagged() {
        let pages: Vec<Page> = (0..4)
            .map(|i| Page {
                file_index: i,
                name: format!("{i}.jpg"),
                width: 2000,
                height: 1400,
                is_spread: true,
            })
            .collect();
        let c = Comic::from_authoritative_order(pages, ReadingDirection::default());
        let w = c.sanity_check();
        assert!(w.contains(&ComicWarning::AllSpreads), "{w:?}");
        assert!(w.contains(&ComicWarning::NoSingles), "{w:?}");
        assert_eq!(c.spread_count(), 4);
    }

    /// Mixed orientations with the wide ones not marked as spreads is the
    /// signature of a zip with unflagged double-page scans.
    #[test]
    fn mixed_orientations_are_flagged() {
        let pages = vec![
            Page {
                file_index: 0,
                name: "1.jpg".into(),
                width: 800,
                height: 1200,
                is_spread: false,
            },
            Page {
                file_index: 1,
                name: "2.jpg".into(),
                width: 2400,
                height: 1600,
                is_spread: false,
            },
        ];
        let c = Comic::from_authoritative_order(pages, ReadingDirection::default());
        assert!(c.sanity_check().contains(&ComicWarning::MixedOrientation));
    }

    #[test]
    fn a_normal_comic_produces_no_warnings() {
        let pages: Vec<Page> = (0..10)
            .map(|i| Page {
                file_index: i,
                name: format!("{i}.jpg"),
                width: 800,
                height: 1200,
                is_spread: false,
            })
            .collect();
        let c = Comic::from_authoritative_order(pages, ReadingDirection::default());
        assert_eq!(c.sanity_check(), Vec::<ComicWarning>::new());
    }

    #[test]
    fn an_unknown_dimension_is_not_treated_as_landscape() {
        // A page whose dimensions were never probed has width == height == 0.
        // Counting that as landscape would flag every unprobed comic.
        let c = Comic::from_names(names(&["1.jpg", "2.jpg", "3.jpg"]));
        assert!(!c.sanity_check().contains(&ComicWarning::MixedOrientation));
    }

    // ---- round trips ----

    #[test]
    fn a_comic_round_trips_through_json() {
        let mut c = Comic::from_names(names(&["2.jpg", "10.jpg", "1.jpg"]));
        c.set_direction(ReadingDirection::RightToLeft);
        let json = serde_json::to_string(&c).unwrap();
        let back: Comic = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn direction_round_trips_through_json() {
        for d in [
            ReadingDirection::LeftToRight,
            ReadingDirection::RightToLeft,
            ReadingDirection::VerticalRightToLeft,
        ] {
            let json = serde_json::to_string(&d).unwrap();
            let back: ReadingDirection = serde_json::from_str(&json).unwrap();
            assert_eq!(d, back);
        }
    }

    #[test]
    fn direction_has_a_stable_string_form() {
        assert_eq!(ReadingDirection::LeftToRight.to_string(), "ltr");
        assert_eq!(ReadingDirection::RightToLeft.to_string(), "rtl");
        assert_eq!(
            ReadingDirection::VerticalRightToLeft.to_string(),
            "vertical-rtl"
        );
    }

    /// The property that makes natural_cmp a *natural* sort, checked against
    /// the implementation rather than against a list of examples: for any two
    /// names whose digit runs are the same, the comparison must agree with
    /// comparing those runs as integers.
    ///
    /// This is the test that would have caught the first version of this
    /// comparator, which reduced every comparison to a plain string compare
    /// because it stopped at the first non-digit. Examples did not catch it --
    /// they caught it, but only because they were written with the same wrong
    /// expectation. A property over the digit runs does not share that blind
    /// spot.
    #[test]
    fn a_digit_run_compares_as_the_number_it_spells() {
        for (x, y) in [
            (2u32, 10u32),
            (3, 21),
            (9, 100),
            (10, 9),
            (7, 70),
            (100, 21),
        ] {
            assert_eq!(
                natural_cmp(&format!("p{x}.jpg"), &format!("p{y}.jpg")),
                x.cmp(&y),
                "p{x} vs p{y}"
            );
        }
        // Leading zeros must not change the value, in either direction.
        for (x, y) in [(2u32, 10u32), (3, 21), (9, 100)] {
            assert_eq!(
                natural_cmp(&format!("p{x:03}.jpg"), &format!("p{y}.jpg")),
                x.cmp(&y),
                "p{x:03} vs p{y}"
            );
        }
    }

    /// A natural sort is what `ls -v` does, and the corpus is full of names
    /// where the only difference is the width of a number. Asserting the
    /// property over a generated set of widths is stronger than listing cases.
    #[test]
    fn wider_numbers_sort_later_at_every_width() {
        for width in 1..6 {
            for base in 1..40u32 {
                let wide = base * 10 + 5;
                let narrow = base;
                assert_eq!(
                    natural_cmp(&format!("{wide:0width$}"), &format!("{narrow:0width$}")),
                    wide.cmp(&narrow),
                    "{wide:0width$} vs {narrow:0width$}"
                );
            }
        }
    }

    /// A warning is shown to a user, so it has to be a phrase they can read
    /// and act on rather than a debug string. The test is that each one is a
    /// lowercase phrase naming a condition -- not that it is punctuated, which
    /// is a style choice and not the property worth pinning.
    #[test]
    fn warnings_render_as_readable_phrases() {
        let all = [
            ComicWarning::NoPages,
            ComicWarning::SinglePage,
            ComicWarning::AllSpreads,
            ComicWarning::NoSingles,
            ComicWarning::MixedOrientation,
        ];
        for w in all {
            let s = w.to_string();
            assert!(!s.is_empty(), "{w:?} renders as nothing");
            assert!(
                s.chars().all(|c| !c.is_ascii_uppercase()),
                "{s:?} should read as a phrase, not a label"
            );
            assert!(
                s.split_whitespace().count() >= 3,
                "{s:?} is too terse to act on"
            );
        }
        // And they are distinguishable from one another, or the UI shows the
        // same message for two different problems.
        let rendered: Vec<String> = all.iter().map(|w| w.to_string()).collect();
        let mut sorted = rendered.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), rendered.len(), "two warnings render the same");
    }
}

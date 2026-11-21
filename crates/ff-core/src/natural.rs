//! Natural ("human") ordering: `img2` sorts before `img10`, comparison is
//! case-insensitive and Unicode-aware.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::iter::Peekable;

/// Compare two strings naturally. Digit runs compare by numeric value
/// (arbitrary length, no overflow), everything else compares by lowercase
/// Unicode scalar values. Ties are broken by an exact comparison so the
/// ordering is total and deterministic.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().flat_map(char::to_lowercase).peekable();
    let mut bi = b.chars().flat_map(char::to_lowercase).peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na = take_number(&mut ai);
                    let nb = take_number(&mut bi);
                    let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(&nb));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    if ca != cb {
                        return ca.cmp(&cb);
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

/// Natural comparison for file names that may not be valid UTF-8.
pub fn natural_cmp_os(a: &OsStr, b: &OsStr) -> Ordering {
    natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()).then_with(|| a.cmp(b))
}

/// Consume a run of ASCII digits, returning it without leading zeros.
fn take_number<I: Iterator<Item = char>>(it: &mut Peekable<I>) -> String {
    let mut digits = String::new();
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        if !(digits.is_empty() && c == '0') {
            digits.push(c);
        }
        it.next();
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn numbers_sort_by_value() {
        assert_eq!(
            sorted(vec!["img10.jpg", "img2.jpg", "img1.jpg", "img100.jpg"]),
            vec!["img1.jpg", "img2.jpg", "img10.jpg", "img100.jpg"]
        );
        assert_eq!(
            sorted(vec!["Day10", "Day2", "Day1"]),
            vec!["Day1", "Day2", "Day10"]
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(
            sorted(vec!["b.png", "A.png", "a.png", "C.png"]),
            vec!["A.png", "a.png", "b.png", "C.png"]
        );
    }

    #[test]
    fn leading_zeros_and_huge_numbers() {
        assert_eq!(natural_cmp("a007", "a7"), Ordering::Less); // equal value, exact tie-break
        assert_eq!(natural_cmp("a08", "a9"), Ordering::Less);
        assert_eq!(
            natural_cmp(
                "x99999999999999999999999999",
                "x100000000000000000000000000"
            ),
            Ordering::Less
        );
    }

    #[test]
    fn unicode_names() {
        assert_eq!(
            sorted(vec!["写真10.jpg", "写真2.jpg", "Écran.png", "école.png"]),
            vec!["école.png", "Écran.png", "写真2.jpg", "写真10.jpg"]
        );
        assert_eq!(natural_cmp("my photo 2", "my photo 10"), Ordering::Less);
    }

    #[test]
    fn prefix_is_smaller() {
        assert_eq!(natural_cmp("img", "img1"), Ordering::Less);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
    }
}

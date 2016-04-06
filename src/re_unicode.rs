// Copyright 2014-2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Index;
use std::str::FromStr;
use std::sync::Arc;

use syntax;

use exec::{Exec, ExecNoSyncStr, ExecBuilder};
use error::Error;
use re_plugin::Plugin;
use re_trait::{self, RegularExpression};

/// Escapes all regular expression meta characters in `text`.
///
/// The string returned may be safely used as a literal in a regular
/// expression.
pub fn quote(text: &str) -> String {
    syntax::quote(text)
}

/// Tests if the given regular expression matches somewhere in the text given.
///
/// If there was a problem compiling the regular expression, an error is
/// returned.
///
/// To find submatches, split or replace text, you'll need to compile an
/// expression first.
pub fn is_match(regex: &str, text: &str) -> Result<bool, Error> {
    Regex::new(regex).map(|r| r.is_match(text))
}

/// A compiled regular expression for matching Unicode strings.
///
/// It is represented as either a sequence of bytecode instructions (dynamic)
/// or as a specialized Rust function (native). It can be used to search, split
/// or replace text. All searching is done with an implicit `.*?` at the
/// beginning and end of an expression. To force an expression to match the
/// whole string (or a prefix or a suffix), you must use an anchor like `^` or
/// `$` (or `\A` and `\z`).
///
/// While this crate will handle Unicode strings (whether in the regular
/// expression or in the search text), all positions returned are **byte
/// indices**. Every byte index is guaranteed to be at a Unicode code point
/// boundary.
///
/// The lifetimes `'r` and `'t` in this crate correspond to the lifetime of a
/// compiled regular expression and text to search, respectively.
///
/// The only methods that allocate new strings are the string replacement
/// methods. All other methods (searching and splitting) return borrowed
/// pointers into the string given.
///
/// # Examples
///
/// Find the location of a US phone number:
///
/// ```rust
/// # use regex::Regex;
/// let re = Regex::new("[0-9]{3}-[0-9]{3}-[0-9]{4}").unwrap();
/// assert_eq!(re.find("phone: 111-222-3333"), Some((7, 19)));
/// ```
///
/// # Using the `std::str::StrExt` methods with `Regex`
///
/// > **Note**: This section requires that this crate is currently compiled with
/// >           the `pattern` Cargo feature enabled.
///
/// Since `Regex` implements `Pattern`, you can use regexes with methods
/// defined on `std::str::StrExt`. For example, `is_match`, `find`, `find_iter`
/// and `split` can be replaced with `StrExt::contains`, `StrExt::find`,
/// `StrExt::match_indices` and `StrExt::split`.
///
/// Here are some examples:
///
/// ```rust,ignore
/// # use regex::Regex;
/// let re = Regex::new(r"\d+").unwrap();
/// let haystack = "a111b222c";
///
/// assert!(haystack.contains(&re));
/// assert_eq!(haystack.find(&re), Some(1));
/// assert_eq!(haystack.match_indices(&re).collect::<Vec<_>>(),
///            vec![(1, 4), (5, 8)]);
/// assert_eq!(haystack.split(&re).collect::<Vec<_>>(), vec!["a", "b", "c"]);
/// ```
#[derive(Clone)]
pub struct Regex(#[doc(hidden)] pub _Regex);

#[derive(Clone)]
#[doc(hidden)]
pub enum _Regex {
    // The representation of `Regex` is exported to support the `regex!`
    // syntax extension. Do not rely on it.
    //
    // See the comments for the `program` module in `lib.rs` for a more
    // detailed explanation for what `regex!` requires.
    #[doc(hidden)]
    Dynamic(Exec),
    #[doc(hidden)]
    Plugin(Plugin),
}

impl fmt::Display for Regex {
    /// Shows the original regular expression.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl fmt::Debug for Regex {
    /// Shows the original regular expression.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[doc(hidden)]
impl From<Exec> for Regex {
    fn from(exec: Exec) -> Regex {
        Regex(_Regex::Dynamic(exec))
    }
}

/// Equality comparison is based on the original string. It is possible that
/// different regular expressions have the same matching behavior, but are
/// still compared unequal. For example, `\d+` and `\d\d*` match the same set
/// of strings, but are not considered equal.
impl PartialEq for Regex {
    fn eq(&self, other: &Regex) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Regex {}

impl FromStr for Regex {
    type Err = Error;

    /// Attempts to parse a string into a regular expression
    fn from_str(s: &str) -> Result<Regex, Error> {
        Regex::new(s)
    }
}

impl Regex {
    /// Compiles a regular expression. Once compiled, it can be used repeatedly
    /// to search, split or replace text in a string.
    ///
    /// If an invalid expression is given, then an error is returned.
    pub fn new(re: &str) -> Result<Regex, Error> {
        Regex::with_size_limit(10 * (1 << 20), re)
    }

    /// Compiles a regular expression with the given size limit.
    ///
    /// The size limit is applied to the size of the *compiled* data structure.
    /// If the data structure exceeds the size given, then an error is
    /// returned.
    pub fn with_size_limit(size: usize, re: &str) -> Result<Regex, Error> {
        ExecBuilder::new(re).size_limit(size).build().map(Regex::from)
    }

    /// Returns true if and only if the regex matches the string given.
    ///
    /// It is recommended to use this method if all you need to do is test
    /// a match, since the underlying matching engine may be able to do less
    /// work.
    ///
    /// # Example
    ///
    /// Test if some text contains at least one word with exactly 13
    /// Unicode word characters:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let text = "I categorically deny having triskaidekaphobia.";
    /// assert!(Regex::new(r"\b\w{13}\b").unwrap().is_match(text));
    /// # }
    /// ```
    pub fn is_match(&self, text: &str) -> bool {
        self.shortest_match(text).is_some()
    }

    /// Returns the start and end byte range of the leftmost-first match in
    /// `text`. If no match exists, then `None` is returned.
    ///
    /// Note that this should only be used if you want to discover the position
    /// of the match. Testing the existence of a match is faster if you use
    /// `is_match`.
    ///
    /// # Example
    ///
    /// Find the start and end location of the first word with exactly 13
    /// Unicode word characters:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let text = "I categorically deny having triskaidekaphobia.";
    /// let pos = Regex::new(r"\b\w{13}\b").unwrap().find(text);
    /// assert_eq!(pos, Some((2, 15)));
    /// # }
    /// ```
    pub fn find(&self, text: &str) -> Option<(usize, usize)> {
        match self.0 {
            _Regex::Dynamic(ref exec) => {
                exec.searcher_str().find_at(text, 0)
            }
            _Regex::Plugin(ref plug) => plug.find_at(text, 0),
        }
    }

    /// Returns an iterator for each successive non-overlapping match in
    /// `text`, returning the start and end byte indices with respect to
    /// `text`.
    ///
    /// # Example
    ///
    /// Find the start and end location of every word with exactly 13 Unicode
    /// word characters:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let text = "Retroactively relinquishing remunerations is reprehensible.";
    /// for pos in Regex::new(r"\b\w{13}\b").unwrap().find_iter(text) {
    ///     println!("{:?}", pos);
    /// }
    /// // Output:
    /// // (0, 13)
    /// // (14, 27)
    /// // (28, 41)
    /// // (45, 58)
    /// # }
    /// ```
    pub fn find_iter<'r, 't>(&'r self, text: &'t str) -> FindMatches<'r, 't> {
        match self.0 {
            _Regex::Dynamic(ref exec) => {
                let it = exec.searcher_str().find_iter(text);
                FindMatches(FindMatchesInner::Dynamic(it))
            }
            _Regex::Plugin(ref plug) => {
                let it = plug.find_iter(text);
                FindMatches(FindMatchesInner::Plugin(it))
            }
        }
    }

    /// Returns the capture groups corresponding to the leftmost-first
    /// match in `text`. Capture group `0` always corresponds to the entire
    /// match. If no match is found, then `None` is returned.
    ///
    /// You should only use `captures` if you need access to submatches.
    /// Otherwise, `find` is faster for discovering the location of the overall
    /// match.
    ///
    /// # Examples
    ///
    /// Say you have some text with movie names and their release years,
    /// like "'Citizen Kane' (1941)". It'd be nice if we could search for text
    /// looking like that, while also extracting the movie name and its release
    /// year separately.
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let re = Regex::new(r"'([^']+)'\s+\((\d{4})\)").unwrap();
    /// let text = "Not my favorite movie: 'Citizen Kane' (1941).";
    /// let caps = re.captures(text).unwrap();
    /// assert_eq!(caps.at(1), Some("Citizen Kane"));
    /// assert_eq!(caps.at(2), Some("1941"));
    /// assert_eq!(caps.at(0), Some("'Citizen Kane' (1941)"));
    /// // You can also access the groups by index using the Index notation.
    /// // Note that this will panic on an invalid index.
    /// assert_eq!(&caps[1], "Citizen Kane");
    /// assert_eq!(&caps[2], "1941");
    /// assert_eq!(&caps[0], "'Citizen Kane' (1941)");
    /// # }
    /// ```
    ///
    /// Note that the full match is at capture group `0`. Each subsequent
    /// capture group is indexed by the order of its opening `(`.
    ///
    /// We can make this example a bit clearer by using *named* capture groups:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let re = Regex::new(r"'(?P<title>[^']+)'\s+\((?P<year>\d{4})\)")
    ///                .unwrap();
    /// let text = "Not my favorite movie: 'Citizen Kane' (1941).";
    /// let caps = re.captures(text).unwrap();
    /// assert_eq!(caps.name("title"), Some("Citizen Kane"));
    /// assert_eq!(caps.name("year"), Some("1941"));
    /// assert_eq!(caps.at(0), Some("'Citizen Kane' (1941)"));
    /// // You can also access the groups by name using the Index notation.
    /// // Note that this will panic on an invalid group name.
    /// assert_eq!(&caps["title"], "Citizen Kane");
    /// assert_eq!(&caps["year"], "1941");
    /// assert_eq!(&caps[0], "'Citizen Kane' (1941)");
    ///
    /// # }
    /// ```
    ///
    /// Here we name the capture groups, which we can access with the `name`
    /// method or the `Index` notation with a `&str`. Note that the named
    /// capture groups are still accessible with `at` or the `Index` notation
    /// with a `usize`.
    ///
    /// The `0`th capture group is always unnamed, so it must always be
    /// accessed with `at(0)` or `[0]`.
    pub fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        let mut slots = vec![None; 2 * self.captures_len()];
        let result = match self.0 {
            _Regex::Dynamic(ref exec) => {
                exec.searcher_str().captures_at(&mut slots, text, 0)
            }
            _Regex::Plugin(ref plug) => plug.captures_at(&mut slots, text, 0),
        };
        result.map(|_| Captures {
            text: text,
            slots: slots,
            named_groups: NamedGroups::from_regex(self)
        })
    }

    /// Returns an iterator over all the non-overlapping capture groups matched
    /// in `text`. This is operationally the same as `find_iter`, except it
    /// yields information about submatches.
    ///
    /// # Example
    ///
    /// We can use this to find all movie titles and their release years in
    /// some text, where the movie is formatted like "'Title' (xxxx)":
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let re = Regex::new(r"'(?P<title>[^']+)'\s+\((?P<year>\d{4})\)")
    ///                .unwrap();
    /// let text = "'Citizen Kane' (1941), 'The Wizard of Oz' (1939), 'M' (1931).";
    /// for caps in re.captures_iter(text) {
    ///     println!("Movie: {:?}, Released: {:?}",
    ///              caps.name("title"), caps.name("year"));
    /// }
    /// // Output:
    /// // Movie: Citizen Kane, Released: 1941
    /// // Movie: The Wizard of Oz, Released: 1939
    /// // Movie: M, Released: 1931
    /// # }
    /// ```
    pub fn captures_iter<'r, 't>(
        &'r self,
        text: &'t str,
    ) -> FindCaptures<'r, 't> {
        match self.0 {
            _Regex::Dynamic(ref exec) => {
                let it = exec.searcher_str().captures_iter(text);
                FindCaptures(FindCapturesInner::Dynamic(it))
            }
            _Regex::Plugin(ref plug) => {
                let it = plug.captures_iter(text);
                FindCaptures(FindCapturesInner::Plugin(it))
            }
        }
    }

    /// Returns an iterator of substrings of `text` delimited by a match of the
    /// regular expression. Namely, each element of the iterator corresponds to
    /// text that *isn't* matched by the regular expression.
    ///
    /// This method will *not* copy the text given.
    ///
    /// # Example
    ///
    /// To split a string delimited by arbitrary amounts of spaces or tabs:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let re = Regex::new(r"[ \t]+").unwrap();
    /// let fields: Vec<&str> = re.split("a b \t  c\td    e").collect();
    /// assert_eq!(fields, vec!["a", "b", "c", "d", "e"]);
    /// # }
    /// ```
    pub fn split<'r, 't>(&'r self, text: &'t str) -> RegexSplits<'r, 't> {
        RegexSplits {
            finder: self.find_iter(text),
            last: 0,
        }
    }

    /// Returns an iterator of at most `limit` substrings of `text` delimited
    /// by a match of the regular expression. (A `limit` of `0` will return no
    /// substrings.) Namely, each element of the iterator corresponds to text
    /// that *isn't* matched by the regular expression. The remainder of the
    /// string that is not split will be the last element in the iterator.
    ///
    /// This method will *not* copy the text given.
    ///
    /// # Example
    ///
    /// Get the first two words in some text:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let re = Regex::new(r"\W+").unwrap();
    /// let fields: Vec<&str> = re.splitn("Hey! How are you?", 3).collect();
    /// assert_eq!(fields, vec!("Hey", "How", "are you?"));
    /// # }
    /// ```
    pub fn splitn<'r, 't>(&'r self, text: &'t str, limit: usize)
                         -> RegexSplitsN<'r, 't> {
        RegexSplitsN {
            splits: self.split(text),
            n: limit,
        }
    }

    /// Replaces the leftmost-first match with the replacement provided.
    /// The replacement can be a regular string (where `$N` and `$name` are
    /// expanded to match capture groups) or a function that takes the matches'
    /// `Captures` and returns the replaced string.
    ///
    /// If no match is found, then a copy of the string is returned unchanged.
    ///
    /// # Examples
    ///
    /// Note that this function is polymorphic with respect to the replacement.
    /// In typical usage, this can just be a normal string:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let re = Regex::new("[^01]+").unwrap();
    /// assert_eq!(re.replace("1078910", ""), "1010");
    /// # }
    /// ```
    ///
    /// But anything satisfying the `Replacer` trait will work. For example,
    /// a closure of type `|&Captures| -> String` provides direct access to the
    /// captures corresponding to a match. This allows one to access
    /// submatches easily:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # use regex::Captures; fn main() {
    /// let re = Regex::new(r"([^,\s]+),\s+(\S+)").unwrap();
    /// let result = re.replace("Springsteen, Bruce", |caps: &Captures| {
    ///     format!("{} {}", caps.at(2).unwrap_or(""), caps.at(1).unwrap_or(""))
    /// });
    /// assert_eq!(result, "Bruce Springsteen");
    /// # }
    /// ```
    ///
    /// But this is a bit cumbersome to use all the time. Instead, a simple
    /// syntax is supported that expands `$name` into the corresponding capture
    /// group. Here's the last example, but using this expansion technique
    /// with named capture groups:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let re = Regex::new(r"(?P<last>[^,\s]+),\s+(?P<first>\S+)").unwrap();
    /// let result = re.replace("Springsteen, Bruce", "$first $last");
    /// assert_eq!(result, "Bruce Springsteen");
    /// # }
    /// ```
    ///
    /// Note that using `$2` instead of `$first` or `$1` instead of `$last`
    /// would produce the same result. To write a literal `$` use `$$`.
    ///
    /// Finally, sometimes you just want to replace a literal string with no
    /// submatch expansion. This can be done by wrapping a string with
    /// `NoExpand`:
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// use regex::NoExpand;
    ///
    /// let re = Regex::new(r"(?P<last>[^,\s]+),\s+(\S+)").unwrap();
    /// let result = re.replace("Springsteen, Bruce", NoExpand("$2 $last"));
    /// assert_eq!(result, "$2 $last");
    /// # }
    /// ```
    pub fn replace<R: Replacer>(&self, text: &str, rep: R) -> String {
        self.replacen(text, 1, rep)
    }

    /// Replaces all non-overlapping matches in `text` with the replacement
    /// provided. This is the same as calling `replacen` with `limit` set to
    /// `0`.
    ///
    /// See the documentation for `replace` for details on how to access
    /// submatches in the replacement string.
    pub fn replace_all<R: Replacer>(&self, text: &str, rep: R) -> String {
        self.replacen(text, 0, rep)
    }

    /// Replaces at most `limit` non-overlapping matches in `text` with the
    /// replacement provided. If `limit` is 0, then all non-overlapping matches
    /// are replaced.
    ///
    /// See the documentation for `replace` for details on how to access
    /// submatches in the replacement string.
    pub fn replacen<R: Replacer>(
        &self,
        text: &str,
        limit: usize,
        mut rep: R,
    ) -> String {

        // If we know that the replacement doesn't have any capture expansions,
        // then we can fast path. The fast path can make a tremendous
        // difference:
        //
        //   1) We use `find_iter` instead of `captures_iter`. Not asking for
        //      captures generally makes the regex engines faster.
        //   2) We don't need to look up all of the capture groups and do
        //      replacements inside the replacement string. We just push it
        //      at each match and be done with it.
        if let Some(rep) = rep.no_expand() {
            let mut new = String::with_capacity(text.len());
            let mut last_match = 0;
            for (i, (s, e)) in self.find_iter(text).enumerate() {
                if limit > 0 && i >= limit {
                    break
                }
                new.push_str(&text[last_match..s]);
                new.push_str(&rep);
                last_match = e;
            }
            new.push_str(&text[last_match..]);
            return new;
        }

        // The slower path, which we use if the replacement needs access to
        // capture groups.
        let mut new = String::with_capacity(text.len());
        let mut last_match = 0;
        for (i, cap) in self.captures_iter(text).enumerate() {
            if limit > 0 && i >= limit {
                break
            }
            // unwrap on 0 is OK because captures only reports matches
            let (s, e) = cap.pos(0).unwrap();
            new.push_str(&text[last_match..s]);
            new.push_str(&rep.reg_replace(&cap));
            last_match = e;
        }
        new.push_str(&text[last_match..]);
        new
    }

    /// Returns the end location of a match in the text given.
    ///
    /// This method may have the same performance characteristics as
    /// `is_match`, except it provides an end location for a match. In
    /// particular, the location returned *may be shorter* than the proper end
    /// of the leftmost-first match.
    ///
    /// # Example
    ///
    /// Typically, `a+` would match the entire first sequence of `a` in some
    /// text, but `shortest_match` can give up as soon as it sees the first
    /// `a`.
    ///
    /// ```rust
    /// # extern crate regex; use regex::Regex;
    /// # fn main() {
    /// let text = "aaaaa";
    /// let pos = Regex::new(r"a+").unwrap().shortest_match(text);
    /// assert_eq!(pos, Some(1));
    /// # }
    /// ```
    pub fn shortest_match(&self, text: &str) -> Option<usize> {
        match self.0 {
            _Regex::Dynamic(ref exec) => {
                exec.searcher_str().shortest_match_at(text, 0)
            }
            _Regex::Plugin(ref plug) => plug.shortest_match_at(text, 0),
        }
    }

    /// Returns the original string of this regex.
    pub fn as_str(&self) -> &str {
        match self.0 {
            _Regex::Dynamic(ref exec) => &exec.regex_strings()[0],
            _Regex::Plugin(ref plug) => &plug.original,
        }
    }

    /// Returns an iterator over the capture names.
    pub fn capture_names(&self) -> CaptureNames {
        CaptureNames(match self.0 {
            _Regex::Plugin(ref n) => _CaptureNames::Plugin(n.names.iter()),
            _Regex::Dynamic(ref d) => {
                _CaptureNames::Dynamic(d.capture_names().iter())
            }
        })
    }

    /// Returns the number of captures.
    pub fn captures_len(&self) -> usize {
        match self.0 {
            _Regex::Plugin(ref n) => n.names.len(),
            _Regex::Dynamic(ref d) => d.capture_names().len()
        }
    }
}

/// An iterator over the names of all possible captures.
///
/// `None` indicates an unnamed capture; the first element (capture 0, the
/// whole matched region) is always unnamed.
///
/// `'r` is the lifetime of the compiled regular expression.
pub struct CaptureNames<'r>(_CaptureNames<'r>);

enum _CaptureNames<'r> {
    #[doc(hidden)]
    Plugin(::std::slice::Iter<'r, Option<&'static str>>),
    #[doc(hidden)]
    Dynamic(::std::slice::Iter<'r, Option<String>>)
}

impl<'r> Iterator for CaptureNames<'r> {
    type Item = Option<&'r str>;

    fn next(&mut self) -> Option<Option<&'r str>> {
        match self.0 {
            _CaptureNames::Plugin(ref mut i) => i.next().cloned(),
            _CaptureNames::Dynamic(ref mut i) => {
                i.next().as_ref().map(|o| o.as_ref().map(|s| s.as_ref()))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self.0 {
            _CaptureNames::Plugin(ref i)  => i.size_hint(),
            _CaptureNames::Dynamic(ref i) => i.size_hint(),
        }
    }
}

/// NoExpand indicates literal string replacement.
///
/// It can be used with `replace` and `replace_all` to do a literal
/// string replacement without expanding `$name` to their corresponding
/// capture groups.
///
/// `'t` is the lifetime of the literal text.
pub struct NoExpand<'t>(pub &'t str);

/// Replacer describes types that can be used to replace matches in a string.
pub trait Replacer {
    /// Returns a possibly owned string that is used to replace the match
    /// corresponding to the `caps` capture group.
    ///
    /// The `'a` lifetime refers to the lifetime of a borrowed string when
    /// a new owned string isn't needed (e.g., for `NoExpand`).
    fn reg_replace(&mut self, caps: &Captures) -> Cow<str>;

    /// Returns a possibly owned string that never needs expansion.
    fn no_expand(&mut self) -> Option<Cow<str>> { None }
}

impl<'t> Replacer for NoExpand<'t> {
    fn reg_replace(&mut self, _: &Captures) -> Cow<str> {
        self.0.into()
    }

    fn no_expand(&mut self) -> Option<Cow<str>> {
        Some(self.0.into())
    }
}

impl<'t> Replacer for &'t str {
    fn reg_replace<'a>(&'a mut self, caps: &Captures) -> Cow<'a, str> {
        caps.expand(*self).into()
    }

    fn no_expand(&mut self) -> Option<Cow<str>> {
        // if there is a $ there may be an expansion
        match self.find('$') {
            Some(_) => None,
            None => Some((*self).into()),
        }
    }
}

impl<F> Replacer for F where F: FnMut(&Captures) -> String {
    fn reg_replace<'a>(&'a mut self, caps: &Captures) -> Cow<'a, str> {
        (*self)(caps).into()
    }
}

/// Yields all substrings delimited by a regular expression match.
///
/// `'r` is the lifetime of the compiled regular expression and `'t` is the
/// lifetime of the string being split.
pub struct RegexSplits<'r, 't> {
    finder: FindMatches<'r, 't>,
    last: usize,
}

impl<'r, 't> Iterator for RegexSplits<'r, 't> {
    type Item = &'t str;

    fn next(&mut self) -> Option<&'t str> {
        let text = self.finder.text();
        match self.finder.next() {
            None => {
                if self.last >= text.len() {
                    None
                } else {
                    let s = &text[self.last..];
                    self.last = text.len();
                    Some(s)
                }
            }
            Some((s, e)) => {
                let matched = &text[self.last..s];
                self.last = e;
                Some(matched)
            }
        }
    }
}

/// Yields at most `N` substrings delimited by a regular expression match.
///
/// The last substring will be whatever remains after splitting.
///
/// `'r` is the lifetime of the compiled regular expression and `'t` is the
/// lifetime of the string being split.
pub struct RegexSplitsN<'r, 't> {
    splits: RegexSplits<'r, 't>,
    n: usize,
}

impl<'r, 't> Iterator for RegexSplitsN<'r, 't> {
    type Item = &'t str;

    fn next(&mut self) -> Option<&'t str> {
        if self.n == 0 {
            return None
        }
        self.n -= 1;
        if self.n == 0 {
            let text = self.splits.finder.text();
            Some(&text[self.splits.last..])
        } else {
            self.splits.next()
        }
    }
}

enum NamedGroups {
    Plugin(&'static [(&'static str, usize)]),
    Dynamic(Arc<HashMap<String, usize>>),
}

impl NamedGroups {
    fn from_regex(regex: &Regex) -> NamedGroups {
        match regex.0 {
            _Regex::Plugin(ref plug) => NamedGroups::Plugin(&plug.groups),
            _Regex::Dynamic(ref exec) => {
                NamedGroups::Dynamic(exec.capture_name_idx().clone())
            }
        }
    }

    fn pos(&self, name: &str) -> Option<usize> {
        match *self {
            NamedGroups::Plugin(groups) => {
                groups.binary_search_by(|&(n, _)| n.cmp(name))
                      .ok().map(|i| groups[i].1)
            },
            NamedGroups::Dynamic(ref groups) => {
                groups.get(name).map(|i| *i)
            },
        }
    }

    fn iter<'n>(&'n self) -> NamedGroupsIter<'n> {
        match *self {
            NamedGroups::Plugin(g) => NamedGroupsIter::Plugin(g.iter()),
            NamedGroups::Dynamic(ref g) => NamedGroupsIter::Dynamic(g.iter()),
        }
    }
}

enum NamedGroupsIter<'n> {
    Plugin(::std::slice::Iter<'static, (&'static str, usize)>),
    Dynamic(::std::collections::hash_map::Iter<'n, String, usize>),
}

impl<'n> Iterator for NamedGroupsIter<'n> {
    type Item = (&'n str, usize);

    fn next(&mut self) -> Option<Self::Item> {
        match *self {
            NamedGroupsIter::Plugin(ref mut it) => it.next().map(|&v| v),
            NamedGroupsIter::Dynamic(ref mut it) => {
                it.next().map(|(s, i)| (s.as_ref(), *i))
            }
        }
    }
}

/// Captures represents a group of captured strings for a single match.
///
/// The 0th capture always corresponds to the entire match. Each subsequent
/// index corresponds to the next capture group in the regex. If a capture
/// group is named, then the matched string is *also* available via the `name`
/// method. (Note that the 0th capture is always unnamed and so must be
/// accessed with the `at` method.)
///
/// Positions returned from a capture group are always byte indices.
///
/// `'t` is the lifetime of the matched text.
pub struct Captures<'t> {
    text: &'t str,
    slots: Vec<Option<usize>>,
    named_groups: NamedGroups,
}

impl<'t> Captures<'t> {
    /// Returns the start and end positions of the Nth capture group. Returns
    /// `None` if `i` is not a valid capture group or if the capture group did
    /// not match anything. The positions returned are *always* byte indices
    /// with respect to the original string matched.
    pub fn pos(&self, i: usize) -> Option<(usize, usize)> {
        let (s, e) = (i * 2, i * 2 + 1);
        match (self.slots.get(s), self.slots.get(e)) {
            (Some(&Some(s)), Some(&Some(e))) => Some((s, e)),
            _ => None,
        }
    }

    /// Returns the matched string for the capture group `i`.  If `i` isn't
    /// a valid capture group or didn't match anything, then `None` is
    /// returned.
    pub fn at(&self, i: usize) -> Option<&'t str> {
        match self.pos(i) {
            None => None,
            Some((s, e)) => Some(&self.text[s..e])
        }
    }

    /// Returns the matched string for the capture group named `name`.  If
    /// `name` isn't a valid capture group or didn't match anything, then
    /// `None` is returned.
    pub fn name(&self, name: &str) -> Option<&'t str> {
        self.named_groups.pos(name).and_then(|i| self.at(i))
    }

    /// Creates an iterator of all the capture groups in order of appearance
    /// in the regular expression.
    pub fn iter(&'t self) -> SubCaptures<'t> {
        SubCaptures { idx: 0, caps: self, }
    }

    /// Creates an iterator of all the capture group positions in order of
    /// appearance in the regular expression. Positions are byte indices
    /// in terms of the original string matched.
    pub fn iter_pos(&'t self) -> SubCapturesPos<'t> {
        SubCapturesPos { idx: 0, slots: &self.slots }
    }

    /// Creates an iterator of all named groups as an tuple with the group
    /// name and the value. The iterator returns these values in arbitrary
    /// order.
    pub fn iter_named(&'t self) -> SubCapturesNamed<'t> {
        SubCapturesNamed {
            caps: self,
            names: self.named_groups.iter()
        }
    }

    /// Expands all instances of `$name` in `text` to the corresponding capture
    /// group `name`.
    ///
    /// `name` may be an integer corresponding to the index of the
    /// capture group (counted by order of opening parenthesis where `0` is the
    /// entire match) or it can be a name (consisting of letters, digits or
    /// underscores) corresponding to a named capture group.
    ///
    /// If `name` isn't a valid capture group (whether the name doesn't exist or
    /// isn't a valid index), then it is replaced with the empty string.
    ///
    /// To write a literal `$` use `$$`.
    pub fn expand(&self, text: &str) -> String {
        const REPLACE_EXPAND: &'static str = r"(?x)
          (?P<before>^|\b|[^$]) # Ignore `$$name`.
          \$
          (?P<name> # Match the actual capture name. Can be...
            [0-9]+  # A sequence of digits (for indexed captures), or...
            |
            [_a-zA-Z][_0-9a-zA-Z]* # A name for named captures.
          )
        ";
        // How evil can you get?
        let re = Regex::new(REPLACE_EXPAND).unwrap();
        let text = re.replace_all(text, |refs: &Captures| -> String {
            let before = refs.name("before").unwrap_or("");
            let name = refs.name("name").unwrap_or("");
            format!("{}{}", before, match name.parse::<usize>() {
                Err(_) => self.name(name).unwrap_or("").to_owned(),
                Ok(i) => self.at(i).unwrap_or("").to_owned(),
            })
        });
        let re = Regex::new(r"\$\$").unwrap();
        re.replace_all(&text, NoExpand("$"))
    }

    /// Returns the number of captured groups.
    #[inline]
    pub fn len(&self) -> usize {
        self.slots.len() / 2
    }

    /// Returns true if and only if there are no captured groups.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Get a group by index.
///
/// `'t` is the lifetime of the matched text.
///
/// The text can't outlive the `Captures` object if this method is
/// used, because of how `Index` is defined (normally `a[i]` is part
/// of `a` and can't outlive it); to do that, use `at()` instead.
///
/// # Panics
///
/// If there is no group at the given index.
impl<'t> Index<usize> for Captures<'t> {
    type Output = str;

    fn index(&self, i: usize) -> &str {
        self.at(i).unwrap_or_else(|| panic!("no group at index '{}'", i))
    }
}

/// Get a group by name.
///
/// `'t` is the lifetime of the matched text and `'i` is the lifetime
/// of the group name (the index).
///
/// The text can't outlive the `Captures` object if this method is
/// used, because of how `Index` is defined (normally `a[i]` is part
/// of `a` and can't outlive it); to do that, use `name` instead.
///
/// # Panics
///
/// If there is no group named by the given value.
impl<'t, 'i> Index<&'i str> for Captures<'t> {
    type Output = str;

    fn index<'a>(&'a self, name: &'i str) -> &'a str {
        self.name(name).unwrap_or_else(|| panic!("no group named '{}'", name))
    }
}

/// An iterator over capture groups for a particular match of a regular
/// expression.
///
/// `'c` is the lifetime of the captures.
pub struct SubCaptures<'c> {
    idx: usize,
    caps: &'c Captures<'c>,
}

impl<'c> Iterator for SubCaptures<'c> {
    type Item = Option<&'c str>;

    fn next(&mut self) -> Option<Option<&'c str>> {
        if self.idx < self.caps.len() {
            self.idx += 1;
            Some(self.caps.at(self.idx - 1))
        } else {
            None
        }
    }
}

/// An iterator over capture group positions for a particular match of a
/// regular expression.
///
/// Positions are byte indices in terms of the original string matched.
///
/// `'c` is the lifetime of the captures.
pub struct SubCapturesPos<'c> {
    idx: usize,
    slots: &'c [Option<usize>]
}

impl<'c> Iterator for SubCapturesPos<'c> {
    type Item = Option<(usize, usize)>;

    fn next(&mut self) -> Option<Option<(usize, usize)>> {
        if self.idx >= self.slots.len() {
            return None
        }
        let r = match (self.slots[self.idx], self.slots[self.idx + 1]) {
            (Some(s), Some(e)) => Some((s, e)),
            (None, None) => None,
            _ => unreachable!()
        };
        self.idx += 2;
        Some(r)
    }
}

/// An Iterator over named capture groups as a tuple with the group
/// name and the value.
///
/// `'c` is the lifetime of the captures.
pub struct SubCapturesNamed<'c> {
    caps: &'c Captures<'c>,
    names: NamedGroupsIter<'c>,
}

impl<'c> Iterator for SubCapturesNamed<'c> {
    type Item = (&'c str, Option<&'c str>);

    fn next(&mut self) -> Option<(&'c str, Option<&'c str>)> {
        self.names.next().map(|(name, pos)| (name, self.caps.at(pos)))
    }
}

/// An iterator that yields all non-overlapping capture groups matching a
/// particular regular expression.
///
/// The iterator stops when no more matches can be found.
///
/// `'r` is the lifetime of the compiled regular expression and `'t` is the
/// lifetime of the matched string.
pub struct FindCaptures<'r, 't>(FindCapturesInner<'r, 't>);

enum FindCapturesInner<'r, 't> {
    Dynamic(re_trait::FindCaptures<'t, ExecNoSyncStr<'r>>),
    Plugin(re_trait::FindCaptures<'t, Plugin>),
}

impl<'r, 't> Iterator for FindCaptures<'r, 't> {
    type Item = Captures<'t>;

    fn next(&mut self) -> Option<Captures<'t>> {
        match self.0 {
            FindCapturesInner::Dynamic(ref mut it) => {
                let named = it.regex().capture_name_idx().clone();
                it.next().map(|slots| Captures {
                    text: it.text(),
                    slots: slots,
                    named_groups: NamedGroups::Dynamic(named),
                })
            }
            FindCapturesInner::Plugin(ref mut it) => {
                it.next().map(|slots| Captures {
                    text: it.text(),
                    slots: slots,
                    named_groups: NamedGroups::Plugin(it.regex().groups),
                })
            }
        }
    }
}

/// An iterator over all non-overlapping matches for a particular string.
///
/// The iterator yields a tuple of integers corresponding to the start and end
/// of the match. The indices are byte offsets. The iterator stops when no more
/// matches can be found.
///
/// `'r` is the lifetime of the compiled regular expression and `'t` is the
/// lifetime of the matched string.
pub struct FindMatches<'r, 't>(FindMatchesInner<'r, 't>);

enum FindMatchesInner<'r, 't> {
    Dynamic(re_trait::FindMatches<'t, ExecNoSyncStr<'r>>),
    Plugin(re_trait::FindMatches<'t, Plugin>),
}

impl<'r, 't> FindMatches<'r, 't> {
    fn text(&self) -> &'t str {
        match self.0 {
            FindMatchesInner::Dynamic(ref it) => it.text(),
            FindMatchesInner::Plugin(ref it) => it.text(),
        }
    }
}

impl<'r, 't> Iterator for FindMatches<'r, 't> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        match self.0 {
            FindMatchesInner::Dynamic(ref mut it) => it.next(),
            FindMatchesInner::Plugin(ref mut it) => it.next(),
        }
    }
}

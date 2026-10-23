//! Lua-style string patterns shared by the interpreter and the C backend.
//!
//! The engine implements the classic Lua pattern language (character classes,
//! sets, anchors, captures, back references, balanced matches, frontier
//! matches, and the four quantifiers) over Unicode scalar values.  Matching is
//! depth-bounded and step-bounded so a hostile pattern cannot consume
//! exponential time or overflow the host stack.

use std::fmt;

/// Runtime error code for malformed patterns and replacements.
pub const E_PATTERN: &str = "E2010";
/// Runtime error code for a pattern that exceeded the matcher budget.
pub const E_PATTERN_COMPLEX: &str = "E2011";

const MAX_CAPTURES: usize = 32;
const MAX_DEPTH: usize = 200;
const MAX_STEPS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKind {
    /// A substring capture such as `(abc)`.
    Substring,
    /// A position capture written as `()`.
    Position,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capture {
    /// Zero-based start index in the subject, in Unicode scalar values.
    pub start: usize,
    /// Zero-based exclusive end index.  For position captures this equals start.
    pub end: usize,
    pub kind: CaptureKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Zero-based start index of the whole match.
    pub start: usize,
    /// Zero-based exclusive end index of the whole match.
    pub end: usize,
    pub captures: Vec<Capture>,
}

impl Match {
    /// Returns the full matched text for `source`.
    pub fn text<'a>(&self, source: &'a [char]) -> String {
        source[self.start..self.end].iter().collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    /// Stable runtime error code (`E2010` or `E2011`).
    pub code: &'static str,
    pub message: String,
}

impl PatternError {
    fn malformed(message: impl Into<String>) -> Self {
        Self {
            code: E_PATTERN,
            message: message.into(),
        }
    }

    fn complex() -> Self {
        Self {
            code: E_PATTERN_COMPLEX,
            message: "pattern exceeded the bounded matching budget; simplify nested quantifiers"
                .to_string(),
        }
    }
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PatternError {}

#[derive(Clone, Copy, Debug)]
enum CaptureState {
    Open(usize),
    Closed(Capture),
}

/// A validated pattern.  Compiling once lets `gmatch` avoid re-parsing the
/// same pattern for every iteration.
#[derive(Clone, Debug)]
pub struct Pattern {
    chars: Vec<char>,
    capture_count: usize,
    anchored: bool,
}

impl Pattern {
    /// Validates `pattern` and prepares it for matching.
    pub fn new(pattern: &str) -> Result<Self, PatternError> {
        let chars: Vec<char> = pattern.chars().collect();
        let capture_count = validate_chars(&chars)?;
        Ok(Self {
            anchored: chars.first() == Some(&'^'),
            chars,
            capture_count,
        })
    }

    pub fn capture_count(&self) -> usize {
        self.capture_count
    }

    pub fn is_anchored(&self) -> bool {
        self.anchored
    }

    /// Searches from a zero-based subject index.  A leading `^` anchors the
    /// match at `start`; `honor_anchor == false` is used by `gmatch`, where a
    /// leading caret is deliberately ignored.
    pub fn find_from(
        &self,
        source: &[char],
        start: usize,
        honor_anchor: bool,
    ) -> Result<Option<Match>, PatternError> {
        if start > source.len() {
            return Ok(None);
        }
        let mut pstart = 0usize;
        let only_start = if self.anchored {
            pstart = 1;
            honor_anchor
        } else {
            false
        };
        let end_start = if only_start { start } else { source.len() };
        let mut s = start;
        loop {
            let mut matcher = Matcher::new(source, &self.chars);
            if let Some(end) = matcher.do_match(s, pstart, 0)? {
                let captures = matcher.closed_captures();
                return Ok(Some(Match {
                    start: s,
                    end,
                    captures,
                }));
            }
            if s >= end_start {
                return Ok(None);
            }
            s += 1;
        }
    }
}

/// The stateful iterator returned by `string.gmatch`.
#[derive(Clone, Debug)]
pub struct Gmatch {
    pattern: Pattern,
    source: Vec<char>,
    pos: usize,
    done: bool,
}

impl Gmatch {
    pub fn new(source: &str, pattern: &str) -> Result<Self, PatternError> {
        Ok(Self {
            pattern: Pattern::new(pattern)?,
            source: source.chars().collect(),
            pos: 0,
            done: false,
        })
    }

    pub fn next_match(&mut self) -> Result<Option<Match>, PatternError> {
        if self.done || self.pos > self.source.len() {
            return Ok(None);
        }
        let found = self
            .pattern
            .find_from(&self.source, self.pos, false)?;
        let Some(found) = found else {
            self.done = true;
            return Ok(None);
        };
        if found.end > found.start {
            self.pos = found.end;
        } else if found.end < self.source.len() {
            self.pos = found.end + 1;
        } else {
            self.done = true;
        }
        Ok(Some(found))
    }

    pub fn source_chars(&self) -> &[char] {
        &self.source
    }
}

/// Validates a pattern and returns its explicit capture count.
pub fn validate(pattern: &str) -> Result<usize, PatternError> {
    let chars: Vec<char> = pattern.chars().collect();
    validate_chars(&chars)
}

/// Literal (plain) substring search.  No pattern syntax is interpreted.
pub fn find_plain(
    source: &[char],
    pattern: &[char],
    start: usize,
    first_only: bool,
) -> Option<Match> {
    if start > source.len() {
        return None;
    }
    if pattern.is_empty() {
        return Some(Match {
            start,
            end: start,
            captures: Vec::new(),
        });
    }
    if pattern.len() > source.len().saturating_sub(start) {
        return None;
    }
    let last = source.len() - pattern.len();
    let mut i = start;
    loop {
        if source[i..i + pattern.len()] == *pattern {
            return Some(Match {
                start: i,
                end: i + pattern.len(),
                captures: Vec::new(),
            });
        }
        if i >= last || first_only {
            return None;
        }
        i += 1;
    }
}

/// Convenience wrapper used by the dynamic string library.
pub fn find(
    source: &str,
    pattern: &str,
    start: usize,
    plain: bool,
) -> Result<Option<Match>, PatternError> {
    let chars: Vec<char> = source.chars().collect();
    if plain {
        let pat: Vec<char> = pattern.chars().collect();
        return Ok(find_plain(&chars, &pat, start, false));
    }
    let compiled = Pattern::new(pattern)?;
    compiled.find_from(&chars, start, true)
}

/// Returns `(replacement_result, substitution_count)`.
pub fn gsub(
    source: &str,
    pattern: &str,
    replacement: &str,
    limit: Option<usize>,
) -> Result<(String, usize), PatternError> {
    let compiled = Pattern::new(pattern)?;
    validate_replacement(replacement, compiled.capture_count)?;
    let src: Vec<char> = source.chars().collect();
    let max = limit.unwrap_or(usize::MAX);
    let mut out = String::new();
    let mut s = 0usize;
    let mut count = 0usize;

    if compiled.is_anchored() {
        if max == 0 {
            return Ok((source.to_string(), 0));
        }
        if let Some(m) = compiled.find_from(&src, 0, true)? {
            out.extend(src[..m.start].iter());
            append_replacement(&mut out, &src, &m, replacement, compiled.capture_count)?;
            out.extend(src[m.end..].iter());
            return Ok((out, 1));
        }
        return Ok((source.to_string(), 0));
    }

    loop {
        if count >= max {
            out.extend(src[s..].iter());
            break;
        }
        let found = compiled.find_from(&src, s, true)?;
        let Some(m) = found else {
            out.extend(src[s..].iter());
            break;
        };
        if m.start > s {
            out.extend(src[s..m.start].iter());
        }
        append_replacement(&mut out, &src, &m, replacement, compiled.capture_count)?;
        count += 1;
        if m.end > m.start {
            s = m.end;
        } else if m.end < src.len() {
            out.push(src[m.end]);
            s = m.end + 1;
        } else {
            break;
        }
    }
    Ok((out, count))
}

/// Appends the expanded replacement for one match.
fn append_replacement(
    out: &mut String,
    source: &[char],
    m: &Match,
    replacement: &str,
    capture_count: usize,
) -> Result<(), PatternError> {
    let chars: Vec<char> = replacement.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch != '%' {
            out.push(ch);
            i += 1;
            continue;
        }
        i += 1;
        if i >= chars.len() {
            return Err(PatternError::malformed(
                "invalid use of '%' in replacement string",
            ));
        }
        let esc = chars[i];
        i += 1;
        if esc == '%' {
            out.push('%');
            continue;
        }
        if let Some(digit) = esc.to_digit(10) {
            let index = digit as usize;
            if index > capture_count {
                return Err(PatternError::malformed(format!(
                    "replacement references capture %{} but the pattern has {} capture(s)",
                    index, capture_count
                )));
            }
            if index == 0 {
                out.extend(source[m.start..m.end].iter());
            } else {
                let cap = m.captures.get(index - 1).ok_or_else(|| {
                    PatternError::malformed(format!(
                        "replacement references capture %{} but the pattern has {} capture(s)",
                        index, capture_count
                    ))
                })?;
                match cap.kind {
                    CaptureKind::Substring => {
                        out.extend(source[cap.start..cap.end].iter());
                    }
                    CaptureKind::Position => {
                        out.push_str(&(cap.start + 1).to_string());
                    }
                }
            }
            continue;
        }
        return Err(PatternError::malformed(format!(
            "invalid replacement escape `%{}`; use `%0`..`%9` or `%%`",
            esc
        )));
    }
    Ok(())
}

/// Checks replacement syntax and capture references without matching.
pub fn validate_replacement(
    replacement: &str,
    capture_count: usize,
) -> Result<(), PatternError> {
    let chars: Vec<char> = replacement.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '%' {
            i += 1;
            continue;
        }
        i += 1;
        if i >= chars.len() {
            return Err(PatternError::malformed(
                "invalid use of '%' in replacement string",
            ));
        }
        let esc = chars[i];
        i += 1;
        if esc == '%' {
            continue;
        }
        if let Some(digit) = esc.to_digit(10) {
            if digit as usize > capture_count {
                return Err(PatternError::malformed(format!(
                    "replacement references capture %{} but the pattern has {} capture(s)",
                    digit, capture_count
                )));
            }
            continue;
        }
        return Err(PatternError::malformed(format!(
            "invalid replacement escape `%{}`; use `%0`..`%9` or `%%`",
            esc
        )));
    }
    Ok(())
}

// ===== validation =====

fn validate_chars(pat: &[char]) -> Result<usize, PatternError> {
    let mut capture_count = 0usize;
    let mut open = Vec::new();
    let mut i = 0usize;
    while i < pat.len() {
        match pat[i] {
            '%' => {
                if i + 1 >= pat.len() {
                    return Err(PatternError::malformed(
                        "malformed pattern: '%' at end of pattern",
                    ));
                }
                match pat[i + 1] {
                    'b' => {
                        if i + 3 >= pat.len() {
                            return Err(PatternError::malformed(
                                "malformed pattern: '%b' needs two characters",
                            ));
                        }
                        i += 4;
                    }
                    'f' => {
                        if i + 2 >= pat.len() || pat[i + 2] != '[' {
                            return Err(PatternError::malformed(
                                "malformed pattern: '%f' must be followed by a set",
                            ));
                        }
                        i = scan_set(pat, i + 2)?;
                    }
                    digit if digit.is_ascii_digit() => {
                        let index = digit.to_digit(10).unwrap_or(0) as usize;
                        if index == 0 {
                            return Err(PatternError::malformed(
                                "malformed pattern: '%0' is not a valid back reference",
                            ));
                        }
                        if index > capture_count {
                            return Err(PatternError::malformed(format!(
                                "malformed pattern: back reference %{} has no earlier capture",
                                index
                            )));
                        }
                        i += 2;
                    }
                    _ => i += 2,
                }
            }
            '[' => {
                i = scan_set(pat, i)?;
            }
            '(' => {
                if capture_count >= MAX_CAPTURES {
                    return Err(PatternError::malformed(
                        "malformed pattern: too many captures (maximum is 32)",
                    ));
                }
                capture_count += 1;
                if i + 1 < pat.len() && pat[i + 1] == ')' {
                    i += 2;
                } else {
                    open.push(i);
                    i += 1;
                }
            }
            ')' => {
                if open.pop().is_none() {
                    return Err(PatternError::malformed(
                        "malformed pattern: unmatched ')'",
                    ));
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    if let Some(_) = open.last() {
        return Err(PatternError::malformed(
            "malformed pattern: unfinished capture",
        ));
    }
    Ok(capture_count)
}

/// Returns the index just past a `[...]` set.
fn scan_set(pat: &[char], start: usize) -> Result<usize, PatternError> {
    debug_assert_eq!(pat.get(start), Some(&'['));
    let mut i = start + 1;
    if i < pat.len() && pat[i] == '^' {
        i += 1;
    }
    // A ']' immediately after '[' or '[^' is a literal member.
    if i < pat.len() && pat[i] == ']' {
        i += 1;
    }
    while i < pat.len() {
        match pat[i] {
            ']' => return Ok(i + 1),
            '%' => {
                if i + 1 >= pat.len() {
                    return Err(PatternError::malformed(
                        "malformed pattern: '%' at end of character set",
                    ));
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    Err(PatternError::malformed(
        "malformed pattern: missing ']' in character set",
    ))
}

// ===== matcher =====

struct Matcher<'a> {
    source: &'a [char],
    pattern: &'a [char],
    captures: [Option<CaptureState>; MAX_CAPTURES],
    level: usize,
    steps: usize,
}

impl<'a> Matcher<'a> {
    fn new(source: &'a [char], pattern: &'a [char]) -> Self {
        Self {
            source,
            pattern,
            captures: [None; MAX_CAPTURES],
            level: 0,
            steps: 0,
        }
    }

    fn closed_captures(&self) -> Vec<Capture> {
        (0..self.level)
            .filter_map(|index| match self.captures[index] {
                Some(CaptureState::Closed(cap)) => Some(cap),
                _ => None,
            })
            .collect()
    }

    fn tick(&mut self) -> Result<(), PatternError> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err(PatternError::complex());
        }
        Ok(())
    }

    fn do_match(
        &mut self,
        s: usize,
        p: usize,
        depth: usize,
    ) -> Result<Option<usize>, PatternError> {
        let saved_captures = self.captures;
        let saved_level = self.level;
        let result = self.do_match_inner(s, p, depth);
        if matches!(result, Ok(None)) {
            // Failed branches must not leak captures into sibling branches.
            self.captures = saved_captures;
            self.level = saved_level;
        }
        result
    }

    fn do_match_inner(
        &mut self,
        mut s: usize,
        mut p: usize,
        depth: usize,
    ) -> Result<Option<usize>, PatternError> {
        if depth > MAX_DEPTH {
            return Err(PatternError::complex());
        }
        self.tick()?;
        loop {
            if p >= self.pattern.len() {
                return Ok(Some(s));
            }
            match self.pattern[p] {
                '(' => {
                    if p + 1 < self.pattern.len() && self.pattern[p + 1] == ')' {
                        self.start_capture(s, CaptureKind::Position)?;
                        p += 2;
                    } else {
                        self.start_capture(s, CaptureKind::Substring)?;
                        p += 1;
                    }
                }
                ')' => {
                    self.end_capture(s)?;
                    p += 1;
                }
                '$' if p + 1 == self.pattern.len() => {
                    return Ok(if s == self.source.len() { Some(s) } else { None });
                }
                '%' => {
                    let Some(&esc) = self.pattern.get(p + 1) else {
                        return Err(PatternError::malformed(
                            "malformed pattern: '%' at end of pattern",
                        ));
                    };
                    match esc {
                        'b' => {
                            let Some(next_s) = self.match_balance(s, p)? else {
                                return Ok(None);
                            };
                            s = next_s;
                            p += 4;
                        }
                        'f' => {
                            let Some(next_s) = self.match_frontier(s, p)? else {
                                return Ok(None);
                            };
                            s = next_s;
                            p = self.class_end(p + 2)?;
                        }
                        digit if digit.is_ascii_digit() => {
                            let Some(next_s) = self.match_capture(s, digit)? else {
                                return Ok(None);
                            };
                            s = next_s;
                            p += 2;
                        }
                        _ => {
                            let ep = self.class_end(p)?;
                            if let Some(next) = self.quantifier(s, p, ep, depth)? {
                                return Ok(next);
                            }
                            if !self.single_match(s, p, ep) {
                                return Ok(None);
                            }
                            s += 1;
                            p = ep;
                        }
                    }
                }
                _ => {
                    let ep = self.class_end(p)?;
                    if let Some(next) = self.quantifier(s, p, ep, depth)? {
                        return Ok(next);
                    }
                    if !self.single_match(s, p, ep) {
                        return Ok(None);
                    }
                    s += 1;
                    p = ep;
                }
            }
        }
    }

    fn quantifier(
        &mut self,
        s: usize,
        p: usize,
        ep: usize,
        depth: usize,
    ) -> Result<Option<Option<usize>>, PatternError> {
        if ep >= self.pattern.len() {
            return Ok(None);
        }
        match self.pattern[ep] {
            '?' => {
                let mut next = s;
                if self.single_match(s, p, ep) {
                    next += 1;
                }
                Ok(Some(self.do_match(next, ep + 1, depth + 1)?))
            }
            '*' => Ok(Some(self.max_expand(s, p, ep, depth)?)),
            '+' => {
                if !self.single_match(s, p, ep) {
                    return Ok(Some(None));
                }
                Ok(Some(self.max_expand(s + 1, p, ep, depth)?))
            }
            '-' => Ok(Some(self.min_expand(s, p, ep, depth)?)),
            _ => Ok(None),
        }
    }

    fn max_expand(
        &mut self,
        s: usize,
        p: usize,
        ep: usize,
        depth: usize,
    ) -> Result<Option<usize>, PatternError> {
        let mut count = 0usize;
        while self.single_match(s + count, p, ep) {
            count += 1;
        }
        let mut i = count;
        loop {
            if let Some(end) = self.do_match(s + i, ep + 1, depth + 1)? {
                return Ok(Some(end));
            }
            if i == 0 {
                return Ok(None);
            }
            i -= 1;
        }
    }

    fn min_expand(
        &mut self,
        mut s: usize,
        p: usize,
        ep: usize,
        depth: usize,
    ) -> Result<Option<usize>, PatternError> {
        loop {
            if let Some(end) = self.do_match(s, ep + 1, depth + 1)? {
                return Ok(Some(end));
            }
            if !self.single_match(s, p, ep) {
                return Ok(None);
            }
            s += 1;
        }
    }

    fn start_capture(&mut self, start: usize, kind: CaptureKind) -> Result<(), PatternError> {
        if self.level >= MAX_CAPTURES {
            return Err(PatternError::malformed(
                "malformed pattern: too many captures (maximum is 32)",
            ));
        }
        self.captures[self.level] = Some(CaptureState::Open(start));
        self.level += 1;
        if kind == CaptureKind::Position {
            // Position captures are closed immediately; the open marker above
            // is overwritten with a zero-width closed capture.
            self.captures[self.level - 1] = Some(CaptureState::Closed(Capture {
                start,
                end: start,
                kind,
            }));
        }
        Ok(())
    }

    fn end_capture(&mut self, end: usize) -> Result<(), PatternError> {
        let mut index = self.level;
        while index > 0 {
            index -= 1;
            if let Some(CaptureState::Open(start)) = self.captures[index] {
                self.captures[index] = Some(CaptureState::Closed(Capture {
                    start,
                    end,
                    kind: CaptureKind::Substring,
                }));
                return Ok(());
            }
        }
        Err(PatternError::malformed(
            "malformed pattern: unmatched ')'",
        ))
    }

    fn match_capture(&self, s: usize, digit: char) -> Result<Option<usize>, PatternError> {
        let index = digit.to_digit(10).unwrap_or(0) as usize;
        if index == 0 || index > self.level {
            return Err(PatternError::malformed(format!(
                "malformed pattern: invalid capture index %{}",
                digit
            )));
        }
        let Some(CaptureState::Closed(cap)) = self.captures[index - 1] else {
            return Err(PatternError::malformed(format!(
                "malformed pattern: capture %{} is not closed",
                digit
            )));
        };
        let len = cap.end.saturating_sub(cap.start);
        if s + len > self.source.len() {
            return Ok(None);
        }
        if self.source[cap.start..cap.end] == self.source[s..s + len] {
            Ok(Some(s + len))
        } else {
            Ok(None)
        }
    }

    fn match_balance(&self, s: usize, p: usize) -> Result<Option<usize>, PatternError> {
        if p + 3 >= self.pattern.len() {
            return Err(PatternError::malformed(
                "malformed pattern: '%b' needs two characters",
            ));
        }
        let open = self.pattern[p + 2];
        let close = self.pattern[p + 3];
        if s >= self.source.len() || self.source[s] != open {
            return Ok(None);
        }
        let mut depth = 1usize;
        let mut i = s + 1;
        while i < self.source.len() {
            if self.source[i] == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(i + 1));
                }
            } else if self.source[i] == open {
                depth += 1;
            }
            i += 1;
        }
        Ok(None)
    }

    fn match_frontier(&self, s: usize, p: usize) -> Result<Option<usize>, PatternError> {
        if p + 2 >= self.pattern.len() || self.pattern[p + 2] != '[' {
            return Err(PatternError::malformed(
                "malformed pattern: '%f' must be followed by a set",
            ));
        }
        let ep = self.class_end(p + 2)?;
        let previous = if s == 0 { '\0' } else { self.source[s - 1] };
        let current = if s >= self.source.len() {
            '\0'
        } else {
            self.source[s]
        };
        if self.bracket_match(previous, p + 2, ep)
            || !self.bracket_match(current, p + 2, ep)
        {
            return Ok(None);
        }
        Ok(Some(s))
    }

    fn class_end(&self, p: usize) -> Result<usize, PatternError> {
        if p >= self.pattern.len() {
            return Err(PatternError::malformed(
                "malformed pattern: unexpected end of pattern",
            ));
        }
        match self.pattern[p] {
            '%' => {
                if p + 1 >= self.pattern.len() {
                    return Err(PatternError::malformed(
                        "malformed pattern: '%' at end of pattern",
                    ));
                }
                match self.pattern[p + 1] {
                    'b' => {
                        if p + 3 >= self.pattern.len() {
                            return Err(PatternError::malformed(
                                "malformed pattern: '%b' needs two characters",
                            ));
                        }
                        Ok(p + 4)
                    }
                    'f' => {
                        if p + 2 >= self.pattern.len() || self.pattern[p + 2] != '[' {
                            return Err(PatternError::malformed(
                                "malformed pattern: '%f' must be followed by a set",
                            ));
                        }
                        scan_set(self.pattern, p + 2)
                    }
                    _ => Ok(p + 2),
                }
            }
            '[' => scan_set(self.pattern, p),
            _ => Ok(p + 1),
        }
    }

    fn single_match(&self, s: usize, p: usize, ep: usize) -> bool {
        let Some(&ch) = self.source.get(s) else {
            return false;
        };
        if ep > p && self.pattern[p] == '[' {
            return self.bracket_match(ch, p, ep);
        }
        if self.pattern[p] == '.' {
            return true;
        }
        if self.pattern[p] == '%' && p + 1 < self.pattern.len() {
            return match_class(ch, self.pattern[p + 1]);
        }
        self.pattern[p] == ch
    }

    fn bracket_match(&self, ch: char, p: usize, ep: usize) -> bool {
        let mut i = p + 1;
        let mut negate = false;
        if i < ep && self.pattern[i] == '^' {
            negate = true;
            i += 1;
        }
        let mut matched = false;
        if i < ep && self.pattern[i] == ']' {
            if ch == ']' {
                matched = true;
            }
            i += 1;
        }
        while i < ep {
            if self.pattern[i] == '%' && i + 1 < ep {
                if match_class(ch, self.pattern[i + 1]) {
                    matched = true;
                }
                i += 2;
            } else if i + 2 < ep
                && self.pattern[i + 1] == '-'
                && self.pattern[i + 2] != ']'
            {
                if self.pattern[i] <= ch && ch <= self.pattern[i + 2] {
                    matched = true;
                }
                i += 3;
            } else {
                if self.pattern[i] == ch {
                    matched = true;
                }
                i += 1;
            }
        }
        if negate {
            !matched
        } else {
            matched
        }
    }
}

fn match_class(ch: char, class: char) -> bool {
    let is_upper = class.is_ascii_uppercase();
    let class = class.to_ascii_lowercase();
    let matched = match class {
        'a' => ch.is_ascii_alphabetic(),
        'c' => ch.is_ascii_control(),
        'd' => ch.is_ascii_digit(),
        'g' => ch.is_ascii_graphic(),
        'l' => ch.is_ascii_lowercase(),
        'p' => ch.is_ascii_punctuation(),
        's' => ch.is_ascii_whitespace(),
        'u' => ch.is_ascii_uppercase(),
        'w' => ch.is_ascii_alphanumeric(),
        'x' => ch.is_ascii_hexdigit(),
        'z' => ch == '\0',
        _ => ch == class,
    };
    if is_upper {
        !matched
    } else {
        matched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(value: &str) -> Vec<char> {
        value.chars().collect()
    }

    fn found(source: &str, pattern: &str) -> Option<(usize, usize)> {
        find(source, pattern, 0, false)
            .unwrap()
            .map(|m| (m.start, m.end))
    }

    fn captured(source: &str, pattern: &str) -> Vec<String> {
        let chars = chars(source);
        let match_ = find(source, pattern, 0, false).unwrap().unwrap();
        match_
            .captures
            .iter()
            .map(|cap| match cap.kind {
                CaptureKind::Substring => chars[cap.start..cap.end].iter().collect(),
                CaptureKind::Position => (cap.start + 1).to_string(),
            })
            .collect()
    }

    #[test]
    fn classes_and_quantifiers() {
        assert_eq!(found("abc123", "%d+"), Some((3, 6)));
        assert_eq!(found("abc123", "^%a+"), Some((0, 3)));
        assert_eq!(found("abc123", "%d+$"), Some((3, 6)));
        assert_eq!(found("aaab", "a-b"), Some((0, 4)));
        assert_eq!(found("aaab", "a*b"), Some((0, 4)));
        assert_eq!(found("ac", "ab?c"), Some((0, 2)));
        assert_eq!(found("abc", "[^%d]+"), Some((0, 3)));
    }

    #[test]
    fn captures_and_positions() {
        assert_eq!(captured("key=value", "(%a+)=(%a+)"), vec!["key", "value"]);
        assert_eq!(captured("abc", "()b()"), vec!["2", "3"]);
        assert_eq!(found("abcabc", "(abc)%1"), Some((0, 6)));
        let balanced = find("(a(b)c)", "%b()", 0, false).unwrap().unwrap();
        assert_eq!(balanced.start, 0);
        assert_eq!(balanced.end, 7);
    }

    #[test]
    fn gsub_expands_captures_and_empty_matches() {
        let (text, count) = gsub("a=1 b=2", "(%a+)=(%d+)", "%2:%1", None).unwrap();
        assert_eq!(text, "1:a 2:b");
        assert_eq!(count, 2);
        let (text, count) = gsub("abc", "", "-", None).unwrap();
        assert_eq!(text, "-a-b-c-");
        assert_eq!(count, 4);
        let (text, count) = gsub("abc", ".", "%0%0", Some(2)).unwrap();
        assert_eq!(text, "aabbc");
        assert_eq!(count, 2);
    }

    #[test]
    fn failed_backtracking_restores_captures() {
        // The sibling branch after `(a*)` closes `(b)` before `c` fails.  A
        // retry must start from the capture state saved at the branch entry.
        assert_eq!(found("aaab", "(a*)(b)c"), None);
        assert_eq!(captured("aaabc", "(a*)(b)c"), vec!["aaa", "b"]);
    }

    #[test]
    fn malformed_patterns_are_reported() {
        assert!(Pattern::new("[abc").is_err());
        assert!(Pattern::new("%").is_err());
        assert!(Pattern::new("(abc").is_err());
        assert!(Pattern::new("abc)").is_err());
        assert!(Pattern::new("%b").is_err());
        assert!(gsub("x", "(x)", "%2", None).is_err());
    }
}

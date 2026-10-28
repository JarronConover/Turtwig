/*! Markdown styled in the message box as it's typed (`chat.live_markdown`); the text stays as typed.
Backspace right after what completed a piece of formatting turns it back into plain text (`literal`). */

use gpui_kit::*;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Marker,
    Quote,
    Heading,
    List,
    Bold,
    Italic,
    Strike,
    Code,
    CodeBlock,
    Link,
    Url,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Marker => "marker",
            Kind::Quote => "quote",
            Kind::Heading => "heading",
            Kind::List => "list",
            Kind::Bold => "bold",
            Kind::Italic => "italic",
            Kind::Strike => "strike",
            Kind::Code => "code",
            Kind::CodeBlock => "code_block",
            Kind::Link => "link",
            Kind::Url => "url",
        }
    }
}

/** One piece of formatting, named by the offset it starts at; `trigger_end` is where the text that
completed it ends, and `block` says it formats whole lines. */
#[derive(Debug, PartialEq)]
pub struct Construct {
    pub id: usize,
    pub trigger_end: usize,
    pub block: bool,
    pub pieces: Vec<(Range<usize>, Kind)>,
}

/** The formatting in `text`, less what `literal` turned back into plain text. */
pub fn parse(text: &str, literal: &HashSet<usize>) -> Vec<Construct> {
    let mut out: Vec<Construct> = vec![];
    let mut fence: Option<usize> = None;
    let mut start = 0;
    for line in text.split('\n') {
        let (ls, le) = (start, start + line.len());
        start = le + 1;
        let indent = line.len() - line.trim_start_matches(' ').len();
        let body = &line[indent..];
        if body.starts_with("```") && !literal.contains(&ls) {
            match fence.take() {
                Some(i) => out[i].pieces.push((ls..le, Kind::Marker)),
                None => {
                    fence = Some(out.len());
                    out.push(Construct { id: ls, trigger_end: ls + indent + 3, block: true, pieces: vec![(ls..le, Kind::Marker)] });
                }
            }
            continue;
        }
        if let Some(i) = fence {
            if le > ls {
                out[i].pieces.push((ls..le, Kind::CodeBlock));
            }
            continue;
        }
        let content = match block_marker(body) {
            Some((n, kind)) if !literal.contains(&ls) => {
                let end = ls + indent + n;
                let mut pieces = vec![(ls + indent..end, if kind == Kind::List { Kind::List } else { Kind::Marker })];
                if kind != Kind::List && end < le {
                    pieces.push((end..le, kind));
                }
                out.push(Construct { id: ls, trigger_end: end, block: true, pieces });
                end
            }
            _ => ls,
        };
        inline(text, content..le, literal, &mut out);
    }
    out
}

/** What a piece shows as once its markdown hides (`hide_markers`): "quote" (a bar), "bullet" (•) or "hide"
(nothing); None: it stays text. Code fences and numbered lists stay. */
pub fn token_tag(k: &Construct, piece: &(Range<usize>, Kind), text: &str) -> Option<&'static str> {
    let s = &text[piece.0.clone()];
    match piece.1 {
        Kind::Marker if k.block => match s.as_bytes().first() {
            Some(b'>') => Some("quote"),
            Some(b'#') => Some("hide"),
            _ => None,
        },
        Kind::Marker | Kind::Url => Some("hide"),
        Kind::List if matches!(s, "- " | "* " | "+ ") => Some("bullet"),
        _ => None,
    }
}

/** For a new line typed inside a quote or list: the marker's length (with its indent), and the marker the
next line starts with ("> ", "- ", "3. "). */
pub fn continuation(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let body = &line[indent..];
    let (n, kind) = block_marker(body)?;
    let next = match kind {
        Kind::Quote => "> ".to_string(),
        Kind::List => match body[..n].trim_end().strip_suffix(['.', ')']).and_then(|d| d.parse::<u64>().ok()) {
            Some(d) => format!("{}{} ", d + 1, &body[n - 2..n - 1]),
            None => body[..n].to_string(),
        },
        _ => return None,
    };
    Some((indent + n, format!("{}{next}", &line[..indent])))
}

/** Tab or shift-tab (`out`) in a list: its items in `range` move in under the item above at their level,
or out to their parent's, nested items with them, numbered ones renumbered for where they land. */
pub struct Reindent {
    pub range: Range<usize>,
    pub text: String,
    /** Each changed line: (start before, prefix length before, start after, prefix length after). */
    lines: Vec<(usize, usize, usize, usize)>,
}

impl Reindent {
    /** Where offset `p` of the old text is in the new. */
    pub fn map(&self, p: usize) -> usize {
        let Some(&(ls, old, ns, new)) = self.lines.iter().rev().find(|l| l.0 <= p) else { return p };
        let col = p - ls;
        ns + if col >= old { col - old + new } else { col.min(new) }
    }
}

/** None when `range` starts on a line that isn't a list item, so Tab does what it otherwise does. */
pub fn reindent(text: &str, range: Range<usize>, out: bool, literal: &HashSet<usize>) -> Option<Reindent> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let mut fenced = false;
    let list: Vec<bool> = lines
        .iter()
        .zip(&starts)
        .map(|(l, ls)| {
            if l.trim_start_matches(' ').starts_with("```") {
                fenced = !fenced;
                return false;
            }
            !fenced && !literal.contains(ls) && matches!(block_marker(l.trim_start_matches(' ')), Some((_, Kind::List)))
        })
        .collect();
    let line_of = |p: usize| starts.iter().rposition(|s| *s <= p).unwrap_or(0);
    let first = line_of(range.start);
    let last = if range.end > range.start && starts.contains(&range.end) { line_of(range.end - 1) } else { line_of(range.end) };
    if !list[first] {
        return None;
    }
    let indent = |l: &str| l.len() - l.trim_start_matches(' ').len();
    let before = lines.clone();
    let mut changed_to = first;
    let mut i = first;
    while i <= last {
        if !list[i] {
            i += 1;
            continue;
        }
        let ind = indent(&lines[i]);
        // The nearest item above whose indent passes `want`, within this run of list lines.
        let above = |lines: &[String], want: &dyn Fn(usize) -> bool| (0..i).rev().take_while(|j| list[*j]).find(|j| want(indent(&lines[*j])));
        let to = if out {
            above(&lines, &|d| d < ind).map_or(0, |j| indent(&lines[j]))
        } else {
            match above(&lines, &|d| d <= ind) {
                Some(j) if indent(&lines[j]) == ind => ind + block_marker(&lines[j][ind..]).map_or(2, |(n, _)| n),
                _ => ind,
            }
        };
        let mut end = i + 1;
        while end < lines.len() && list[end] && indent(&lines[end]) > ind {
            end += 1;
        }
        if to != ind {
            for line in &mut lines[i..end] {
                let d = indent(line);
                *line = format!("{}{}", " ".repeat((d + to).saturating_sub(ind)), &line[d..]);
            }
            let body = lines[i][to..].to_string();
            let digits = body.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 {
                let sibling = (0..i).rev().take_while(|j| list[*j] && indent(&lines[*j]) >= to).find(|j| indent(&lines[*j]) == to);
                let n = sibling.and_then(|j| lines[j][to..].split(['.', ')']).next()?.parse::<u64>().ok()).map_or(1, |n| n + 1);
                lines[i] = format!("{}{n}{}", " ".repeat(to), &body[digits..]);
            }
            changed_to = changed_to.max(end - 1);
        }
        i = end;
    }
    let prefix = |l: &str| indent(l) + block_marker(l.trim_start_matches(' ')).map_or(0, |(n, _)| n);
    let mut at = starts[first];
    let mut map = vec![];
    for j in first..=changed_to {
        map.push((starts[j], prefix(&before[j]), at, prefix(&lines[j])));
        at += lines[j].len() + 1;
    }
    Some(Reindent { range: starts[first]..starts[changed_to] + before[changed_to].len(), text: lines[first..=changed_to].join("\n"), lines: map })
}

/** A line's own marker ("> ", "## ", "- ", "1. "): its length and what the line becomes. */
fn block_marker(body: &str) -> Option<(usize, Kind)> {
    if body.starts_with("> ") {
        return Some((2, Kind::Quote));
    }
    let hashes = body.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && body[hashes..].starts_with(' ') {
        return Some((hashes + 1, Kind::Heading));
    }
    if ["- ", "* ", "+ "].iter().any(|m| body.starts_with(m)) {
        return Some((2, Kind::List));
    }
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && (body[digits..].starts_with(". ") || body[digits..].starts_with(") ")) {
        return Some((digits + 2, Kind::List));
    }
    None
}

/** Code spans, links, bold, strikethrough and italics within one line's `span`. */
fn inline(text: &str, span: Range<usize>, literal: &HashSet<usize>, out: &mut Vec<Construct>) {
    let s = &text.as_bytes()[span.clone()];
    let at = |i: usize| span.start + i;
    let push = |out: &mut Vec<Construct>, open: Range<usize>, inner: Kind, close: Range<usize>| {
        if !literal.contains(&at(open.start)) {
            let pieces = vec![(at(open.start)..at(open.end), Kind::Marker), (at(open.end)..at(close.start), inner), (at(close.start)..at(close.end), if inner == Kind::Link { Kind::Url } else { Kind::Marker })];
            out.push(Construct { id: at(open.start), trigger_end: at(close.end), block: false, pieces });
        }
    };
    let free = |taken: &[Range<usize>], r: Range<usize>| !taken.iter().any(|t| r.start < t.end && t.start < r.end);
    // Code spans first: nothing inside one is formatting.
    let mut taken: Vec<Range<usize>> = vec![];
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'`' {
            i += 1;
            continue;
        }
        let n = s[i..].iter().take_while(|b| **b == b'`').count();
        let close = (i + n + 1..s.len()).find(|&j| s[j..].starts_with(&s[i..i + n]) && s.get(j + n) != Some(&b'`') && s[j - 1] != b'`');
        match close {
            Some(j) => {
                push(out, i..i + n, Kind::Code, j..j + n);
                taken.push(i..j + n);
                i = j + n;
            }
            None => i += n,
        }
    }
    // [text](url)
    let mut i = 0;
    while let Some(o) = s[i..].iter().position(|b| *b == b'[').map(|o| i + o) {
        i = o + 1;
        let Some(c) = s[o + 1..].iter().position(|b| *b == b']').map(|c| o + 1 + c) else { break };
        if c == o + 1 || s.get(c + 1) != Some(&b'(') {
            continue;
        }
        let Some(e) = s[c + 2..].iter().position(|b| *b == b')' || *b == b' ').map(|e| c + 2 + e) else { continue };
        if s[e] != b')' || e == c + 2 || !free(&taken, o..e + 1) {
            continue;
        }
        push(out, o..o + 1, Kind::Link, c..e + 1);
        taken.push(o..e + 1);
        i = e + 1;
    }
    let spans = taken.clone();
    for (delim, kind) in [("**", Kind::Bold), ("__", Kind::Bold), ("~~", Kind::Strike), ("*", Kind::Italic), ("_", Kind::Italic)] {
        let d = delim.as_bytes();
        let n = d.len();
        // A lone `*` or `_`, not one of a pair; `_` only between words, so snake_case stays plain.
        let lone = |j: usize| n > 1 || (s.get(j + 1) != Some(&d[0]) && (j == 0 || s[j - 1] != d[0]));
        let edge = |j: usize, opening: bool| d[0] != b'_' || if opening { j == 0 || !s[j - 1].is_ascii_alphanumeric() } else { s.get(j + n).is_none_or(|b| !b.is_ascii_alphanumeric()) };
        let mark = |j: usize, opening: bool| s[j..].starts_with(d) && lone(j) && edge(j, opening) && free(&spans, j..j + n);
        let mut i = 0;
        while i + n <= s.len() {
            if !mark(i, true) {
                i += 1;
                continue;
            }
            let close = (i + n + 1..s.len()).find(|&j| mark(j, false)).filter(|&j| s[i + n] != b' ' && s[j - 1] != b' ');
            match close {
                Some(j) if free(&taken, i..i + n) && free(&taken, j..j + n) => {
                    push(out, i..i + n, kind, j..j + n);
                    taken.extend([i..i + n, j..j + n]);
                    i = j + n;
                }
                _ => i += n,
            }
        }
    }
}

/** A style from `chat.live_markdown.styles`: color, background, bold, italic, underline, strikethrough, fade. */
pub fn style(v: &Value, theme: &Map<String, Value>) -> HighlightStyle {
    let c = |k: &str| v.get(k).and_then(|x| crate::ui::style::color(x, theme));
    let on = |k: &str| v.get(k).and_then(Value::as_bool) == Some(true);
    let color = c("color");
    HighlightStyle {
        color,
        background_color: c("background"),
        font_weight: on("bold").then_some(FontWeight::BOLD),
        font_style: on("italic").then_some(FontStyle::Italic),
        underline: on("underline").then(|| UnderlineStyle { thickness: px(1.), color: c("underline_color").or(color), wavy: on("wavy") }),
        strikethrough: on("strikethrough").then(|| StrikethroughStyle { thickness: px(1.), color }),
        fade_out: v.get("fade").and_then(Value::as_f64).map(|f| f as f32),
    }
}

/** The text's formatting as styles to paint, flattened so none overlap: a line's style under its inline ones,
`over` (spelling marks) on top. */
pub fn decorations(text: &str, literal: &HashSet<usize>, styles: &Map<String, Value>, theme: &Map<String, Value>, over: Vec<(Range<usize>, HighlightStyle)>) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut pieces: Vec<(Range<usize>, HighlightStyle, bool)> = vec![];
    for k in parse(text, literal) {
        for (r, kind) in k.pieces {
            if let Some(v) = styles.get(kind.key()) {
                pieces.push((r, style(v, theme), k.block));
            }
        }
    }
    pieces.extend(over.into_iter().map(|(r, s)| (r, s, false)));
    let mut cuts: Vec<usize> = pieces.iter().flat_map(|(r, _, _)| [r.start, r.end]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<(Range<usize>, HighlightStyle)> = vec![];
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut over: Vec<&(Range<usize>, HighlightStyle, bool)> = pieces.iter().filter(|(r, _, _)| r.start <= a && b <= r.end).collect();
        if over.is_empty() {
            continue;
        }
        over.sort_by_key(|(_, _, block)| !block);
        let s = over.iter().skip(1).fold(over[0].1, |acc, (_, st, _)| acc.highlight(*st));
        match out.last_mut() {
            Some((r, last)) if r.end == a && *last == s => r.end = b,
            _ => out.push((a..b, s)),
        }
    }
    out
}

/** Markdown that shows as the message box drew it: each new line is a line break (not a space), and a line
without a quote's `>` or a list's marker, right after one, starts a paragraph of its own. */
pub fn as_typed(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut code = false;
    // The line before, when it's text (not blank, not in or opening a code block).
    let mut prev: Option<&str> = None;
    for (i, line) in text.split('\n').enumerate() {
        let body = line.trim_start_matches(' ');
        let fence = body.starts_with("```") || body.starts_with("~~~");
        let text_line = !code && !fence && !body.is_empty();
        if i > 0 {
            if let (Some(p), true) = (prev, text_line) {
                let pb = p.trim_start_matches(' ');
                let after_quote = pb.starts_with('>') && !body.starts_with('>');
                let after_item = matches!(block_marker(pb), Some((_, Kind::List))) && body.len() == line.len() && block_marker(body).is_none();
                if after_quote || after_item {
                    out.push('\n');
                } else if !p.ends_with("  ") {
                    out.push_str("  ");
                }
            }
            out.push('\n');
        }
        out.push_str(line);
        code ^= fence;
        prev = Some(line).filter(|_| text_line);
    }
    out
}

/** Moves `literal`'s offsets over an edit that replaced `start..old_end` with text ending at `new_end`;
formatting whose start was deleted goes. */
pub fn shift(literal: &mut HashSet<usize>, start: usize, old_end: usize, new_end: usize) {
    *literal = literal.iter().filter_map(|&o| if o < start { Some(o) } else if o >= old_end { Some(o - old_end + new_end) } else { None }).collect();
}

/** Where `old` and `new` differ: the start, the end in `old`, and the end in `new`. */
pub fn edit(old: &str, new: &str) -> (usize, usize, usize) {
    let mut p = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(p) || !new.is_char_boundary(p) {
        p -= 1;
    }
    let max = old.len().min(new.len()) - p;
    let mut q = old.bytes().rev().zip(new.bytes().rev()).take(max).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(old.len() - q) || !new.is_char_boundary(new.len() - q) {
        q -= 1;
    }
    (p, old.len() - q, new.len() - q)
}

#[cfg(test)]
mod tests {
    use super::{edit, parse, shift, Kind};
    use std::collections::HashSet;

    fn kinds(text: &str, literal: &[usize]) -> Vec<(String, Kind)> {
        let lit: HashSet<usize> = literal.iter().copied().collect();
        parse(text, &lit).into_iter().flat_map(|k| k.pieces).map(|(r, kind)| (text[r].to_string(), kind)).collect()
    }

    #[test]
    fn a_quote_formats_once_its_marker_is_typed() {
        assert_eq!(kinds(">", &[]), vec![]);
        assert_eq!(kinds("> hi", &[]), vec![("> ".into(), Kind::Marker), ("hi".into(), Kind::Quote)]);
        assert_eq!(kinds("> hi", &[0]), vec![]);
    }

    #[test]
    fn inline_formatting_needs_its_closing_mark() {
        assert_eq!(kinds("a **b", &[]), vec![]);
        assert_eq!(kinds("a **b**", &[]), vec![("**".into(), Kind::Marker), ("b".into(), Kind::Bold), ("**".into(), Kind::Marker)]);
        assert_eq!(kinds("*it* and `x*y*`", &[]).iter().filter(|(_, k)| *k == Kind::Italic).count(), 1);
        assert_eq!(kinds("snake_case_name", &[]), vec![]);
        assert_eq!(kinds("a [b](c) d", &[])[1], ("b".into(), Kind::Link));
    }

    #[test]
    fn a_code_block_holds_its_lines() {
        let k = kinds("```\nlet x = **1**;\n```", &[]);
        assert_eq!(k, vec![("```".into(), Kind::Marker), ("let x = **1**;".into(), Kind::CodeBlock), ("```".into(), Kind::Marker)]);
    }

    #[test]
    fn a_new_line_in_a_list_or_quote_continues_it() {
        assert_eq!(super::continuation("> a"), Some((2, "> ".into())));
        assert_eq!(super::continuation("  - a"), Some((4, "  - ".into())));
        assert_eq!(super::continuation("9. a"), Some((3, "10. ".into())));
        assert_eq!(super::continuation("# a"), None);
    }

    #[test]
    fn tab_nests_a_list_item_under_the_one_above() {
        let tab = |text: &str, at: usize, out: bool| {
            super::reindent(text, at..at, out, &HashSet::new()).map(|r| (format!("{}{}{}", &text[..r.range.start], r.text, &text[r.range.end..]), r.map(at)))
        };
        assert_eq!(tab("- a\n- b", 7, false), Some(("- a\n  - b".into(), 9)));
        assert_eq!(tab("- a\n- b", 3, false), Some(("- a\n- b".into(), 3)));
        assert_eq!(tab("- a\n  - b", 9, true), Some(("- a\n- b".into(), 7)));
        assert_eq!(tab("1. a\n2. b\n3. c", 9, false), Some(("1. a\n   1. b\n3. c".into(), 12)));
        assert_eq!(tab("1. a\n   1. b\n3. c", 12, true), Some(("1. a\n2. b\n3. c".into(), 9)));
        assert_eq!(tab("- a\n- b\n  - c\n- d", 6, false), Some(("- a\n  - b\n    - c\n- d".into(), 8)));
        assert_eq!(tab("text\n- a", 2, false), None);
        assert_eq!(tab("```\n- a\n```", 6, false), None);
        let r = super::reindent("- a\n- b\n- c", 4..11, false, &HashSet::new()).unwrap();
        assert_eq!(r.text, "  - b\n  - c");
    }

    #[test]
    fn sent_text_keeps_the_lines_and_quotes_as_typed() {
        use super::as_typed;
        assert_eq!(as_typed("a\nb\n\nc"), "a  \nb\n\nc");
        assert_eq!(as_typed("> a\n> b\nc"), "> a  \n> b\n\nc");
        assert_eq!(as_typed("- a\n- b\nc\n  d"), "- a  \n- b\n\nc  \n  d");
        assert_eq!(as_typed("x\n```\nl1\nl2\n```\ny\nz"), "x\n```\nl1\nl2\n```\ny  \nz");
    }

    #[test]
    fn plain_text_marks_follow_edits() {
        let mut lit: HashSet<usize> = [4].into_iter().collect();
        let (s, o, n) = edit("ab\n\n> x", "abc\n\n> x");
        assert_eq!((s, o, n), (2, 2, 3));
        shift(&mut lit, s, o, n);
        assert!(lit.contains(&5));
        let (s, o, n) = edit("abc\n\n> x", "abc\n\n x");
        shift(&mut lit, s, o, n);
        assert!(lit.is_empty());
    }
}

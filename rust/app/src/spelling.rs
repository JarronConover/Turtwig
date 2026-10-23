/*! Spell checking for the message box (`chat.spellcheck`), by macOS's own checker: the words it marks,
its guesses for one, and learning or ignoring a word. Offsets are byte offsets into the text. */

use objc2::rc::Retained;
use objc2_app_kit::NSSpellChecker;
use objc2_foundation::{NSRange, NSString};
use std::ops::Range;
use std::sync::OnceLock;

/** One spell document for the app's message boxes. */
fn tag() -> isize {
    static TAG: OnceLock<isize> = OnceLock::new();
    *TAG.get_or_init(NSSpellChecker::uniqueSpellDocumentTag)
}

fn utf16_to_byte(text: &str, u: usize) -> usize {
    let mut n = 0;
    for (i, ch) in text.char_indices() {
        if n >= u {
            return i;
        }
        n += ch.len_utf16();
    }
    text.len()
}

/** The language to check in: `language`, else the checker's own (the first in System Settings). Left to
guess, it takes a word any of the Mac's languages knows as spelled right. */
fn language(language: Option<&str>) -> Retained<NSString> {
    language.map(NSString::from_str).unwrap_or_else(|| NSSpellChecker::sharedSpellChecker().language())
}

/** The words macOS's checker marks in `text`. */
pub fn misspelled(text: &str, lang: Option<&str>) -> Vec<Range<usize>> {
    let checker = NSSpellChecker::sharedSpellChecker();
    let s = NSString::from_str(text);
    let lang = language(lang);
    let len = text.encode_utf16().count();
    let mut out = vec![];
    let mut at = 0;
    while at < len {
        let r = unsafe { checker.checkSpellingOfString_startingAt_language_wrap_inSpellDocumentWithTag_wordCount(&s, at as isize, Some(&lang), false, tag(), std::ptr::null_mut()) };
        if r.length == 0 || r.location >= len {
            break;
        }
        out.push(utf16_to_byte(text, r.location)..utf16_to_byte(text, r.location + r.length));
        at = r.location + r.length;
    }
    out
}

/** The checker's suggestions for the word at `word` in `text`, best first. */
pub fn guesses(text: &str, word: Range<usize>, lang: Option<&str>) -> Vec<String> {
    let checker = NSSpellChecker::sharedSpellChecker();
    let range = NSRange::new(text[..word.start].encode_utf16().count(), text[word.clone()].encode_utf16().count());
    checker
        .guessesForWordRange_inString_language_inSpellDocumentWithTag(range, &NSString::from_str(text), Some(&language(lang)), tag())
        .map(|a| a.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

/** Adds a word to the user's dictionary (the one every Mac app shares). */
pub fn learn(word: &str) {
    NSSpellChecker::sharedSpellChecker().learnWord(&NSString::from_str(word));
}


/** Of the checker's marks, the ones to show: none in `skip` (code, links), none in a word that reads as
code or a path (`a_b`, `x.rs`, `/cmd`, `@who`, digits), and not the word the caret is still typing. */
pub fn shown(text: &str, found: Vec<Range<usize>>, skip: &[Range<usize>], caret: Option<usize>) -> Vec<Range<usize>> {
    found
        .into_iter()
        .filter(|r| !skip.iter().any(|s| s.start < r.end && r.start < s.end))
        .filter(|r| caret != Some(r.end))
        .filter(|r| {
            let start = text[..r.start].rfind(char::is_whitespace).map_or(0, |i| i + 1);
            let end = text[r.end..].find(char::is_whitespace).map_or(text.len(), |i| r.end + i);
            let word = text[start..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']).trim_start_matches(['(', '"', '\'']);
            !word.chars().any(|c| c.is_ascii_digit() || "/\\_@#=<>{}[]|~`$%^&*+".contains(c)) && !word.trim_end_matches('.').contains('.')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::shown;

    #[test]
    fn code_paths_and_the_word_being_typed_go_unmarked() {
        let text = "teh file_name.rs at /Users/tj and `wrod` hte";
        let at = |w: &str| text.find(w).map(|i| i..i + w.len()).unwrap();
        let found = vec![at("teh"), at("file"), at("rs"), at("tj"), at("wrod"), at("hte")];
        let code = text.find('`').unwrap()..text.rfind('`').unwrap() + 1;
        assert_eq!(shown(text, found.clone(), &[code.clone()], None), vec![at("teh"), at("hte")]);
        assert_eq!(shown(text, found, &[code], Some(text.len())), vec![at("teh")]);
        let text = "a sentance. Then (anothr), done";
        let at = |w: &str| text.find(w).map(|i| i..i + w.len()).unwrap();
        assert_eq!(shown(text, vec![at("sentance"), at("anothr")], &[], None), vec![at("sentance"), at("anothr")]);
    }
}

#[cfg(test)]
mod native {
    #[test]
    fn a_learned_word_is_spelled_right() {
        let w = "zqxwvabox";
        assert_eq!(super::misspelled(w, Some("en")).len(), 1);
        super::learn(w);
        let after = super::misspelled(w, Some("en"));
        objc2_app_kit::NSSpellChecker::sharedSpellChecker().unlearnWord(&objc2_foundation::NSString::from_str(w));
        assert!(after.is_empty());
        assert_eq!(super::misspelled("I wrote teh speling", Some("en")), vec![8..11, 12..19]);
    }
}

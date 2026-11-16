// SPDX-License-Identifier: GPL-3.0-or-later
//! Editing rules for single-line text boxes that the native EDIT control
//! lacks: Ctrl+Backspace deletes the previous word (EDIT inserts a box
//! character, U+007F, instead).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: u16) -> Class {
    match char::from_u32(c as u32) {
        Some(ch) if ch.is_whitespace() => Class::Space,
        Some(ch) if ch.is_alphanumeric() || ch == '_' => Class::Word,
        // Lone surrogates belong to some (non-BMP) word character.
        None => Class::Word,
        Some(_) => Class::Punct,
    }
}

/// Start (UTF-16 index) of the word before `caret`, as Ctrl+Backspace in
/// Windows text boxes: trailing spaces, then one run of word characters
/// or one run of punctuation.
pub fn word_start_before(text: &[u16], caret: usize) -> usize {
    let mut i = caret.min(text.len());
    while i > 0 && class(text[i - 1]) == Class::Space {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let run = class(text[i - 1]);
    while i > 0 && class(text[i - 1]) == run {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn deletes_words_spaces_and_punctuation() {
        let t = ws("open file.txt  ");
        assert_eq!(word_start_before(&t, t.len()), 10); // "txt  "
        assert_eq!(word_start_before(&t, 10), 9); // "."
        assert_eq!(word_start_before(&t, 9), 5); // "file"
        assert_eq!(word_start_before(&t, 5), 0); // "open "
        assert_eq!(word_start_before(&t, 0), 0);
        assert_eq!(word_start_before(&t, 99), 10);
    }

    #[test]
    fn accents_and_non_bmp_are_word_characters() {
        let t = ws("abrir configuración");
        assert_eq!(word_start_before(&t, t.len()), 6);
        let t = ws("a 😀😀");
        assert_eq!(word_start_before(&t, t.len()), 2);
    }
}

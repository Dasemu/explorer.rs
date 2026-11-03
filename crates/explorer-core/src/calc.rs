// SPDX-License-Identifier: GPL-3.0-or-later
//! Calculator for the Start menu search box.
//!
//! A small recursive-descent evaluator: `+ - * / ^ %`, the typographic
//! `× ÷ −`, parentheses and unary minus. Numbers accept `.` or `,` as the
//! decimal separator (Spanish uses `,`); see [`parse_number`] for the
//! disambiguation rules. `%` after a number is "percent" (`50%` = 0.5,
//! `200 * 10%` = 20); between two operands it is the remainder (`10 % 3`).
//!
//! It never panics: input length, token count and nesting depth are bounded
//! and every error is a `None`.

/// Longest accepted input, in bytes.
pub const MAX_INPUT: usize = 256;
/// Deepest accepted nesting of parentheses / unary operators.
pub const MAX_DEPTH: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok {
    Num(f64),
    Plus,
    Minus,
    Mul,
    Div,
    Pow,
    Percent,
    LParen,
    RParen,
}

/// Parse one number token (digits with `.`/`,` separators).
///
/// - one kind of separator, used once (`3,5` / `3.5`): decimal separator;
/// - both kinds (`1.234,5` / `1,234.5`): the last one is the decimal
///   separator, the other groups thousands;
/// - one kind used several times (`1.000.000`): thousands groups, valid only
///   if every group after the first has exactly three digits;
/// - anything else is rejected.
pub fn parse_number(s: &str) -> Option<f64> {
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return None;
    }
    let dots = s.matches('.').count();
    let commas = s.matches(',').count();
    let normalized: String = match (dots, commas) {
        (0, 0) => s.to_string(),
        (1, 0) => s.to_string(),
        (0, 1) => s.replace(',', "."),
        (_, 0) | (0, _) => {
            let sep = if dots > 0 { '.' } else { ',' };
            let groups: Vec<&str> = s.split(sep).collect();
            if groups[0].is_empty()
                || groups[0].len() > 3
                || groups[1..].iter().any(|g| g.len() != 3)
            {
                return None;
            }
            groups.concat()
        }
        _ => {
            let last_dot = s.rfind('.')?;
            let last_comma = s.rfind(',')?;
            let (dec, thou) = if last_dot > last_comma {
                ('.', ',')
            } else {
                (',', '.')
            };
            if s.matches(dec).count() != 1 {
                return None;
            }
            let (int_part, frac) = s.split_once(dec)?;
            if frac.contains(thou) {
                return None;
            }
            let groups: Vec<&str> = int_part.split(thou).collect();
            if groups[0].is_empty()
                || groups[0].len() > 3
                || groups[1..].iter().any(|g| g.len() != 3)
            {
                return None;
            }
            format!("{}.{}", groups.concat(), frac)
        }
    };
    if normalized.starts_with('.') || normalized.ends_with('.') {
        return None;
    }
    normalized.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn tokenize(input: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => {
                i += 1;
                continue;
            }
            '0'..='9' | '.' | ',' => {
                let start = i;
                while i < chars.len() && matches!(chars[i], '0'..='9' | '.' | ',') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Tok::Num(parse_number(&s)?));
                continue;
            }
            '+' => out.push(Tok::Plus),
            '-' | '\u{2212}' | '\u{2013}' => out.push(Tok::Minus),
            '*' | '\u{00D7}' | '\u{00B7}' | '\u{22C5}' => out.push(Tok::Mul),
            '/' | '\u{00F7}' => out.push(Tok::Div),
            '^' => out.push(Tok::Pow),
            '%' => out.push(Tok::Percent),
            '(' | '[' => out.push(Tok::LParen),
            ')' | ']' => out.push(Tok::RParen),
            _ => return None,
        }
        i += 1;
        if out.len() > MAX_INPUT {
            return None;
        }
    }
    Some(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.pos).copied()
    }

    fn enter(&mut self) -> Option<()> {
        self.depth += 1;
        (self.depth <= MAX_DEPTH).then_some(())
    }

    // expr := term (('+'|'-') term)*
    fn expr(&mut self) -> Option<f64> {
        self.enter()?;
        let mut v = self.term()?;
        while let Some(t @ (Tok::Plus | Tok::Minus)) = self.peek() {
            self.pos += 1;
            let rhs = self.term()?;
            v = if t == Tok::Plus { v + rhs } else { v - rhs };
        }
        self.depth -= 1;
        Some(v)
    }

    // term := unary (('*'|'/'|'%') unary)*
    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::Mul) => {
                    self.pos += 1;
                    v *= self.unary()?;
                }
                Some(Tok::Div) => {
                    self.pos += 1;
                    let d = self.unary()?;
                    if d == 0.0 {
                        return None;
                    }
                    v /= d;
                }
                Some(Tok::Percent) => {
                    // Binary remainder (postfix percent was consumed in
                    // `postfix`, so here an operand must follow).
                    self.pos += 1;
                    let d = self.unary()?;
                    if d == 0.0 {
                        return None;
                    }
                    v %= d;
                }
                _ => return Some(v),
            }
        }
    }

    // unary := ('-'|'+') unary | power
    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.pos += 1;
                self.enter()?;
                let v = -self.unary()?;
                self.depth -= 1;
                Some(v)
            }
            Some(Tok::Plus) => {
                self.pos += 1;
                self.enter()?;
                let v = self.unary()?;
                self.depth -= 1;
                Some(v)
            }
            _ => self.power(),
        }
    }

    // power := postfix ('^' unary)?   (right associative; -2^2 = -4)
    fn power(&mut self) -> Option<f64> {
        let base = self.postfix()?;
        if self.peek() == Some(Tok::Pow) {
            self.pos += 1;
            self.enter()?;
            let exp = self.unary()?;
            self.depth -= 1;
            return Some(base.powf(exp));
        }
        Some(base)
    }

    // postfix := primary '%'*   where '%' is percent only when it is not
    // followed by the start of an operand.
    fn postfix(&mut self) -> Option<f64> {
        let mut v = self.primary()?;
        while self.peek() == Some(Tok::Percent) {
            let next = self.toks.get(self.pos + 1).copied();
            let operand_follows = matches!(
                next,
                Some(Tok::Num(_) | Tok::LParen | Tok::Minus | Tok::Plus)
            );
            if operand_follows {
                break;
            }
            self.pos += 1;
            v /= 100.0;
        }
        Some(v)
    }

    fn primary(&mut self) -> Option<f64> {
        match self.peek()? {
            Tok::Num(n) => {
                self.pos += 1;
                Some(n)
            }
            Tok::LParen => {
                self.pos += 1;
                let v = self.expr()?;
                if self.peek() == Some(Tok::RParen) {
                    self.pos += 1;
                }
                // A missing closing parenthesis at the very end is
                // forgiven (the user is still typing).
                else if self.peek().is_some() {
                    return None;
                }
                Some(v)
            }
            _ => None,
        }
    }
}

/// Evaluate an arithmetic expression. `None` for anything that is not a
/// complete, finite expression.
pub fn evaluate(input: &str) -> Option<f64> {
    if input.len() > MAX_INPUT {
        return None;
    }
    let toks = tokenize(input)?;
    if toks.is_empty() {
        return None;
    }
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let v = p.expr()?;
    if p.pos != p.toks.len() || !v.is_finite() {
        return None;
    }
    Some(v)
}

/// Should the search box show a calculator result for `query`?
///
/// Plain numbers ("2024") are not calculations; a leading `=` forces one.
pub fn looks_like_expression(query: &str) -> bool {
    let q = query.trim();
    if let Some(rest) = q.strip_prefix('=') {
        return !rest.trim().is_empty();
    }
    let has_digit = q.chars().any(|c| c.is_ascii_digit());
    let has_operator = q
        .trim_start_matches(['-', '+', '\u{2212}'])
        .chars()
        .any(|c| "+-*/^%()×÷−".contains(c));
    has_digit && has_operator
}

/// Evaluate a search-box query (handles the optional leading `=`).
pub fn evaluate_query(query: &str) -> Option<f64> {
    if !looks_like_expression(query) {
        return None;
    }
    let q = query.trim();
    evaluate(q.strip_prefix('=').unwrap_or(q))
}

/// Format a result: up to 10 decimals, no trailing zeros, scientific for
/// very large or very small magnitudes. `decimal` is `.` or `,`.
pub fn format_result(v: f64, decimal: char) -> String {
    if !v.is_finite() {
        return String::new();
    }
    let v = if v == 0.0 { 0.0 } else { v }; // no "-0"
    let a = v.abs();
    let s = if a != 0.0 && !(1e-9..1e15).contains(&a) {
        let s = format!("{v:.6e}");
        // Trim mantissa zeros: 1.500000e20 -> 1.5e20.
        match s.split_once('e') {
            Some((m, e)) => {
                let m = if m.contains('.') {
                    m.trim_end_matches('0').trim_end_matches('.')
                } else {
                    m
                };
                format!("{m}e{e}")
            }
            None => s,
        }
    } else {
        let s = format!("{v:.10}");
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s == "-0" {
            "0".to_string()
        } else {
            s
        }
    };
    if decimal == '.' {
        s
    } else {
        s.replace('.', &decimal.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> Option<f64> {
        evaluate(s)
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(ev("1+2*3"), Some(7.0));
        assert_eq!(ev("(1+2)*3"), Some(9.0));
        assert_eq!(ev("2^3^2"), Some(512.0));
        assert_eq!(ev("-2^2"), Some(-4.0));
        assert_eq!(ev("2^-1"), Some(0.5));
        assert_eq!(ev("10/4"), Some(2.5));
        assert_eq!(ev("6 × 7"), Some(42.0));
        assert_eq!(ev("84 ÷ 2"), Some(42.0));
        assert_eq!(ev("50 − 8"), Some(42.0));
        assert_eq!(ev("--3"), Some(3.0));
        assert_eq!(ev("2*(3"), Some(6.0)); // unclosed at end is forgiven
    }

    #[test]
    fn percent_and_remainder() {
        assert_eq!(ev("50%"), Some(0.5));
        assert_eq!(ev("200*10%"), Some(20.0));
        assert_eq!(ev("10 % 3"), Some(1.0));
        assert_eq!(ev("(10%)*2"), Some(0.2));
    }

    #[test]
    fn decimal_separators() {
        assert_eq!(ev("3,5+1"), Some(4.5));
        assert_eq!(ev("3.5+1"), Some(4.5));
        assert_eq!(ev("1.234,5*2"), Some(2469.0));
        assert_eq!(ev("1,234.5*2"), Some(2469.0));
        assert_eq!(ev("1.000.000/1000"), Some(1000.0));
        assert_eq!(ev("1,5,3+1"), None); // ambiguous
        assert_eq!(ev("1.2.3+1"), None);
        assert_eq!(parse_number("12,"), None);
        assert_eq!(parse_number(",5"), None);
        assert_eq!(parse_number("1.234,5"), Some(1234.5));
    }

    #[test]
    fn errors_never_panic() {
        for s in [
            "", "+", "1+", "()", "1/0", "1%0", "abc", "1 2", "((((", "))", "2^99999", "1e5",
            "9^9^9^9", "%", "1%%", "*1", ")1(",
        ] {
            assert!(ev(s).is_none_or(|v| v.is_finite()), "{s}");
        }
        assert_eq!(ev("1/0"), None);
        assert_eq!(ev("2^99999"), None);
        let deep = format!("{}1{}", "(".repeat(500), ")".repeat(500));
        assert_eq!(ev(&deep), None);
        let deep_minus = format!("{}1", "-".repeat(200));
        assert_eq!(ev(&deep_minus), None);
        let long = "1+".repeat(200) + "1";
        assert_eq!(ev(&long), None);
    }

    #[test]
    fn query_detection() {
        assert!(!looks_like_expression("2024"));
        assert!(!looks_like_expression("-5"));
        assert!(!looks_like_expression("notepad"));
        assert!(!looks_like_expression("c++"));
        assert!(looks_like_expression("2+2"));
        assert!(looks_like_expression("=5"));
        assert!(!looks_like_expression("="));
        assert_eq!(evaluate_query("=5"), Some(5.0));
        assert_eq!(evaluate_query("6*7"), Some(42.0));
        assert_eq!(evaluate_query("windows 11"), None);
        assert_eq!(evaluate_query("7-zip"), None);
        assert_eq!(evaluate_query("14:30"), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_result(42.0, '.'), "42");
        assert_eq!(format_result(4.5, ','), "4,5");
        assert_eq!(format_result(1.0 / 3.0, '.'), "0.3333333333");
        assert_eq!(format_result(-0.0, '.'), "0");
        assert_eq!(format_result(0.1 + 0.2, '.'), "0.3");
        assert_eq!(format_result(1.5e20, '.'), "1.5e20");
        assert_eq!(format_result(2.5e-12, ','), "2,5e-12");
        assert_eq!(format_result(f64::NAN, '.'), "");
    }
}

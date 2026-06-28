//! A tiny arithmetic evaluator for the launcher's quick-math feature.
//! Supports + - * / %, parentheses, unary +/-, and decimals. Dependency-free.

/// Evaluate `input` as a math expression. Returns `None` if it isn't a complete,
/// valid expression — including the case where there's no operator at all (so a
/// plain app search like "5" isn't treated as math).
pub fn eval(input: &str) -> Option<f64> {
    let chars: Vec<char> = input.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.is_empty() {
        return None;
    }
    // Require at least one operator so bare numbers/words aren't "math".
    if !chars.iter().any(|c| "+-*/%".contains(*c)) {
        return None;
    }
    // Only digits, operators, parens, dot allowed.
    if !chars
        .iter()
        .all(|c| c.is_ascii_digit() || "+-*/%().".contains(*c))
    {
        return None;
    }
    let mut p = Parser { s: &chars, i: 0 };
    let v = p.expr()?;
    if p.i == chars.len() && v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// Format a result compactly: integers without a trailing `.0`, otherwise
/// trimmed to a reasonable number of decimals.
pub fn format_result(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_string()
    }
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(c) = self.peek() {
            match c {
                '+' => {
                    self.i += 1;
                    v += self.term()?;
                }
                '-' => {
                    self.i += 1;
                    v -= self.term()?;
                }
                _ => break,
            }
        }
        Some(v)
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.factor()?;
        while let Some(c) = self.peek() {
            match c {
                '*' => {
                    self.i += 1;
                    v *= self.factor()?;
                }
                '/' => {
                    self.i += 1;
                    let d = self.factor()?;
                    if d == 0.0 {
                        return None;
                    }
                    v /= d;
                }
                '%' => {
                    self.i += 1;
                    let d = self.factor()?;
                    if d == 0.0 {
                        return None;
                    }
                    v %= d;
                }
                _ => break,
            }
        }
        Some(v)
    }

    fn factor(&mut self) -> Option<f64> {
        match self.peek()? {
            '(' => {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() == Some(')') {
                    self.i += 1;
                    Some(v)
                } else {
                    None
                }
            }
            '-' => {
                self.i += 1;
                Some(-self.factor()?)
            }
            '+' => {
                self.i += 1;
                self.factor()
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' {
                self.i += 1;
            } else {
                break;
            }
        }
        if self.i == start {
            return None;
        }
        let s: String = self.s[start..self.i].iter().collect();
        s.parse().ok()
    }
}

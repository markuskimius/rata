//! File name patterns for `archive.toml`.
//!
//! | Pattern | Matches |
//! |---|---|
//! | `*` | any run of characters, including none (and a leading `.`) |
//! | `?` | any one character |
//! | `[abc]`, `[a-z]` | one character from the set or range |
//! | `[!abc]`, `[^a-z]` | one character not in it |
//! | `{log,txt}` | any of the comma-separated alternatives, which may hold patterns |
//! | `\x` | `x` itself, for a literal `*`, `?`, `[`, `{`, `}`, `,`, or `\` |
//!
//! Everything else matches itself. Case matters. A pattern matches the
//! whole name. In a set, a `]` right after the `[` (or `[!`) is literal.

/// The most alternatives a pattern's `{...}` groups may expand to.
pub const MAX_ALTERNATIVES: usize = 256;

/// A compiled pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    /// The pattern as written.
    pub text: String,
    /// One token sequence per brace alternative.
    alts: Vec<Vec<Tok>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(char),
    One,
    Star,
    Set {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

impl Tok {
    fn matches(&self, c: char) -> bool {
        match self {
            Tok::Lit(l) => *l == c,
            Tok::One => true,
            Tok::Star => false,
            Tok::Set { negated, ranges } => {
                ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)) != *negated
            }
        }
    }
}

impl Glob {
    /// Compile `text`, or say what is wrong with it.
    pub fn new(text: &str) -> Result<Glob, String> {
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        let alts = parse_seq(&chars, &mut i, 0).map_err(|m| format!("pattern \"{text}\": {m}"))?;
        Ok(Glob {
            text: text.to_string(),
            alts,
        })
    }

    /// Whether the pattern matches all of `name`.
    pub fn matches(&self, name: &str) -> bool {
        let t: Vec<char> = name.chars().collect();
        self.alts.iter().any(|p| match_toks(p, &t))
    }
}

fn parse_seq(s: &[char], i: &mut usize, depth: usize) -> Result<Vec<Vec<Tok>>, String> {
    let mut out: Vec<Vec<Tok>> = vec![Vec::new()];
    while *i < s.len() {
        let c = s[*i];
        if depth > 0 && (c == ',' || c == '}') {
            return Ok(out);
        }
        *i += 1;
        let tok = match c {
            '*' => Tok::Star,
            '?' => Tok::One,
            '\\' => {
                let e = *s.get(*i).ok_or("ends with a lone \\")?;
                *i += 1;
                Tok::Lit(e)
            }
            '[' => parse_set(s, i)?,
            '}' => return Err("unmatched } (write \\} for a literal })".into()),
            '{' => {
                let mut alts = Vec::new();
                loop {
                    alts.extend(parse_seq(s, i, depth + 1)?);
                    match s.get(*i) {
                        Some(',') => *i += 1,
                        Some('}') => {
                            *i += 1;
                            break;
                        }
                        _ => return Err("unclosed { (write \\{ for a literal {)".into()),
                    }
                }
                let mut next = Vec::new();
                for head in &out {
                    for alt in &alts {
                        let mut v = head.clone();
                        v.extend(alt.iter().cloned());
                        next.push(v);
                    }
                }
                if next.len() > MAX_ALTERNATIVES {
                    return Err(format!(
                        "{{...}} expands to more than {MAX_ALTERNATIVES} alternatives"
                    ));
                }
                out = next;
                continue;
            }
            other => Tok::Lit(other),
        };
        for seq in &mut out {
            seq.push(tok.clone());
        }
    }
    Ok(out)
}

/// Parse a set after its `[`, up to and including the `]`.
fn parse_set(s: &[char], i: &mut usize) -> Result<Tok, String> {
    let unclosed = || "unclosed [ (write \\[ for a literal [)".to_string();
    let negated = matches!(s.get(*i), Some('!' | '^'));
    if negated {
        *i += 1;
    }
    let mut ranges = Vec::new();
    let mut first = true;
    loop {
        let c = *s.get(*i).ok_or_else(unclosed)?;
        *i += 1;
        if c == ']' && !first {
            break;
        }
        first = false;
        let lo = if c == '\\' {
            let e = *s.get(*i).ok_or_else(unclosed)?;
            *i += 1;
            e
        } else {
            c
        };
        let hi = if s.get(*i) == Some(&'-') && s.get(*i + 1).is_some_and(|n| *n != ']') {
            let mut h = s[*i + 1];
            *i += 2;
            if h == '\\' {
                h = *s.get(*i).ok_or_else(unclosed)?;
                *i += 1;
            }
            if h < lo {
                return Err(format!("range {lo}-{h} is backwards"));
            }
            h
        } else {
            lo
        };
        ranges.push((lo, hi));
    }
    Ok(Tok::Set { negated, ranges })
}

/// Match with one-token-per-character backtracking over the last `*`.
fn match_toks(p: &[Tok], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] != Tok::Star && p[pi].matches(t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == Tok::Star {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|t| *t == Tok::Star)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        Glob::new(p).unwrap().matches(t)
    }

    #[test]
    fn basics() {
        assert!(m("*", ""));
        assert!(m("*", ".hidden"));
        assert!(m("a*c", "abbbc"));
        assert!(m("a?c", "abc"));
        assert!(!m("a?c", "ac"));
        assert!(!m("abc", "ABC"));
        assert!(m("*.log", "x.log"));
        assert!(!m("*.log", "x.log.gz"));
        assert!(m("a,b", "a,b"));
    }

    #[test]
    fn sets() {
        assert!(m("x[0-9].log", "x7.log"));
        assert!(!m("x[0-9].log", "xa.log"));
        assert!(m("x[!0-9]", "xa"));
        assert!(m("x[^0-9]", "xa"));
        assert!(!m("x[!0-9]", "x1"));
        assert!(m("[]a]", "]"));
        assert!(m("[!]]", "a"));
        assert!(m("[a-]", "-"));
        assert!(m("[ab]*", "bcd"));
        assert!(m("*_[0-9][0-9][0-9][0-9]1[12]*", "WEB_20261127_x"));
    }

    #[test]
    fn braces() {
        assert!(m("*.{log,txt}", "a.txt"));
        assert!(!m("*.{log,txt}", "a.csv"));
        assert!(m("{WEB,API}_*.log", "API_1.log"));
        assert!(m("a{b,{c,d}e}", "ade"));
        assert!(m("a{,b}", "a"));
        assert!(m("{x}", "x"));
    }

    #[test]
    fn escapes() {
        assert!(m(r"\*", "*"));
        assert!(!m(r"\*", "a"));
        assert!(m(r"a\{b\}", "a{b}"));
        assert!(m(r"[\]]", "]"));
        assert!(m(r"{a\,b,c}", "a,b"));
    }

    #[test]
    fn errors() {
        for (p, msg) in [
            ("a[b", "unclosed ["),
            ("a{b", "unclosed {"),
            ("a}", "unmatched }"),
            ("a\\", "lone \\"),
            ("[z-a]", "backwards"),
            (
                "{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}{m,n}{o,p}{q,r}",
                "more than 256",
            ),
        ] {
            let e = Glob::new(p).unwrap_err();
            assert!(e.contains(msg), "{p}: {e}");
        }
    }
}

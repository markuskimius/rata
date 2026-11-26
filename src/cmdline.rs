//! Command lines: splitting, `${RATA}` replacement, and exact matching.
//!
//! # Splitting
//!
//! A command written as a string is split into arguments with the platform's
//! rules: POSIX shell quoting on Unix, Windows (`CommandLineToArgvW`) rules on
//! Windows, where a backslash is literal unless it precedes a double quote.
//! A command written as an array is used as-is. No shell is involved.
//!
//! # Matching
//!
//! A process matches a command when:
//!
//! * its program matches the command's program, and
//! * its remaining arguments, joined with single spaces, equal the command's
//!   remaining arguments joined the same way (case-sensitive).
//!
//! The program matches exactly when the command gives a path. When the command
//! gives a bare name (no directory), only the file-name part of the process's
//! program is compared. On Windows, program comparison ignores case and an
//! optional `.exe` extension.
//!
//! As a fallback for programs that rewrite their process title (for example
//! `nginx: master process /usr/sbin/nginx`), a process also matches when its
//! whole command line, joined with single spaces, equals the whole command
//! joined the same way.

use crate::config::CommandSpec;

/// Text replaced by the rata root directory in commands.
pub const RATA_PLACEHOLDER: &str = "${RATA}";

/// A command split into arguments, with `${RATA}` replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    args: Vec<String>,
}

impl CommandLine {
    /// Build a command line from a config value, using this platform's rules.
    pub fn from_spec(spec: &CommandSpec, rata_root: &str) -> Result<Self, String> {
        let raw = match spec {
            CommandSpec::Line(line) => split(line)?,
            CommandSpec::Args(args) => args.clone(),
        };
        Self::from_args(raw.iter().map(|a| expand(a, rata_root)).collect())
    }

    /// Build a command line from already-split arguments.
    pub fn from_args(args: Vec<String>) -> Result<Self, String> {
        match args.first() {
            None => Err("command is empty".into()),
            Some(p) if p.is_empty() => Err("program name is empty".into()),
            Some(_) => Ok(CommandLine { args }),
        }
    }

    /// All arguments, program first.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The program (first argument).
    pub fn program(&self) -> &str {
        &self.args[0]
    }

    /// The arguments joined with single spaces, for display.
    pub fn joined(&self) -> String {
        self.args.join(" ")
    }

    /// Whether the program is a relative path such as `bin/api`
    /// (not a bare name, and not absolute).
    pub fn program_is_relative_path(&self) -> bool {
        has_dir(self.program(), cfg!(windows))
            && !std::path::Path::new(self.program()).is_absolute()
    }

    /// Whether a process with this argument list matches, using this platform's rules.
    pub fn matches(&self, argv: &[String]) -> bool {
        matches_with(&self.args, argv, cfg!(windows))
    }

    /// Whether a process's program (or process name) matches this command's
    /// program. Used to find near misses.
    pub fn program_matches(&self, program: &str) -> bool {
        program_matches_with(self.program(), program, cfg!(windows))
    }
}

/// Replace every `${RATA}` in `arg` with `rata_root`.
pub fn expand(arg: &str, rata_root: &str) -> String {
    arg.replace(RATA_PLACEHOLDER, rata_root)
}

/// Split a command string with this platform's rules.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    if cfg!(windows) {
        split_windows(line)
    } else {
        split_unix(line)
    }
}

/// Split a command string with POSIX shell quoting rules.
pub fn split_unix(line: &str) -> Result<Vec<String>, String> {
    shell_words::split(line).map_err(|_| "missing closing quote".to_string())
}

/// Split a command string with Windows `CommandLineToArgvW` rules.
///
/// * Spaces and tabs separate arguments outside double quotes.
/// * `2n` backslashes before `"` become `n` backslashes, and the quote toggles
///   quoting; `2n+1` backslashes before `"` become `n` backslashes and a
///   literal `"`.
/// * Backslashes not followed by `"` are literal.
/// * Inside quotes, `""` is a literal `"`.
///
/// Unlike `CommandLineToArgvW`, an unclosed quote is an error.
pub fn split_windows(line: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = line.chars().collect();
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' if !quoted => {
                if in_arg {
                    args.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
                i += 1;
            }
            '\\' => {
                let start = i;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                }
                let n = i - start;
                in_arg = true;
                if i < chars.len() && chars[i] == '"' {
                    cur.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        cur.push('"');
                        i += 1;
                    }
                } else {
                    cur.extend(std::iter::repeat_n('\\', n));
                }
            }
            '"' => {
                in_arg = true;
                if quoted && chars.get(i + 1) == Some(&'"') {
                    cur.push('"');
                    i += 2;
                } else {
                    quoted = !quoted;
                    i += 1;
                }
            }
            _ => {
                in_arg = true;
                cur.push(c);
                i += 1;
            }
        }
    }
    if quoted {
        return Err("missing closing quote".into());
    }
    if in_arg {
        args.push(cur);
    }
    Ok(args)
}

/// Exact-match `argv` against `pattern` (both program first).
///
/// `windows` selects Windows program-name rules. See the module docs.
pub fn matches_with(pattern: &[String], argv: &[String], windows: bool) -> bool {
    let argv = trim_trailing_empty(argv);
    let (Some(p0), Some(a0)) = (pattern.first(), argv.first()) else {
        return false;
    };
    if program_matches_with(p0, a0, windows) && pattern[1..].join(" ") == argv[1..].join(" ") {
        return true;
    }
    // Rewritten process titles: compare the whole line.
    pattern.join(" ") == argv.join(" ").trim_end()
}

/// Compare a command's program with a process's program. See the module docs.
pub fn program_matches_with(pattern: &str, program: &str, windows: bool) -> bool {
    if has_dir(pattern, windows) {
        if windows {
            normalize_windows(pattern) == normalize_windows(program)
        } else {
            pattern == program
        }
    } else {
        let base = basename(program, windows);
        if windows {
            normalize_windows(pattern) == normalize_windows(base)
        } else {
            pattern == base
        }
    }
}

fn trim_trailing_empty(argv: &[String]) -> &[String] {
    let mut end = argv.len();
    while end > 0 && argv[end - 1].is_empty() {
        end -= 1;
    }
    &argv[..end]
}

fn is_sep(c: char, windows: bool) -> bool {
    c == '/' || (windows && c == '\\')
}

fn has_dir(program: &str, windows: bool) -> bool {
    program.chars().any(|c| is_sep(c, windows)) || (windows && program.contains(':'))
}

fn basename(program: &str, windows: bool) -> &str {
    match program.rfind(|c| is_sep(c, windows)) {
        Some(i) => &program[i + 1..],
        None => program,
    }
}

fn normalize_windows(s: &str) -> String {
    let s = s.replace('/', "\\").to_lowercase();
    match s.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unix_split() {
        assert_eq!(
            split_unix(r#"bin/api --name "my app" 'x y' a\ b"#).unwrap(),
            v(&["bin/api", "--name", "my app", "x y", "a b"])
        );
        assert!(split_unix(r#"bin/api "oops"#).is_err());
        assert_eq!(
            split_unix("${RATA}/bin/api").unwrap(),
            v(&["${RATA}/bin/api"])
        );
    }

    #[test]
    fn windows_split() {
        assert_eq!(
            split_windows(r#"C:\rata\bin\api.exe --dir "C:\Program Files\x" a"#).unwrap(),
            v(&[r"C:\rata\bin\api.exe", "--dir", r"C:\Program Files\x", "a"])
        );
        assert_eq!(split_windows(r#"a\\"b c" d"#).unwrap(), v(&[r"a\b c", "d"]));
        assert_eq!(split_windows(r#"a\"b"#).unwrap(), v(&[r#"a"b"#]));
        assert_eq!(
            split_windows(r#""say ""hi""""#).unwrap(),
            v(&[r#"say "hi""#])
        );
        assert_eq!(split_windows(r#"a "" b"#).unwrap(), v(&["a", "", "b"]));
        assert_eq!(split_windows("  a\t b  ").unwrap(), v(&["a", "b"]));
        assert!(split_windows(r#"a "b"#).is_err());
    }

    #[test]
    fn expand_after_split() {
        let spec = CommandSpec::Line("${RATA}/bin/api --root ${RATA}".into());
        let cmd = CommandLine::from_spec(&spec, "/opt/my rata").unwrap();
        assert_eq!(
            cmd.args(),
            v(&["/opt/my rata/bin/api", "--root", "/opt/my rata"])
        );
        let spec = CommandSpec::Args(v(&["${RATA}/bin/api", "x y"]));
        let cmd = CommandLine::from_spec(&spec, "/r").unwrap();
        assert_eq!(cmd.args(), v(&["/r/bin/api", "x y"]));
    }

    #[test]
    fn empty_commands() {
        assert!(CommandLine::from_spec(&CommandSpec::Line("  ".into()), "/r").is_err());
        assert!(CommandLine::from_spec(&CommandSpec::Args(vec![]), "/r").is_err());
        assert!(CommandLine::from_spec(&CommandSpec::Args(v(&["", "a"])), "/r").is_err());
    }

    #[test]
    fn exact_match_only() {
        let p = v(&["/opt/rata/bin/api", "--port", "8080"]);
        assert!(matches_with(&p, &p, false));
        assert!(!matches_with(
            &p,
            &v(&["/opt/rata/bin/api", "--port", "8080", "-v"]),
            false
        ));
        assert!(!matches_with(
            &p,
            &v(&["/opt/rata/bin/api", "--port"]),
            false
        ));
        assert!(!matches_with(
            &p,
            &v(&["vim", "/opt/rata/bin/api", "--port", "8080"]),
            false
        ));
        assert!(!matches_with(
            &p,
            &v(&["/opt/rata/bin/API", "--port", "8080"]),
            false
        ));
        assert!(!matches_with(
            &p,
            &v(&["/opt/rata/bin/api", "--PORT", "8080"]),
            false
        ));
        assert!(!matches_with(
            &p,
            &v(&["/other/bin/api", "--port", "8080"]),
            false
        ));
        assert!(!matches_with(&p, &[], false));
    }

    #[test]
    fn path_program_needs_exact_path() {
        let p = v(&["bin/api"]);
        assert!(matches_with(&p, &v(&["bin/api"]), false));
        assert!(!matches_with(&p, &v(&["/opt/rata/bin/api"]), false));
    }

    #[test]
    fn bare_program_matches_basename() {
        let p = v(&["java", "-jar", "x.jar"]);
        assert!(matches_with(&p, &v(&["java", "-jar", "x.jar"]), false));
        assert!(matches_with(
            &p,
            &v(&["/usr/bin/java", "-jar", "x.jar"]),
            false
        ));
        assert!(!matches_with(
            &p,
            &v(&["/usr/bin/Java", "-jar", "x.jar"]),
            false
        ));
        assert!(!matches_with(
            &p,
            &v(&["/usr/bin/java", "-jar", "y.jar"]),
            false
        ));
    }

    #[test]
    fn windows_program_rules() {
        let p = v(&["java", "-jar", "x.jar"]);
        assert!(matches_with(
            &p,
            &v(&[r"C:\Program Files\Java\bin\JAVA.EXE", "-jar", "x.jar"]),
            true
        ));
        assert!(!matches_with(
            &p,
            &v(&[r"C:\Program Files\Java\bin\java.exe", "-jar", "X.jar"]),
            true
        ));
        let p = v(&[r"C:\rata\bin\api.exe", "-v"]);
        assert!(matches_with(&p, &v(&["c:/RATA/bin/api", "-v"]), true));
        assert!(!matches_with(&p, &v(&[r"D:\rata\bin\api.exe", "-v"]), true));
        // On Unix, case and .exe matter.
        assert!(!program_matches_with("api", "api.exe", false));
    }

    #[test]
    fn rewritten_titles_and_trailing_empties() {
        let p = split_unix("nginx: master process /usr/sbin/nginx").unwrap();
        assert!(matches_with(
            &p,
            &v(&["nginx: master process /usr/sbin/nginx"]),
            false
        ));
        assert!(matches_with(
            &p,
            &v(&["nginx: master process /usr/sbin/nginx  ", "", ""]),
            false
        ));
        let p = v(&["/bin/api", "a"]);
        assert!(matches_with(&p, &v(&["/bin/api", "a", ""]), false));
    }

    #[test]
    fn relative_program_detection() {
        let rel = CommandLine::from_args(v(&["bin/api"])).unwrap();
        assert!(rel.program_is_relative_path());
        let bare = CommandLine::from_args(v(&["java"])).unwrap();
        assert!(!bare.program_is_relative_path());
        let abs = if cfg!(windows) {
            r"C:\rata\api.exe"
        } else {
            "/opt/rata/api"
        };
        let abs = CommandLine::from_args(v(&[abs])).unwrap();
        assert!(!abs.program_is_relative_path());
    }
}

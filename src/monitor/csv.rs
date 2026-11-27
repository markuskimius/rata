//! Minimal CSV (RFC 4180): comma separators, `"` quoting, LF line ends.
//!
//! A field is quoted only when it contains a comma, a quote, or a line
//! break. Spaces are kept as they are, so the trailing space of a rata
//! datetime in standard time survives.

/// Append one record, with a trailing `\n`, to `out`.
pub fn write_record<S: AsRef<str>>(out: &mut String, fields: &[S]) {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let f = f.as_ref();
        if f.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&f.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(f);
        }
    }
    out.push('\n');
}

/// Parse complete records from `text`.
///
/// Only records that end with a line break are returned, so a record that
/// was cut off by a crash (or is still being written) is left out. Returns
/// the records and the number of bytes they used.
pub fn parse_records(text: &str) -> (Vec<Vec<String>>, usize) {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut used = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek().is_some_and(|(_, n)| *n == '"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' if chars.peek().is_some_and(|(_, n)| *n == '\n') => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                used = i + 1;
            }
            _ => field.push(c),
        }
    }
    (records, used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let rows = [
            vec!["time", "mount"],
            vec!["2026-11-26 14:30:00.000000000 -0500 ", "/Volumes/My, Disk"],
            vec!["x", "say \"hi\"\nthere"],
            vec!["", ""],
        ];
        let mut out = String::new();
        for r in &rows {
            write_record(&mut out, r);
        }
        assert!(out.contains("-0500 ,\"/Volumes/My, Disk\"\n"));
        let (parsed, used) = parse_records(&out);
        assert_eq!(used, out.len());
        let want: Vec<Vec<String>> = rows
            .iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect();
        assert_eq!(parsed, want);
    }

    #[test]
    fn partial_last_record_is_left_out() {
        let (r, used) = parse_records("a,b\n1,2\n3,");
        assert_eq!(r, [vec!["a", "b"], vec!["1", "2"]]);
        assert_eq!(used, 8);
        let (r, _) = parse_records("a\r\n\"unterminated\n");
        assert_eq!(r, [vec!["a"]]);
    }
}

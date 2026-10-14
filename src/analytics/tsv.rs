//! Strict UTF-8 TSV record reader for Apple analytics segments. Handles an
//! optional BOM, LF/CRLF line ends and quoted cells holding tabs, newlines and
//! doubled quotes. Error text never contains cell contents.
use anyhow::{Result, bail};

pub(super) const MAX_COLUMNS: usize = 64;
pub(super) const MAX_CELL_BYTES: usize = 8192;

pub struct Reader<'a> {
    text: &'a str,
    pos: usize,
}
impl<'a> Reader<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text: text.strip_prefix('\u{feff}').unwrap_or(text),
            pos: 0,
        }
    }
    /// Next record, or `None` once the input is exhausted. A final line
    /// terminator does not open another record.
    pub fn next_record(&mut self) -> Result<Option<Vec<String>>> {
        let text = self.text;
        let bytes = text.as_bytes();
        if self.pos >= text.len() {
            return Ok(None);
        }
        let mut fields = Vec::new();
        loop {
            if fields.len() >= MAX_COLUMNS {
                bail!("segment record exceeds the column limit")
            }
            let mut field = String::new();
            if bytes.get(self.pos) == Some(&b'"') {
                self.pos += 1;
                loop {
                    let Some(i) = text[self.pos..].find('"') else {
                        bail!("unterminated quoted field")
                    };
                    if field.len().saturating_add(i) > MAX_CELL_BYTES {
                        bail!("segment cell exceeds the byte limit")
                    }
                    field.push_str(&text[self.pos..self.pos + i]);
                    self.pos += i + 1;
                    if bytes.get(self.pos) == Some(&b'"') {
                        if field.len() == MAX_CELL_BYTES {
                            bail!("segment cell exceeds the byte limit")
                        }
                        field.push('"');
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                match bytes.get(self.pos) {
                    None => {}
                    Some(b'\t') => {
                        self.pos += 1;
                        fields.push(field);
                        continue;
                    }
                    Some(b'\n') => self.pos += 1,
                    Some(b'\r') if bytes.get(self.pos + 1) == Some(&b'\n') => self.pos += 2,
                    _ => bail!("unexpected character after a quoted field"),
                }
                fields.push(field);
                return Ok(Some(fields));
            }
            let rest = &text[self.pos..];
            let end = rest.find(['\t', '\n']).unwrap_or(rest.len());
            let cell = &rest[..end];
            if cell.len() > MAX_CELL_BYTES {
                bail!("segment cell exceeds the byte limit")
            }
            match rest.as_bytes().get(end) {
                Some(b'\t') => {
                    fields.push(cell.to_owned());
                    self.pos += end + 1;
                }
                terminator => {
                    fields.push(cell.strip_suffix('\r').unwrap_or(cell).to_owned());
                    self.pos += end + usize::from(terminator.is_some());
                    return Ok(Some(fields));
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn all(text: &str) -> Result<Vec<Vec<String>>> {
        let mut reader = Reader::new(text);
        let mut out = Vec::new();
        while let Some(record) = reader.next_record()? {
            out.push(record)
        }
        Ok(out)
    }
    #[test]
    fn bom_crlf_and_trailing_newline() {
        let rows = all("\u{feff}a\tb\r\n1\t2\r\n").unwrap();
        assert_eq!(rows, [["a", "b"], ["1", "2"]]);
        assert_eq!(all("a\tb").unwrap(), [["a", "b"]]);
        assert!(all("").unwrap().is_empty());
    }
    #[test]
    fn quoted_cells_keep_tabs_quotes_and_newlines() {
        let rows = all("a\tb\n\"x\ty\"\t\"say \"\"hi\"\"\nthere\"\r\n\"\"\t\n").unwrap();
        assert_eq!(rows[1], ["x\ty", "say \"hi\"\nthere"]);
        assert_eq!(rows[2], ["", ""]);
    }
    #[test]
    fn rejects_malformed_quoting() {
        assert!(all("\"open\tb\n").is_err());
        assert!(all("\"a\"x\tb\n").is_err());
        // A quote in the middle of an unquoted cell is literal.
        assert_eq!(all("a\"b\tc\n").unwrap(), [["a\"b", "c"]]);
    }
    #[test]
    fn rejects_excessive_columns_before_unbounded_vector_growth() {
        assert!(all(&format!("{}x\n", "x\t".repeat(64))).is_err());
    }
    #[test]
    fn rejects_oversized_quoted_and_unquoted_cells() {
        let large = "x".repeat(8193);
        assert!(all(&format!("{large}\n")).is_err());
        assert!(all(&format!("\"{large}\"\n")).is_err());
    }
}

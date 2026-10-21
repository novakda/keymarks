//! Lossless reader/writer for Powermarks 3.5 `pm35.htm` files.
//! Format notes: `docs/pm35-format.md`.
//!
//! Text is kept exactly as stored (still HTML-entity encoded) so `write(parse(x)) == x`.

/// mdata field indices recovered from pm.exe (see docs/pm35-format.md).
pub mod field {
    pub const SCHEDULE: usize = 0;
    pub const ADDED: usize = 1;
    pub const LAST_VISITED: usize = 2;
    pub const LAST_MODIFIED: usize = 3;
    pub const LAST_CHECKED: usize = 4;
    pub const STATUS_TIME: usize = 5;
    pub const STATUS: usize = 6;
    pub const HASH_MODE: usize = 7;
    pub const CHECK_ENABLED: usize = 8;
    pub const PAGE_HASH: usize = 9;
    pub const VISITS: usize = 10;
    pub const RATING: usize = 11;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Database {
    /// Everything before the first entry: doctype, document id, schedules, macros, `<DL>`.
    pub head: String,
    pub entries: Vec<Entry>,
    /// Everything after the last entry: `</DL><p>`, tagged-keyword dictionary, closing tags.
    pub tail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    /// `(name, value)` in file order; names are keywords, notes, desc, user_field.
    pub extras: Vec<(String, String)>,
    /// The 15 raw mdata values.
    pub mdata: Vec<String>,
}

impl Entry {
    pub fn extra(&self, name: &str) -> Option<&str> {
        self.extras.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// Hex mdata field as a number (timestamps are Unix seconds). None if empty or invalid.
    pub fn num(&self, i: usize) -> Option<u32> {
        self.mdata.get(i).and_then(|v| u32::from_str_radix(v, 16).ok())
    }
}

const ENTRY: &str = "<DT><A HREF=\"";
const END: &str = "</DL><p>";

/// Parse raw file bytes (Latin-1).
pub fn parse(bytes: &[u8]) -> Result<Database, String> {
    let text: String = bytes.iter().map(|&b| b as char).collect();
    let t = text.as_str();
    let first = t.find(ENTRY).unwrap_or_else(|| t.find(END).unwrap_or(t.len()));
    let end = t.rfind(END).ok_or("missing </DL><p>")?;
    let mut entries = Vec::new();
    let mut pos = first;
    while pos < end {
        let (entry, next) = parse_entry(t, pos).map_err(|e| format!("entry at char {pos}: {e}"))?;
        entries.push(entry);
        pos = next;
    }
    Ok(Database { head: t[..first].to_string(), entries, tail: t[end..].to_string() })
}

fn parse_entry(t: &str, pos: usize) -> Result<(Entry, usize), String> {
    let rest = t[pos..].strip_prefix(ENTRY).ok_or("expected <DT><A HREF=\"")?;
    let (url, rest) = rest.split_once("\">").ok_or("unterminated HREF")?;
    let (title, rest) = rest.split_once("</A>\r\n<DD>").ok_or("missing </A><DD>")?;
    let mut rest = rest;
    let mut extras = Vec::new();
    loop {
        let r = rest.strip_prefix("<!--").ok_or("expected <!--")?;
        if let Some(r) = r.strip_prefix("mdata=[") {
            let (raw, r) = r.split_once("]-->\r\n<BR>\r\n").ok_or("unterminated mdata")?;
            let mdata = raw.split("][").map(String::from).collect();
            let entry = Entry { url: url.into(), title: title.into(), extras, mdata };
            return Ok((entry, t.len() - r.len()));
        }
        let (name, r) = r.split_once("-->").ok_or("unterminated tag")?;
        let (value, r) = r.split_once("\r\n<!--").ok_or("field not followed by a tag")?;
        extras.push((name.to_string(), value.to_string()));
        rest = &rest[rest.len() - r.len() - 4..]; // keep the "<!--"
    }
}

/// Serialize back to Latin-1 bytes. Chars above U+00FF are written as `?`.
pub fn write(db: &Database) -> Vec<u8> {
    let mut s = db.head.clone();
    for e in &db.entries {
        s += &format!("{ENTRY}{}\">{}</A>\r\n<DD>", e.url, e.title);
        for (name, value) in &e.extras {
            s += &format!("<!--{name}-->{value}\r\n");
        }
        s += &format!("<!--mdata=[{}]-->\r\n<BR>\r\n", e.mdata.join("]["));
    }
    s += &db.tail;
    s.chars().map(|c| u8::try_from(c).unwrap_or(b'?')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"<!DOCTYPE HTML PUBLIC \"-//IETF//DTD HTML 3.2//EN\">\r\n<!--Powermarks Format 3.5-->\r\n<HTML>\r\n<HEAD>\r\n<!--document=[User][C0241D718E1DBEFA00F0B4CAC4D4A9BB]-->\r\n<!--scs=[Weekly][1][93A80][37D0B99E]-->\r\n</HEAD>\r\n<BODY>\r\n<DL>\r\n\
<DT><A HREF=\"http://example.com/a?x=1&amp;y=2\">Caf\xe9 &amp; Bar</A>\r\n<DD><!--keywords-->food paris\r\n<!--notes-->line one\r\n<!--desc-->A caf\xe9\r\n<!--user_field-->u\r\n<!--mdata=[Weekly][634DF1F5][65DAAC91][65DAAC73][65DAAC73][65DAAC73][C][1][1][6CCD5516][1][A][][][0]-->\r\n<BR>\r\n\
<DT><A HREF=\"http://example.com/b\">Plain</A>\r\n<DD><!--mdata=[Weekly][634DF3B9][0][0][0][65DAAC26][0][0][1][][0][0][][][0]-->\r\n<BR>\r\n\
</DL><p>\r\n<!--kword=[food][1]-->\r\n</BODY>\r\n</HTML>";

    #[test]
    fn round_trip_and_fields() {
        let db = parse(SAMPLE).unwrap();
        assert_eq!(write(&db), SAMPLE);
        assert_eq!(db.entries.len(), 2);
        let a = &db.entries[0];
        assert_eq!(a.title, "Caf\u{e9} &amp; Bar");
        assert_eq!(a.extra("keywords"), Some("food paris"));
        assert_eq!(a.extra("user_field"), Some("u"));
        assert_eq!(a.mdata.len(), 15);
        assert_eq!(a.num(field::RATING), Some(10));
        assert_eq!(a.num(field::ADDED), Some(0x634DF1F5));
        assert_eq!(db.entries[1].extras, vec![]);
        assert_eq!(db.entries[1].mdata[field::PAGE_HASH], "");
    }

    #[test]
    fn empty_database() {
        let s = b"<HTML><BODY><DL>\r\n</DL><p>\r\n</BODY>\r\n</HTML>";
        let db = parse(s).unwrap();
        assert!(db.entries.is_empty());
        assert_eq!(write(&db), s);
    }

    /// Run against a real file: PM35_REAL=/path/to/pm35.htm cargo test -- --ignored
    #[test]
    #[ignore]
    fn real_file_round_trip() {
        let path = std::env::var("PM35_REAL").expect("set PM35_REAL");
        let bytes = std::fs::read(path).unwrap();
        let db = parse(&bytes).unwrap();
        assert!(db.entries.iter().all(|e| e.mdata.len() == 15));
        assert!(write(&db) == bytes, "round trip differs");
        eprintln!("{} entries round-tripped", db.entries.len());
    }
}

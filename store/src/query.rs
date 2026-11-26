//! Powermarks search syntax compiled to SQL.
//!
//! `a b`, `a & b`, `a .and b`: both · `a | b`, `a .or b`: either · `!a`, `.not a`: without ·
//! `( )`: grouping · `word.`: whole word (otherwise prefix). Precedence: NOT > AND > OR.
//! The dotted operators exist so plain `and`/`or`/`not` stay ordinary search words.

use serde::Deserialize;

/// Exact-mode fields. `folder` is the item's folder path (FIND-07), the others are columns.
pub const FIELDS: [&str; 6] = ["title", "keywords", "description", "notes", "url", "folder"];
/// Powermarks' field-mask bits (column order in pm.exe); bits 5–7 (status checking,
/// schedule, status) have no text to search here.
pub const PM_FIELD_BITS: [&str; 5] = ["title", "url", "keywords", "description", "notes"];

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Words anywhere; keyword matches rank first.
    #[default]
    All,
    /// Words in the keywords field only (the original's keyword search).
    Keywords,
    /// Plain substring over chosen fields; no operators (the original's exact search).
    Exact,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SearchOptions {
    pub mode: Mode,
    /// Prefix matching (`comp` finds `computer`); off means whole words only.
    pub partial: bool,
    /// Exact mode: fields to search.
    pub fields: Vec<String>,
    /// Exact mode: case-sensitive.
    pub match_case: bool,
    /// Column sort, most significant first (the original kept the previous key as tiebreak).
    pub sort: Vec<SortKey>,
    /// The folder shown when the search is empty (Explorer style); None: the whole collection.
    pub folder: Option<i64>,
    /// A search covers `folder` and its subfolders only (FIND-08); off: the whole collection.
    pub in_folder: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SortKey {
    pub column: String,
    #[serde(default)]
    pub desc: bool,
}

impl SortKey {
    /// ORDER BY term for a known column; None for anything else (never interpolate input).
    pub fn sql(&self) -> Option<String> {
        let expr = match self.column.as_str() {
            "title" | "url" | "keywords" | "description" | "notes" => format!("b.{} COLLATE NOCASE", self.column),
            "check_status" | "schedule" => format!("b.{} COLLATE NOCASE", self.column),
            "added" | "last_visited" | "last_modified" | "last_checked" | "visits" | "rating" | "status" | "changed" => {
                format!("b.{}", self.column)
            }
            _ => return None,
        };
        Some(format!("{expr} {} NULLS LAST", if self.desc { "DESC" } else { "ASC" }))
    }
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            mode: Mode::All,
            partial: true,
            fields: FIELDS.map(String::from).to_vec(),
            match_case: false,
            sort: Vec::new(),
            folder: None,
            in_folder: false,
        }
    }
}

/// A compiled query: SQL boolean over `b.id` with `?` parameters, plus an FTS5 MATCH string
/// of the positive terms for ranking (None when there is nothing to rank by).
#[derive(Debug, PartialEq)]
pub struct Filter {
    pub sql: String,
    pub params: Vec<String>,
    pub rank: Option<String>,
}

/// None when the query has nothing to search for (show everything).
pub fn compile(query: &str, opts: &SearchOptions) -> Result<Option<Filter>, String> {
    if opts.mode == Mode::Exact {
        return exact(query, opts);
    }
    let mut p = Parser { toks: tokenize(query), i: 0 };
    if p.toks.is_empty() {
        return Ok(None);
    }
    let expr = p.or()?;
    if p.i < p.toks.len() {
        return Err("unexpected ')'".into());
    }
    let mut f = Filter { sql: String::new(), params: Vec::new(), rank: None };
    let mut positive = Vec::new();
    emit(&expr, opts, false, &mut f, &mut positive);
    f.rank = (!positive.is_empty()).then(|| positive.join(" OR "));
    Ok(Some(f))
}

fn exact(query: &str, opts: &SearchOptions) -> Result<Option<Filter>, String> {
    let needle = query.trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let fields: Vec<&str> = FIELDS.into_iter().filter(|f| opts.fields.iter().any(|o| o == f)).collect();
    if fields.is_empty() {
        return Err("choose at least one field for exact search".into());
    }
    // ponytail: SQLite lower() folds ASCII only; non-ASCII case-insensitive matching needs ICU or a Rust-side scan.
    let found = |text: &str| if opts.match_case { format!("instr({text}, ?) > 0") } else { format!("instr(lower({text}), lower(?)) > 0") };
    let clause = |f: &str| {
        if f == "folder" { format!("b.folder_id IN ({FOLDER_PATHS} SELECT id FROM p WHERE {})", found("path")) } else { found(&format!("b.{f}")) }
    };
    Ok(Some(Filter {
        sql: format!("({})", fields.iter().map(|f| clause(f)).collect::<Vec<_>>().join(" OR ")),
        params: vec![needle.to_string(); fields.len()],
        rank: None,
    }))
}

/// Every folder below the root with its path ("Search engines › Web"), as CTE `p(id, path)`.
const FOLDER_PATHS: &str = "WITH RECURSIVE p(id, path) AS (SELECT id, name FROM folder WHERE parent_id = 1 \
    UNION ALL SELECT f.id, p.path || ' › ' || f.name FROM folder f JOIN p ON f.parent_id = p.id)";

/// Folders whose path has a word matching `?` (an FTS5 query): a folder whose own name
/// matches, and every folder below it. Moves need no re-index; renames re-index one row.
const PATH_MATCH: &str = "b.folder_id IN (WITH RECURSIVE m(id) AS (SELECT rowid FROM folder_fts \
    WHERE folder_fts MATCH ? AND rowid <> 1 UNION SELECT f.id FROM folder f JOIN m ON f.parent_id = m.id) SELECT id FROM m)";

/// The folder `?` and every folder below it.
pub(crate) const SUBTREE: &str = "WITH RECURSIVE t(id) AS (SELECT ? UNION ALL SELECT f.id FROM folder f JOIN t ON f.parent_id = t.id) SELECT id FROM t";

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    And,
    Or,
    Not,
    Open,
    Close,
    Word(String),
}

#[derive(Debug, PartialEq)]
enum Expr {
    Term { word: String, whole: bool },
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

fn tokenize(q: &str) -> Vec<Tok> {
    fn flush(word: &mut String, out: &mut Vec<Tok>) {
        let w = std::mem::take(word);
        match w.to_lowercase().as_str() {
            "" => {}
            ".and" => out.push(Tok::And),
            ".or" => out.push(Tok::Or),
            ".not" => out.push(Tok::Not),
            // Punctuation-only "words" can't match anything in the index; ignore them.
            _ if !w.chars().any(char::is_alphanumeric) => {}
            _ => out.push(Tok::Word(w)),
        }
    }
    let mut out = Vec::new();
    let mut word = String::new();
    for c in q.chars() {
        let op = match c {
            '&' => Some(Tok::And),
            '|' => Some(Tok::Or),
            '!' => Some(Tok::Not),
            '(' => Some(Tok::Open),
            ')' => Some(Tok::Close),
            _ => None,
        };
        if let Some(op) = op {
            flush(&mut word, &mut out);
            out.push(op);
        } else if c.is_whitespace() {
            flush(&mut word, &mut out);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.peek() == Some(&Tok::Or) {
            self.i += 1;
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::And) => self.i += 1,
                Some(Tok::Word(_) | Tok::Not | Tok::Open) => {} // implicit AND
                _ => return Ok(left),
            }
            left = Expr::And(Box::new(left), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Tok::Not) {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        let tok = self.peek().cloned();
        self.i += 1;
        match tok {
            Some(Tok::Word(w)) => {
                let trimmed = w.trim_end_matches('.');
                Ok(Expr::Term { word: trimmed.to_string(), whole: trimmed.len() < w.len() })
            }
            Some(Tok::Open) => {
                let inner = self.or()?;
                if self.peek() != Some(&Tok::Close) {
                    return Err("missing ')'".into());
                }
                self.i += 1;
                Ok(inner)
            }
            Some(Tok::Close) => Err("unexpected ')'".into()),
            Some(_) => Err("an operator needs a word on each side".into()),
            None => Err("the search ends with an operator".into()),
        }
    }
}

/// `rating>=7`, `rating<3`, `rating=0` (0 = not rated): a filter instead of a word.
fn rating_filter(word: &str) -> Option<(&'static str, i64)> {
    let rest = word.strip_prefix("rating").or_else(|| word.strip_prefix("Rating"))?;
    let (op, n) = [(">=", ">="), ("<=", "<="), (">", ">"), ("<", "<"), ("=", "=")]
        .into_iter()
        .find_map(|(p, op)| rest.strip_prefix(p).map(|n| (op, n)))?;
    Some((op, n.parse().ok()?))
}

fn emit(e: &Expr, opts: &SearchOptions, negated: bool, f: &mut Filter, positive: &mut Vec<String>) {
    match e {
        Expr::Term { word, .. } if rating_filter(word).is_some() => {
            let (op, n) = rating_filter(word).unwrap();
            f.sql += &format!("b.rating {op} {n}"); // op from a fixed list, n parsed as a number
        }
        Expr::Term { word, whole } => {
            let star = if *whole || !opts.partial { "" } else { "*" };
            let column = if opts.mode == Mode::Keywords { "keywords : " } else { "" };
            let bare = format!("\"{}\"{star}", word.replace('"', "\"\""));
            let phrase = format!("{column}{bare}");
            let in_text = "b.id IN (SELECT rowid FROM bookmark_fts WHERE bookmark_fts MATCH ?)";
            if !negated {
                positive.push(phrase.clone());
            }
            f.params.push(phrase);
            if opts.mode == Mode::Keywords {
                f.sql += in_text;
            } else {
                // The folder path counts as text of the item (FIND-07).
                f.sql += &format!("({in_text} OR {PATH_MATCH})");
                f.params.push(bare);
            }
        }
        Expr::Not(inner) => {
            f.sql += "NOT (";
            emit(inner, opts, !negated, f, positive);
            f.sql += ")";
        }
        Expr::And(l, r) | Expr::Or(l, r) => {
            let op = if matches!(e, Expr::And(..)) { " AND " } else { " OR " };
            f.sql += "(";
            emit(l, opts, negated, f, positive);
            f.sql += op;
            emit(r, opts, negated, f, positive);
            f.sql += ")";
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the parse tree compactly: & | ! and ~ for whole words.
    fn tree(q: &str) -> Result<String, String> {
        fn show(e: &Expr) -> String {
            match e {
                Expr::Term { word, whole } => format!("{word}{}", if *whole { "~" } else { "" }),
                Expr::Not(i) => format!("!{}", show(i)),
                Expr::And(l, r) => format!("({} & {})", show(l), show(r)),
                Expr::Or(l, r) => format!("({} | {})", show(l), show(r)),
            }
        }
        let mut p = Parser { toks: tokenize(q), i: 0 };
        let e = p.or()?;
        if p.i < p.toks.len() {
            return Err("trailing".into());
        }
        Ok(show(&e))
    }

    #[test]
    fn parses_powermarks_syntax() {
        assert_eq!(tree("a b").unwrap(), "(a & b)");
        assert_eq!(tree("a & b | c").unwrap(), "((a & b) | c)");
        assert_eq!(tree("stock ! free").unwrap(), "(stock & !free)");
        assert_eq!(tree("!free").unwrap(), "!free");
        assert_eq!(tree("(comp|sci)&mag").unwrap(), "((comp | sci) & mag)");
        assert_eq!(tree("a .or b .and .not c").unwrap(), "(a | (b & !c))");
        assert_eq!(tree("or not and").unwrap(), "((or & not) & and)"); // plain words
        assert_eq!(tree("win. v1.0").unwrap(), "(win~ & v1.0)");
        assert_eq!(tree("a - b").unwrap(), "(a & b)"); // punctuation-only word ignored
        assert!(tree("(a | b").unwrap_err().contains("missing ')'"));
        assert!(tree("a | b)").is_err());
        assert!(tree("a &").unwrap_err().contains("ends with"));
        assert!(tree("| a").unwrap_err().contains("operator"));
    }

    #[test]
    fn compiles_modes() {
        let all = SearchOptions::default();
        assert_eq!(compile("  ", &all).unwrap(), None);
        let f = compile("rust ! java.", &all).unwrap().unwrap();
        assert_eq!(f.params, ["\"rust\"*", "\"rust\"*", "\"java\"", "\"java\""], "each word: text, then folder path");
        assert_eq!(f.rank.as_deref(), Some("\"rust\"*")); // negated terms don't rank

        let kw = SearchOptions { mode: Mode::Keywords, partial: false, ..all.clone() };
        assert_eq!(compile("rust", &kw).unwrap().unwrap().params, ["keywords : \"rust\""]);

        let exact = SearchOptions { mode: Mode::Exact, fields: vec!["url".into()], ..all.clone() };
        let f = compile(" .edu/ ", &exact).unwrap().unwrap();
        assert_eq!(f.sql, "(instr(lower(b.url), lower(?)) > 0)");
        assert_eq!(f.params, [".edu/"]);
        let none = SearchOptions { mode: Mode::Exact, fields: vec![], ..all };
        assert!(compile("x", &none).is_err());
    }

    #[test]
    fn rating_filters() {
        let opts = SearchOptions::default();
        let f = compile("rust rating>=7", &opts).unwrap().unwrap();
        assert!(f.sql.contains("b.rating >= 7") && f.params.len() == 2, "{}", f.sql);
        assert!(compile("rating<3 | rating=0", &opts).unwrap().unwrap().sql.contains("b.rating < 3 OR b.rating = 0"));
        assert_eq!(rating_filter("rating>=x"), None, "not a number: an ordinary word");
        assert_eq!(rating_filter("ratings"), None);
    }

    #[test]
    fn sort_keys_are_whitelisted() {
        let key = |c: &str, desc| SortKey { column: c.into(), desc }.sql();
        assert_eq!(key("rating", true).unwrap(), "b.rating DESC NULLS LAST");
        assert_eq!(key("title", false).unwrap(), "b.title COLLATE NOCASE ASC NULLS LAST");
        assert_eq!(key("id; DROP TABLE bookmark", false), None);
    }
}

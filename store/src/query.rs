//! Powermarks search syntax compiled to SQL.
//!
//! `a b`, `a & b`, `a .and b`: both · `a | b`, `a .or b`: either · `!a`, `.not a`: without ·
//! `( )`: grouping · `word.`: whole word (otherwise prefix). Precedence: NOT > AND > OR.
//! The dotted operators exist so plain `and`/`or`/`not` stay ordinary search words.

use serde::Deserialize;

pub const FIELDS: [&str; 5] = ["title", "keywords", "description", "notes", "url"];

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
            "check_status" => "b.check_status COLLATE NOCASE".to_string(),
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
    let clause = |f: &str| {
        if opts.match_case { format!("instr(b.{f}, ?) > 0") } else { format!("instr(lower(b.{f}), lower(?)) > 0") }
    };
    Ok(Some(Filter {
        sql: format!("({})", fields.iter().map(|f| clause(f)).collect::<Vec<_>>().join(" OR ")),
        params: vec![needle.to_string(); fields.len()],
        rank: None,
    }))
}

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

fn emit(e: &Expr, opts: &SearchOptions, negated: bool, f: &mut Filter, positive: &mut Vec<String>) {
    match e {
        Expr::Term { word, whole } => {
            let star = if *whole || !opts.partial { "" } else { "*" };
            let column = if opts.mode == Mode::Keywords { "keywords : " } else { "" };
            let phrase = format!("{column}\"{}\"{star}", word.replace('"', "\"\""));
            f.sql += "b.id IN (SELECT rowid FROM bookmark_fts WHERE bookmark_fts MATCH ?)";
            if !negated {
                positive.push(phrase.clone());
            }
            f.params.push(phrase);
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
        assert_eq!(f.params, ["\"rust\"*", "\"java\""]);
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
    fn sort_keys_are_whitelisted() {
        let key = |c: &str, desc| SortKey { column: c.into(), desc }.sql();
        assert_eq!(key("rating", true).unwrap(), "b.rating DESC NULLS LAST");
        assert_eq!(key("title", false).unwrap(), "b.title COLLATE NOCASE ASC NULLS LAST");
        assert_eq!(key("id; DROP TABLE bookmark", false), None);
    }
}

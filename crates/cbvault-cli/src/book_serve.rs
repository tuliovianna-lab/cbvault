//! `cbvault book serve <book> [<book>…]`: keeps books open and answers probes
//! over standard input and output, one line each way, so another program (an
//! app's backend, say) can read `.ctg` books directly without exporting them.
//!
//! On start it opens every book and prints one **ready** line:
//!
//! ```text
//! {"event":"ready","books":[{"name":"PowerBook2025","path":"…","positions":27592793,"index_seconds":0.71}],"errors":[]}
//! ```
//!
//! A book's `name` is its file name without the extension. A book that cannot
//! be opened (for example because ChessBase has it open and locked) is listed
//! in `errors` with the reason, and the others are served anyway.
//!
//! Then each **request** is one line of tab-separated fields:
//!
//! ```text
//! <id> TAB <book name> TAB fen TAB <FEN>
//! <id> TAB <book name> TAB moves TAB <moves from the initial position, space-separated, SAN or UCI>
//! ```
//!
//! and each **response** is one line: `{"id":<id>,` followed by exactly what
//! `cbvault book probe --json` prints for that position, or
//! `{"id":<id>,"error":"…"}` for a bad request (the process stays up). `id` is
//! echoed as given and must be digits. The process ends when its input closes.
//!
//! Tabs keep the format free of quoting: a FEN and SAN moves never contain one.

use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Instant;

use cbvault::book::Book;
use gigachess::Board;

use super::json_str;

type CliResult = Result<bool, Box<dyn std::error::Error>>;

/// The books being served, by name.
pub(crate) struct Served {
    books: Vec<(String, Book)>,
}

impl Served {
    fn get(&self, name: &str) -> Option<&Book> {
        self.books.iter().find(|(n, _)| n == name).map(|(_, b)| b)
    }
}

/// A book's name: its file name without the extension.
fn book_name(path: &str) -> String {
    let p = Path::new(path);
    p.file_stem().map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned())
}

/// Opens every book; returns them and the ready line.
pub(crate) fn open_all(paths: &[String]) -> (Served, String) {
    let mut books = Vec::new();
    let mut listed = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        let name = book_name(path);
        if books.iter().any(|(n, _)| *n == name) {
            errors.push(format!(
                "{{\"name\":{},\"path\":{},\"error\":\"another book has this name\"}}",
                json_str(&name),
                json_str(path)
            ));
            continue;
        }
        let t0 = Instant::now();
        match Book::open(path) {
            Ok(book) => {
                listed.push(format!(
                    "{{\"name\":{},\"path\":{},\"positions\":{},\"damaged_pages\":{},\"index_seconds\":{:.2}}}",
                    json_str(&name),
                    json_str(&book.path().display().to_string()),
                    book.positions(),
                    book.damaged().len(),
                    t0.elapsed().as_secs_f64()
                ));
                books.push((name, book));
            }
            Err(e) => errors.push(format!(
                "{{\"name\":{},\"path\":{},\"error\":{}}}",
                json_str(&name),
                json_str(path),
                json_str(&e.to_string())
            )),
        }
    }
    let ready = format!("{{\"event\":\"ready\",\"books\":[{}],\"errors\":[{}]}}", listed.join(","), errors.join(","));
    (Served { books }, ready)
}

/// Answers one request line.
pub(crate) fn answer(served: &Served, line: &str) -> String {
    let mut fields = line.splitn(4, '\t');
    let id = fields.next().unwrap_or("");
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || id.len() > 18 {
        return format!(
            "{{\"id\":null,\"error\":{}}}",
            json_str("a request starts with a numeric id, then tabs: id, book, fen|moves, value")
        );
    }
    let fail = |msg: String| format!("{{\"id\":{id},\"error\":{}}}", json_str(&msg));
    let (Some(name), Some(kind)) = (fields.next(), fields.next()) else {
        return fail("expected: id TAB book TAB fen|moves TAB value".into());
    };
    let value = fields.next().unwrap_or("").trim_end_matches(['\r', '\n']);
    let Some(book) = served.get(name) else {
        return fail(format!("no book named {name}"));
    };
    let board = match kind {
        "fen" => match gigachess::fen::parse_fen(value.trim()) {
            Ok(b) => b,
            Err(e) => return fail(format!("bad FEN: {}", e.0)),
        },
        "moves" => {
            let mut b = Board::startpos();
            let moves: Vec<String> = value.split_whitespace().map(str::to_string).collect();
            if let Err(e) = super::book::play_moves(&mut b, &moves) {
                return fail(e.to_string());
            }
            b
        }
        other => return fail(format!("unknown request kind {other}: use fen or moves")),
    };
    match super::book::probe_json(book, &board) {
        Ok((json, _)) => format!("{{\"id\":{id},{}", &json[1..]),
        Err(e) => fail(e.to_string()),
    }
}

pub(crate) fn run_serve(args: impl Iterator<Item = String>) -> CliResult {
    let paths: Vec<String> = args.collect();
    if paths.is_empty() || paths.iter().any(|p| p.starts_with("--")) {
        return Err(super::USAGE.into());
    }
    let (served, ready) = open_all(&paths);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{ready}")?;
    out.flush()?;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        writeln!(out, "{}", answer(&served, &line))?;
        out.flush()?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a one-entry book (the initial position, one move) to a fresh
    /// directory. Synthetic: no real book content.
    fn synthetic_book(dir: &Path) -> String {
        use cbvault_chess::ctg::{decode_byte, keys, stored_form};
        use cbvault_format::ctg::{PAGE_SIZE, Stats};

        let start = Board::startpos();
        let stored = stored_form(&start);
        let e4 = (0u8..=255).find(|&b| decode_byte(&start, &stored, b).is_ok_and(|m| m.to_string() == "e2e4")).unwrap();
        let (k, l) = keys(&stored)[0];
        let mut entry = k[..l].to_vec();
        entry.extend_from_slice(&[3, e4, 0x01]);
        entry.extend_from_slice(&Stats { games: 7, wins: 3, draws: 2, losses: 2, ..Stats::default() }.to_bytes());
        let mut file = vec![0u8; 2 * PAGE_SIZE];
        let page = &mut file[PAGE_SIZE..];
        page[0..2].copy_from_slice(&1u16.to_be_bytes());
        page[2..4].copy_from_slice(&((4 + entry.len()) as u16).to_be_bytes());
        page[4..4 + entry.len()].copy_from_slice(&entry);
        let path = dir.join("Tiny Book.ctg");
        std::fs::write(&path, file).unwrap();
        path.display().to_string()
    }

    #[test]
    fn requests_are_answered_and_bad_ones_do_not_stop_the_server() {
        let dir = std::env::temp_dir().join(format!("cbvault-serve-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = synthetic_book(&dir);
        let missing = dir.join("Missing.ctg").display().to_string();
        let (served, ready) = open_all(&[path.clone(), missing]);
        assert!(ready.starts_with("{\"event\":\"ready\",\"books\":[{\"name\":\"Tiny Book\""), "{ready}");
        assert!(ready.contains("\"errors\":[{\"name\":\"Missing\""), "{ready}");

        let start = Board::startpos();
        let by_fen = answer(&served, &format!("7\tTiny Book\tfen\t{}", start.to_fen()));
        let (direct, found) = super::super::book::probe_json(served.get("Tiny Book").unwrap(), &start).unwrap();
        assert!(found);
        assert_eq!(by_fen, format!("{{\"id\":7,{}", &direct[1..]));
        assert!(by_fen.contains("\"games\":7") && by_fen.contains("\"uci\":\"e2e4\""), "{by_fen}");

        let by_moves = answer(&served, "8\tTiny Book\tmoves\te4");
        assert!(by_moves.starts_with("{\"id\":8,") && by_moves.contains("\"found\":false"), "{by_moves}");
        assert!(answer(&served, "9\tTiny Book\tmoves\te5").contains("\"error\":\"move 1 (e5) is not legal here\""));
        assert!(answer(&served, "10\tNo Such\tfen\tx").contains("no book named No Such"));
        assert!(answer(&served, "11\tTiny Book\tfen\tnot a fen").contains("bad FEN"));
        assert!(answer(&served, "x\tTiny Book").starts_with("{\"id\":null,\"error\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

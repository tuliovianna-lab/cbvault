//! `cbvault book export` and `cbvault book verify-export`.
//!
//! - `book export <book> [--sqlite OUT] [--polyglot OUT] [--weight games|score|marks]
//!   [--with-fen] [--with-page-tails]` walks the book from the initial position
//!   and writes what it finds.
//!   - **SQLite** (needs the `sqlite` feature): every byte of every entry of the
//!     `.ctg`, decoded where the meaning is established and raw everywhere, plus
//!     the real positions and moves under PolyGlot keys. See `docs/book-export.md`.
//!   - **PolyGlot** (`.bin`): position, move and weight only, for engines and GUIs.
//! - `book verify-export <sqlite> <book>` rebuilds every page of the `.ctg` from
//!   the SQLite file and compares it with the original, byte for byte.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use cbvault::book::{Book, BookStats, Error as BookError, WalkedPosition};
use gigachess::{Move, Role};

use super::USAGE;

type CliResult = Result<bool, Box<dyn std::error::Error>>;

/// How a PolyGlot weight is derived from the statistics of the position a move
/// leads to. A PolyGlot book has nothing but this number, so this is a choice
/// of the export, not a fact of the `.ctg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Weight {
    /// Games with a result after the move (ChessBase's N).
    Games,
    /// Half-points scored by the side that plays the move.
    Score,
    /// Like `Games`, but a move marked "Don't play in tournament" weighs 0.
    Marks,
}

impl Weight {
    fn parse(s: &str) -> Option<Weight> {
        match s {
            "games" => Some(Weight::Games),
            "score" => Some(Weight::Score),
            "marks" => Some(Weight::Marks),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Weight::Games => "games",
            Weight::Score => "score",
            Weight::Marks => "marks",
        }
    }

    /// The unscaled weight of a move whose resulting position has `after`.
    /// `after` is from the point of view of the side to move there, which is
    /// the opponent of the side that plays the move.
    fn raw(self, after: &BookStats) -> u64 {
        match self {
            Weight::Games => u64::from(after.results()),
            Weight::Score => 2 * u64::from(after.losses) + u64::from(after.draws),
            Weight::Marks if after.dont_play() => 0,
            Weight::Marks => u64::from(after.results()),
        }
    }
}

/// The PolyGlot encoding of a move: destination square in bits 0–5, origin in
/// bits 6–11 (squares numbered `rank * 8 + file`), promotion piece in bits
/// 12–14 (1 knight … 4 queen). Castling is king → rook in both conventions.
pub(crate) fn polyglot_move(mv: Move) -> u16 {
    let promotion = match mv.promotion() {
        Some(Role::Knight) => 1,
        Some(Role::Bishop) => 2,
        Some(Role::Rook) => 3,
        Some(Role::Queen) => 4,
        _ => 0,
    };
    mv.to().index() as u16 | (mv.from().index() as u16) << 6 | promotion << 12
}

/// One PolyGlot record before sorting: key, move, weight.
type Record = (u64, u16, u16);

/// The PolyGlot records of one position. Moves that did not decode, or that
/// lead to a position the book does not have, are left out. Weights are scaled
/// down per position when the largest one does not fit in 16 bits.
fn polyglot_records(pos: &WalkedPosition, weight: Weight, out: &mut Vec<Record>) {
    let moves: Vec<(u16, u64)> = pos
        .found
        .moves
        .iter()
        .zip(&pos.moves)
        .filter_map(|(m, w)| Some((polyglot_move(m.mv.ok()?), weight.raw(w.child.as_ref()?))))
        .collect();
    let max = moves.iter().map(|m| m.1).max().unwrap_or(0);
    for (mv, raw) in moves {
        let w = if max > u64::from(u16::MAX) { raw * u64::from(u16::MAX) / max } else { raw };
        out.push((pos.key, mv, w as u16));
    }
}

/// Writes PolyGlot records sorted by key (then heavier moves first), 16
/// big-endian bytes each: key, move, weight, and a zero "learn" field.
fn write_polyglot(path: &Path, mut records: Vec<Record>) -> std::io::Result<u64> {
    records.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)).then(a.1.cmp(&b.1)));
    let mut out = BufWriter::with_capacity(1 << 20, File::create_new(path)?);
    for &(key, mv, w) in &records {
        out.write_all(&key.to_be_bytes())?;
        out.write_all(&mv.to_be_bytes())?;
        out.write_all(&w.to_be_bytes())?;
        out.write_all(&[0; 4])?;
    }
    out.flush()?;
    Ok(records.len() as u64)
}

/// An error writing `path`, in the library's error type, so it can leave the
/// walk's callback.
fn write_error(path: &Path, e: impl std::fmt::Display) -> BookError {
    BookError::Io { path: path.to_path_buf(), source: std::io::Error::other(e.to_string()) }
}

/// Export options.
#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) sqlite: Option<PathBuf>,
    pub(crate) polyglot: Option<PathBuf>,
    pub(crate) weight: Weight,
    pub(crate) with_fen: bool,
    pub(crate) with_page_tails: bool,
}

pub(crate) fn run_export(mut args: impl Iterator<Item = String>) -> CliResult {
    let mut path = None;
    let mut opts =
        Options { sqlite: None, polyglot: None, weight: Weight::Games, with_fen: false, with_page_tails: false };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sqlite" => opts.sqlite = Some(args.next().ok_or("--sqlite requires a path")?.into()),
            "--polyglot" => opts.polyglot = Some(args.next().ok_or("--polyglot requires a path")?.into()),
            "--weight" => {
                let w = args.next().ok_or("--weight requires games, score or marks")?;
                opts.weight = Weight::parse(&w).ok_or("--weight requires games, score or marks")?;
            }
            "--with-fen" => opts.with_fen = true,
            "--with-page-tails" => opts.with_page_tails = true,
            other if !other.starts_with('-') && path.is_none() => path = Some(other.to_string()),
            _ => return Err(USAGE.into()),
        }
    }
    let path = path.ok_or(USAGE)?;
    if opts.sqlite.is_none() && opts.polyglot.is_none() {
        return Err("book export needs --sqlite OUT, --polyglot OUT or both".into());
    }
    for out in opts.sqlite.iter().chain(&opts.polyglot) {
        if out.exists() {
            return Err(format!("{} already exists: export never overwrites a file", out.display()).into());
        }
    }
    #[cfg(not(feature = "sqlite"))]
    if opts.sqlite.is_some() {
        return Err(
            "this cbvault was built without the `sqlite` feature (cargo build --release --features sqlite)".into()
        );
    }

    let t0 = Instant::now();
    let book = Book::open(&path)?;
    eprintln!("indexed {} positions in {:.1} s", book.positions(), t0.elapsed().as_secs_f64());

    let mut records: Option<Vec<Record>> = opts.polyglot.as_ref().map(|_| Vec::new());
    #[cfg(feature = "sqlite")]
    let mut db = match &opts.sqlite {
        Some(p) => Some(sqlite::Writer::create(p, &book, &opts)?),
        None => None,
    };

    let t1 = Instant::now();
    let report = book.walk(|pos| {
        if let Some(r) = records.as_mut() {
            polyglot_records(pos, opts.weight, r);
        }
        #[cfg(feature = "sqlite")]
        if let (Some(db), Some(p)) = (db.as_mut(), &opts.sqlite) {
            db.position(pos).map_err(|e| write_error(p, e))?;
        }
        Ok(())
    })?;
    eprintln!(
        "walked {} positions in {} plies in {:.1} s ({} moves, {} undecoded, {} lead out of the book, {} collisions)",
        report.positions,
        report.plies,
        t1.elapsed().as_secs_f64(),
        report.moves_decoded,
        report.moves_undecoded,
        report.children_missing,
        report.collisions
    );

    #[cfg_attr(not(feature = "sqlite"), allow(unused_mut))]
    let mut ok = report.collisions == 0 && book.damaged().is_empty();
    if let (Some(r), Some(p)) = (records, &opts.polyglot) {
        let n = write_polyglot(p, r).map_err(|e| write_error(p, e))?;
        eprintln!("polyglot: {n} moves (weight: {}) -> {}", opts.weight.as_str(), p.display());
    }
    #[cfg(feature = "sqlite")]
    if let (Some(db), Some(p)) = (db, &opts.sqlite) {
        let summary = db.finish(&book, &report)?;
        eprintln!(
            "sqlite: {} entries ({} orphans), {} positions, {} moves, {} page tails -> {}",
            summary.entries,
            summary.orphans,
            summary.positions,
            summary.moves,
            summary.page_tails,
            p.display()
        );
        ok &= summary.entries == book.positions() as u64;
    }
    eprintln!("done in {:.1} s", t0.elapsed().as_secs_f64());
    Ok(ok)
}

#[cfg(not(feature = "sqlite"))]
pub(crate) fn run_verify_export(_args: impl Iterator<Item = String>) -> CliResult {
    Err("this cbvault was built without the `sqlite` feature (cargo build --release --features sqlite)".into())
}

#[cfg(feature = "sqlite")]
pub(crate) fn run_verify_export(args: impl Iterator<Item = String>) -> CliResult {
    let mut paths = Vec::new();
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other if !other.starts_with('-') && paths.len() < 2 => paths.push(other.to_string()),
            _ => return Err(USAGE.into()),
        }
    }
    let [db, book] = <[String; 2]>::try_from(paths).map_err(|_| USAGE)?;
    let t0 = Instant::now();
    let r = sqlite::verify(Path::new(&db), &book)?;
    let secs = t0.elapsed().as_secs_f64();
    if json {
        println!(
            concat!(
                "{{\"header_identical\":{},\"pages\":{},\"pages_identical\":{},\"pages_different\":{},",
                "\"page_tails_exported\":{},\"entries\":{},\"positions_without_entry\":{},",
                "\"moves_not_matching_entry\":{},\"children_missing\":{},\"seconds\":{:.2}}}"
            ),
            r.header_identical,
            r.pages,
            r.pages_identical,
            r.pages_different,
            r.page_tails,
            r.entries,
            r.positions_without_entry,
            r.moves_not_matching_entry,
            r.children_missing,
            secs
        );
    } else {
        let scope = if r.page_tails { "whole pages" } else { "up to the bytes-used mark (page tails not exported)" };
        println!("header page        {}", if r.header_identical { "identical" } else { "DIFFERENT" });
        println!("pages              {} identical, {} different ({scope})", r.pages_identical, r.pages_different);
        for p in &r.first_different {
            println!("  first different: page {p}");
        }
        println!("entries            {}", r.entries);
        println!("positions w/o entry {}", r.positions_without_entry);
        println!("moves ≠ entry bytes {}", r.moves_not_matching_entry);
        println!("children missing   {} (decoded moves whose position is not exported)", r.children_missing);
        println!("verified in {secs:.1} s");
    }
    Ok(r.header_identical
        && r.pages_different == 0
        && r.positions_without_entry == 0
        && r.moves_not_matching_entry == 0)
}

#[cfg(feature = "sqlite")]
mod sqlite {
    //! The SQLite export. The schema is documented in `docs/book-export.md`;
    //! keep the two in step.

    use std::collections::HashMap;
    use std::io::Read;
    use std::path::Path;

    use cbvault::book::{Book, WalkReport, WalkedPosition};
    use cbvault_format::ctg::{self, Annotation, Ctg, Evaluation, PAGE_SIZE, STATS_LEN, Stats};
    use rusqlite::{Connection, OpenFlags, params};
    use sha2::{Digest, Sha256};

    use super::Options;

    type Res<T> = Result<T, Box<dyn std::error::Error>>;

    /// Bumped whenever a table, a column or a meaning changes.
    pub(super) const SCHEMA_VERSION: i64 = 1;

    const SCHEMA: &str = "
        PRAGMA journal_mode = OFF;
        PRAGMA synchronous = OFF;
        PRAGMA locking_mode = EXCLUSIVE;
        PRAGMA cache_size = -1048576;
        PRAGMA temp_store = MEMORY;

        CREATE TABLE meta (key TEXT PRIMARY KEY, value) WITHOUT ROWID;

        CREATE TABLE entry (
            offset               INTEGER PRIMARY KEY,
            ctg_key              BLOB NOT NULL,
            moves_raw            BLOB NOT NULL,
            games                INTEGER NOT NULL,
            wins                 INTEGER NOT NULL,
            losses               INTEGER NOT NULL,
            draws                INTEGER NOT NULL,
            raw_score            INTEGER NOT NULL,
            rated_games          INTEGER NOT NULL,
            rating_sum           INTEGER NOT NULL,
            opponent_rated_games INTEGER NOT NULL,
            opponent_rating_sum  INTEGER NOT NULL,
            marks                INTEGER NOT NULL,
            unknown31            INTEGER NOT NULL,
            eval_raw             INTEGER NOT NULL,
            reached              INTEGER NOT NULL
        );

        CREATE TABLE position (
            key       INTEGER PRIMARY KEY,
            entry     INTEGER NOT NULL,
            ply       INTEGER NOT NULL,
            flipped   INTEGER NOT NULL,
            mirrored  INTEGER NOT NULL,
            eval_code INTEGER,
            fen       TEXT
        );

        CREATE TABLE move (
            position_key  INTEGER NOT NULL,
            ord           INTEGER NOT NULL,
            polyglot_move INTEGER,
            move_byte     INTEGER NOT NULL,
            annotation    INTEGER NOT NULL,
            child_key     INTEGER,
            PRIMARY KEY (position_key, ord)
        ) WITHOUT ROWID;

        CREATE TABLE page_tail (page INTEGER PRIMARY KEY, tail BLOB NOT NULL);
        CREATE TABLE damaged_page (page INTEGER PRIMARY KEY, detail TEXT NOT NULL, raw BLOB NOT NULL);

        CREATE TABLE symbol_rule (code INTEGER PRIMARY KEY, symbol TEXT NOT NULL);
        CREATE TABLE evaluation_rule (code INTEGER PRIMARY KEY, symbol TEXT NOT NULL);
        CREATE TABLE colour_rule (
            marks INTEGER NOT NULL, code INTEGER NOT NULL, colour TEXT NOT NULL,
            PRIMARY KEY (marks, code)
        ) WITHOUT ROWID;
    ";

    const VIEWS: &str = "
        CREATE VIEW move_uci AS
        SELECT position_key, ord,
               CASE WHEN polyglot_move IS NOT NULL THEN
                   char(97 + ((polyglot_move >> 6) & 7), 49 + ((polyglot_move >> 9) & 7),
                        97 + (polyglot_move & 7), 49 + ((polyglot_move >> 3) & 7))
                   || CASE (polyglot_move >> 12) & 7
                          WHEN 1 THEN 'n' WHEN 2 THEN 'b' WHEN 3 THEN 'r' WHEN 4 THEN 'q' ELSE '' END
               END AS uci
        FROM move;

        CREATE VIEW book_positions AS
        SELECT p.key, p.ply, p.fen, p.entry, p.eval_code, ev.symbol AS evaluation,
               e.games, e.wins + e.draws + e.losses AS n, e.wins, e.draws, e.losses,
               CASE WHEN e.wins + e.draws + e.losses > 0
                    THEN (e.wins + e.draws * 0.5) * 100.0 / (e.wins + e.draws + e.losses) END AS score,
               CASE WHEN e.wins + e.draws + e.losses > 0
                    THEN e.draws * 100.0 / (e.wins + e.draws + e.losses) END AS draw_pct,
               CASE WHEN e.rated_games > 0 THEN e.rating_sum / e.rated_games END AS av,
               CASE WHEN e.opponent_rated_games > 0
                    THEN e.opponent_rating_sum / e.opponent_rated_games END AS opponent_av
        FROM position p
        JOIN entry e ON e.offset = p.entry
        LEFT JOIN evaluation_rule ev ON ev.code = p.eval_code;

        CREATE VIEW book_moves AS
        SELECT m.position_key, m.ord,
               CASE WHEN m.polyglot_move IS NOT NULL THEN
                   char(97 + ((m.polyglot_move >> 6) & 7), 49 + ((m.polyglot_move >> 9) & 7),
                        97 + (m.polyglot_move & 7), 49 + ((m.polyglot_move >> 3) & 7))
                   || CASE (m.polyglot_move >> 12) & 7
                          WHEN 1 THEN 'n' WHEN 2 THEN 'b' WHEN 3 THEN 'r' WHEN 4 THEN 'q' ELSE '' END
               END AS uci,
               m.polyglot_move, m.move_byte, m.annotation,
               s.symbol, ev.symbol AS evaluation, c.eval_code, cr.colour,
               (e.marks & 128) != 0 AS main_move, (e.marks & 64) != 0 AS dont_play,
               e.games, e.wins + e.draws + e.losses AS n,
               CASE WHEN e.wins + e.draws + e.losses > 0
                    THEN (e.losses + e.draws * 0.5) * 100.0 / (e.wins + e.draws + e.losses) END AS score,
               CASE WHEN e.wins + e.draws + e.losses > 0
                    THEN e.draws * 100.0 / (e.wins + e.draws + e.losses) END AS draw_pct,
               CASE WHEN e.opponent_rated_games > 0
                    THEN e.opponent_rating_sum / e.opponent_rated_games END AS av,
               m.child_key, c.key IS NOT NULL AS child_in_book
        FROM move m
        LEFT JOIN position c ON c.key = m.child_key
        LEFT JOIN entry e ON e.offset = c.entry
        LEFT JOIN symbol_rule s ON s.code = (m.annotation & 31)
        LEFT JOIN colour_rule cr ON cr.marks = e.marks AND cr.code = (m.annotation & 31)
        LEFT JOIN evaluation_rule ev ON ev.code = c.eval_code;
    ";

    /// A PolyGlot key, stored as SQLite's signed 64-bit integer (two's
    /// complement: the same 64 bits).
    fn sql_key(key: u64) -> i64 {
        key as i64
    }

    /// A `position` row, held until the walk ends so rows go in in key order
    /// (in walk order, keys are random and B-tree inserts get slow).
    struct PositionRow {
        key: i64,
        entry: i64,
        ply: u16,
        flipped: bool,
        mirrored: bool,
        eval_code: Option<u8>,
    }

    /// A `move` row, held for the same reason.
    struct MoveRow {
        position_key: i64,
        ord: u8,
        polyglot_move: Option<u16>,
        move_byte: u8,
        annotation: u8,
        child_key: Option<i64>,
    }

    pub(super) struct Writer {
        conn: Connection,
        with_fen: bool,
        with_page_tails: bool,
        positions: Vec<PositionRow>,
        fens: Vec<(i64, String)>,
        moves: Vec<MoveRow>,
    }

    pub(super) struct Summary {
        pub(super) entries: u64,
        pub(super) orphans: u64,
        pub(super) positions: u64,
        pub(super) moves: u64,
        pub(super) page_tails: u64,
    }

    impl Writer {
        pub(super) fn create(path: &Path, book: &Book, opts: &Options) -> Res<Writer> {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.execute_batch(SCHEMA)?;
            conn.execute_batch("BEGIN")?;
            let w = Writer {
                conn,
                with_fen: opts.with_fen,
                with_page_tails: opts.with_page_tails,
                positions: Vec::new(),
                fens: Vec::new(),
                moves: Vec::new(),
            };
            w.rules()?;
            w.source_meta(book, opts)?;
            Ok(w)
        }

        fn meta(&self, key: &str, value: impl rusqlite::ToSql) -> Res<()> {
            self.conn.execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", params![key, value])?;
            Ok(())
        }

        /// The established rules, as data produced by the Rust code that
        /// implements them, so the views and `book probe` cannot drift apart.
        fn rules(&self) -> Res<()> {
            for code in 0..32u8 {
                if let Some(s) = Annotation(code).symbol() {
                    self.conn.execute("INSERT INTO symbol_rule VALUES (?1, ?2)", params![code, s])?;
                }
            }
            for code in 0..=255u8 {
                if let Some(s) = Evaluation(code).symbol() {
                    self.conn.execute("INSERT INTO evaluation_rule VALUES (?1, ?2)", params![code, s])?;
                }
            }
            for marks in 0..=255u8 {
                for code in 0..32u8 {
                    let after = Stats { raw_tail: [marks, 0, 0], ..Stats::default() };
                    if let Some(c) = ctg::colour(Annotation(code), &after) {
                        self.conn
                            .execute("INSERT INTO colour_rule VALUES (?1, ?2, ?3)", params![marks, code, c.as_str()])?;
                    }
                }
            }
            Ok(())
        }

        fn source_meta(&self, book: &Book, opts: &Options) -> Res<()> {
            self.meta("schema_version", SCHEMA_VERSION)?;
            self.meta("generator", concat!("cbvault ", env!("CARGO_PKG_VERSION")))?;
            self.meta("key", "PolyGlot Zobrist hash, stored as a signed 64-bit integer (same 64 bits)")?;
            self.meta("with_fen", opts.with_fen)?;
            self.meta("with_page_tails", opts.with_page_tails)?;
            let exported = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            self.meta("exported_at_unix", exported)?;
            let ctg_path = book.path();
            if let Some(name) = ctg_path.file_name() {
                self.meta("source_name", name.to_string_lossy().as_ref())?;
            }
            for ext in ["ctg", "cto", "ctb"] {
                if let Some((size, sha)) = file_digest(&ctg_path.with_extension(ext)) {
                    self.meta(&format!("{ext}_bytes"), size as i64)?;
                    self.meta(&format!("{ext}_sha256"), sha)?;
                }
            }
            let mut header = [0u8; PAGE_SIZE];
            book.ctg().read_header(&mut header)?;
            self.meta("header_page", &header[..])?;
            self.meta("pages", i64::from(book.pages()))?;
            Ok(())
        }

        pub(super) fn position(&mut self, pos: &WalkedPosition) -> rusqlite::Result<()> {
            let key = sql_key(pos.key);
            if self.with_fen {
                let fen = pos.board.to_fen();
                self.fens.push((key, fen.split(' ').take(4).collect::<Vec<_>>().join(" ")));
            }
            self.positions.push(PositionRow {
                key,
                entry: pos.found.offset as i64,
                ply: u16::try_from(pos.ply).unwrap_or(u16::MAX),
                flipped: pos.flipped,
                mirrored: pos.mirrored,
                eval_code: pos.found.evaluation.map(|e| e.0),
            });
            for (ord, (m, w)) in pos.found.moves.iter().zip(&pos.moves).enumerate() {
                self.moves.push(MoveRow {
                    position_key: key,
                    ord: ord as u8,
                    polyglot_move: m.mv.ok().map(super::polyglot_move),
                    move_byte: m.byte,
                    annotation: m.annotation,
                    child_key: w.child_key.map(sql_key),
                });
            }
            Ok(())
        }

        /// Inserts the held positions and moves, sorted by key.
        fn flush_rows(&mut self) -> Res<()> {
            self.positions.sort_unstable_by_key(|p| p.key);
            self.fens.sort_unstable_by_key(|f| f.0);
            self.moves.sort_unstable_by_key(|m| (m.position_key, m.ord));
            let mut fens = self.fens.iter().peekable();
            let mut insert_pos = self.conn.prepare(
                "INSERT INTO position(key, entry, ply, flipped, mirrored, eval_code, fen) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            )?;
            for p in &self.positions {
                let fen = fens.next_if(|f| f.0 == p.key).map(|f| f.1.as_str());
                insert_pos.execute(params![p.key, p.entry, p.ply, p.flipped, p.mirrored, p.eval_code, fen])?;
            }
            let mut insert_move = self.conn.prepare(
                "INSERT INTO move(position_key, ord, polyglot_move, move_byte, annotation, child_key) \
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )?;
            for m in &self.moves {
                insert_move.execute(params![
                    m.position_key,
                    m.ord,
                    m.polyglot_move,
                    m.move_byte,
                    m.annotation,
                    m.child_key
                ])?;
            }
            Ok(())
        }

        /// Writes every entry of the `.ctg` (and page tails, damaged pages),
        /// then the indexes, the views and the counts, and commits.
        pub(super) fn finish(mut self, book: &Book, report: &WalkReport) -> Res<Summary> {
            self.flush_rows()?;
            let positions = self.positions.len() as u64;
            let moves = self.moves.len() as u64;
            self.positions = Vec::new();
            self.moves = Vec::new();
            self.fens = Vec::new();
            let ctg = book.ctg();
            let mut entries = 0u64;
            let mut orphans = 0u64;
            let mut tails = 0u64;
            {
                let mut insert = self
                    .conn
                    .prepare("INSERT INTO entry VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)")?;
                let mut insert_tail = self.conn.prepare("INSERT INTO page_tail VALUES (?1, ?2)")?;
                let mut insert_damaged = self.conn.prepare("INSERT INTO damaged_page VALUES (?1, ?2, ?3)")?;
                let damaged: HashMap<u32, &str> = book.damaged().iter().map(|d| (d.page, d.detail.as_str())).collect();
                let mut reached = report.reached.iter().peekable();
                let mut buf = [0u8; PAGE_SIZE];
                for page in 1..ctg.pages() {
                    let bytes = match ctg.mapped_page(page) {
                        Some(b) => b,
                        None => {
                            ctg.read_page(page, &mut buf)?;
                            &buf[..]
                        }
                    };
                    if let Some(detail) = damaged.get(&page) {
                        insert_damaged.execute(params![page, detail, bytes])?;
                    }
                    let Ok(walk) = ctg.entries(page, bytes) else { continue };
                    for entry in walk {
                        let Ok(entry) = entry else { break };
                        while reached.next_if(|&&o| o < entry.offset).is_some() {}
                        let is_reached = reached.next_if(|&&o| o == entry.offset).is_some();
                        if !is_reached {
                            orphans += 1;
                        }
                        let s = entry.stats();
                        insert.execute(params![
                            entry.offset as i64,
                            entry.key,
                            entry.moves,
                            s.games,
                            s.wins,
                            s.losses,
                            s.draws,
                            s.raw_score,
                            s.rated_games,
                            s.rating_sum,
                            s.opponent_rated_games,
                            s.opponent_rating_sum,
                            s.raw_tail[0],
                            s.raw_tail[1],
                            s.raw_tail[2],
                            is_reached
                        ])?;
                        entries += 1;
                    }
                    if self.with_page_tails
                        && let Some(used) = ctg::page_used(bytes)
                        && bytes[used..].iter().any(|&b| b != 0)
                    {
                        insert_tail.execute(params![page, &bytes[used..]])?;
                        tails += 1;
                    }
                }
            }
            self.conn.execute_batch(VIEWS)?;
            self.meta("entries", entries as i64)?;
            self.meta("orphan_entries", orphans as i64)?;
            self.meta("positions", positions as i64)?;
            self.meta("moves", moves as i64)?;
            self.meta("plies", i64::from(report.plies))?;
            self.meta("moves_undecoded", report.moves_undecoded as i64)?;
            self.meta("moves_out_of_book", report.children_missing as i64)?;
            self.meta("zobrist_collisions", report.collisions as i64)?;
            self.meta("damaged_pages", book.damaged().len() as i64)?;
            self.meta("page_tails", tails as i64)?;
            self.conn.execute_batch("COMMIT")?;
            Ok(Summary { entries, orphans, positions, moves, page_tails: tails })
        }
    }

    /// Size and SHA-256 of a file, when it can be read.
    fn file_digest(path: &Path) -> Option<(u64, String)> {
        let mut f = std::fs::File::open(path).ok()?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let n = f.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        let hex = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        Some((size, hex))
    }

    /// What [`verify`] found.
    #[derive(Debug, Default)]
    pub(super) struct VerifyExport {
        pub(super) header_identical: bool,
        pub(super) pages: u64,
        pub(super) pages_identical: u64,
        pub(super) pages_different: u64,
        pub(super) first_different: Vec<u32>,
        pub(super) page_tails: bool,
        pub(super) entries: u64,
        pub(super) positions_without_entry: u64,
        pub(super) moves_not_matching_entry: u64,
        pub(super) children_missing: u64,
    }

    /// One `entry` row, back as bytes.
    struct Row {
        offset: u64,
        key: Vec<u8>,
        moves: Vec<u8>,
        stats: [u8; STATS_LEN],
    }

    fn stats_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<Stats> {
        Ok(Stats {
            games: r.get(3)?,
            wins: r.get(4)?,
            losses: r.get(5)?,
            draws: r.get(6)?,
            raw_score: r.get(7)?,
            rated_games: r.get(8)?,
            rating_sum: r.get(9)?,
            opponent_rated_games: r.get(10)?,
            opponent_rating_sum: r.get(11)?,
            raw_tail: [r.get(12)?, r.get(13)?, r.get(14)?],
        })
    }

    /// Rebuilds every page of the `.ctg` from the export and compares it with
    /// the original; then checks that positions and moves agree with entries.
    pub(super) fn verify(db: &Path, book_path: &str) -> Res<VerifyExport> {
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        // The checks join positions and moves to entries in key order, which is
        // random order for the other table: a large cache and a memory map keep
        // those lookups off the disk.
        conn.execute_batch("PRAGMA cache_size = -1048576; PRAGMA mmap_size = 17179869184;")?;
        let ctg = Ctg::open(book_path)?;
        let mut r = VerifyExport::default();

        let get_meta = |k: &str| -> Res<Option<rusqlite::types::Value>> {
            let v = conn.query_row("SELECT value FROM meta WHERE key = ?1", [k], |row| row.get(0));
            match v {
                Ok(v) => Ok(Some(v)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        let header: Option<Vec<u8>> = match get_meta("header_page")? {
            Some(rusqlite::types::Value::Blob(b)) => Some(b),
            _ => None,
        };
        let mut original = [0u8; PAGE_SIZE];
        ctg.read_header(&mut original)?;
        r.header_identical = header.as_deref() == Some(&original[..]);
        r.page_tails = matches!(get_meta("with_page_tails")?, Some(rusqlite::types::Value::Integer(1)));

        let tails: HashMap<u32, Vec<u8>> = conn
            .prepare("SELECT page, tail FROM page_tail")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let damaged: HashMap<u32, Vec<u8>> = conn
            .prepare("SELECT page, raw FROM damaged_page")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let mut stmt = conn.prepare("SELECT * FROM entry ORDER BY offset")?;
        let mut rows = stmt
            .query_map([], |row| {
                Ok(Row {
                    offset: row.get::<_, i64>(0)? as u64,
                    key: row.get(1)?,
                    moves: row.get(2)?,
                    stats: stats_of(row)?.to_bytes(),
                })
            })?
            .peekable();
        let mut page_buf = [0u8; PAGE_SIZE];
        for page in 1..ctg.pages() {
            r.pages += 1;
            ctg.read_page(page, &mut page_buf)?;
            let base = u64::from(page) * PAGE_SIZE as u64;
            let mut rebuilt = [0u8; PAGE_SIZE];
            let mut at = 4usize;
            let mut count = 0u16;
            let mut fits = true;
            while let Some(Ok(row)) = rows.peek() {
                if row.offset >= base + PAGE_SIZE as u64 {
                    break;
                }
                let Some(Ok(row)) = rows.next() else { break };
                r.entries += 1;
                let len = row.key.len() + 1 + row.moves.len() + STATS_LEN;
                if row.offset != base + at as u64 || at + len > PAGE_SIZE || row.moves.len() > 254 {
                    fits = false;
                    continue;
                }
                rebuilt[at..at + row.key.len()].copy_from_slice(&row.key);
                at += row.key.len();
                rebuilt[at] = (row.moves.len() + 1) as u8;
                at += 1;
                rebuilt[at..at + row.moves.len()].copy_from_slice(&row.moves);
                at += row.moves.len();
                rebuilt[at..at + STATS_LEN].copy_from_slice(&row.stats);
                at += STATS_LEN;
                count += 1;
            }
            if let Some(Err(_)) = rows.peek() {
                rows.next().transpose()?;
            }
            rebuilt[0..2].copy_from_slice(&count.to_be_bytes());
            rebuilt[2..4].copy_from_slice(&(at as u16).to_be_bytes());
            let identical = if let Some(raw) = damaged.get(&page) {
                raw[..] == page_buf[..]
            } else if r.page_tails {
                if let Some(t) = tails.get(&page) {
                    let end = (at + t.len()).min(PAGE_SIZE);
                    rebuilt[at..end].copy_from_slice(&t[..end - at]);
                }
                fits && rebuilt == page_buf
            } else {
                fits && rebuilt[..at] == page_buf[..at]
            };
            if identical {
                r.pages_identical += 1;
            } else {
                r.pages_different += 1;
                if r.first_different.len() < 5 {
                    r.first_different.push(page);
                }
            }
        }
        drop(rows);
        drop(stmt);

        r.positions_without_entry = conn.query_row(
            "SELECT count(*) FROM position p LEFT JOIN entry e ON e.offset = p.entry WHERE e.offset IS NULL",
            [],
            |row| row.get::<_, i64>(0).map(|n| n as u64),
        )?;
        r.children_missing = conn.query_row(
            "SELECT count(*) FROM move m LEFT JOIN position c ON c.key = m.child_key \
             WHERE m.child_key IS NOT NULL AND c.key IS NULL",
            [],
            |row| row.get::<_, i64>(0).map(|n| n as u64),
        )?;

        // Every position's move rows must be its entry's move bytes, in order.
        let mut positions = conn
            .prepare("SELECT p.key, e.moves_raw FROM position p JOIN entry e ON e.offset = p.entry ORDER BY p.key")?;
        let mut moves =
            conn.prepare("SELECT position_key, move_byte, annotation FROM move ORDER BY position_key, ord")?;
        let mut move_rows = moves
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, u8>(1)?, row.get::<_, u8>(2)?)))?
            .peekable();
        let mut pos_rows = positions.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))?;
        for pos in pos_rows.by_ref() {
            let (key, raw) = pos?;
            let mut got = Vec::with_capacity(raw.len());
            while let Some(Ok((k, b, a))) = move_rows.peek() {
                if *k != key {
                    break;
                }
                got.push(*b);
                got.push(*a);
                move_rows.next();
            }
            if got != raw {
                r.moves_not_matching_entry += 1;
            }
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polyglot_moves_use_the_polyglot_square_order() {
        let board = gigachess::Board::startpos();
        let e4 = board.legal_moves().into_iter().find(|m| m.to_string() == "e2e4").unwrap();
        // e2 = 12, e4 = 28: to | from << 6.
        assert_eq!(polyglot_move(e4), 28 | 12 << 6);
        let castle = gigachess::fen::parse_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1").unwrap();
        let oo = castle.legal_moves().into_iter().find(|m| m.to_string() == "e1h1").unwrap();
        assert_eq!(polyglot_move(oo), 7 | 4 << 6, "PolyGlot castles king to rook");
    }

    /// Writes a two-entry book (the initial position, with 1.e4 marked "!" and
    /// "Main move", and the position after it, with an evaluation) to `dir`,
    /// with stale bytes after the page's bytes-used mark. Synthetic: no real
    /// book content.
    #[cfg(feature = "sqlite")]
    fn synthetic_book(dir: &Path) -> PathBuf {
        use cbvault_chess::ctg::{decode_byte, keys, stored_form};
        use cbvault_format::ctg::{PAGE_SIZE, STATS_LEN};

        let start = gigachess::Board::startpos();
        let stored = stored_form(&start);
        let (e4_byte, e4) = (0u8..=255)
            .find_map(|b| decode_byte(&start, &stored, b).ok().filter(|m| m.to_string() == "e2e4").map(|m| (b, m)))
            .unwrap();
        let mut after = start;
        after.play(e4).unwrap();
        let entry = |board: &gigachess::Board, moves: &[u8], stats: BookStats| {
            let (k, l) = keys(&stored_form(board))[0];
            let mut e = k[..l].to_vec();
            e.push(moves.len() as u8 + 1);
            e.extend_from_slice(moves);
            e.extend_from_slice(&stats.to_bytes());
            e
        };
        let root = entry(
            &start,
            &[e4_byte, 0x01],
            BookStats { games: 12, wins: 5, draws: 4, losses: 3, ..Default::default() },
        );
        let child_stats = BookStats {
            games: 12,
            wins: 3,
            draws: 4,
            losses: 5,
            opponent_rated_games: 2,
            opponent_rating_sum: 4801,
            raw_tail: [cbvault_format::ctg::MAIN_MOVE, 0, 16],
            ..Default::default()
        };
        let child = entry(&after, &[], child_stats);
        let mut file = vec![0u8; 2 * PAGE_SIZE];
        file[..9].copy_from_slice(b"synthetic");
        let page = &mut file[PAGE_SIZE..];
        let used = 4 + root.len() + child.len();
        page[0..2].copy_from_slice(&2u16.to_be_bytes());
        page[2..4].copy_from_slice(&(used as u16).to_be_bytes());
        page[4..4 + root.len()].copy_from_slice(&root);
        page[4 + root.len()..used].copy_from_slice(&child);
        page[used..used + 7].copy_from_slice(&[0xab; 7]);
        assert!(used + 7 + STATS_LEN < PAGE_SIZE);
        let path = dir.join("synthetic.ctg");
        std::fs::write(&path, file).unwrap();
        path
    }

    #[cfg(feature = "sqlite")]
    fn args(a: &[&Path]) -> impl Iterator<Item = String> {
        a.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().into_iter()
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn a_synthetic_book_exports_and_rebuilds_byte_for_byte() {
        let dir = std::env::temp_dir().join(format!("cbvault-export-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = synthetic_book(&dir);
        let (db, bin) = (dir.join("out.sqlite"), dir.join("out.bin"));
        let flags = [
            Path::new("--sqlite"),
            db.as_path(),
            Path::new("--polyglot"),
            bin.as_path(),
            Path::new("--with-page-tails"),
        ];
        let mut a: Vec<&Path> = vec![&book];
        a.extend(flags);
        assert!(run_export(args(&a)).unwrap());
        assert!(run_verify_export(args(&[&db, &book])).unwrap());

        // The PolyGlot file: one record, keyed by the well-known PolyGlot key
        // of the initial position.
        let pg = std::fs::read(&bin).unwrap();
        assert_eq!(pg.len(), 16);
        assert_eq!(u64::from_be_bytes(pg[..8].try_into().unwrap()), 0x463b_9618_1691_fc9c);
        assert_eq!(u16::from_be_bytes([pg[8], pg[9]]), 28 | 12 << 6);
        assert_eq!(u16::from_be_bytes([pg[10], pg[11]]), 12);

        // The view gives what `book probe` shows.
        let conn = rusqlite::Connection::open(&db).unwrap();
        let (uci, symbol, colour, n, score, av, evaluation): (String, String, String, i64, f64, i64, String) = conn
            .query_row(
                "SELECT uci, symbol, colour, n, score, av, evaluation FROM book_moves WHERE position_key = ?1",
                [0x463b_9618_1691_fc9c_u64 as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .unwrap();
        let oriented = cbvault_format::ctg::Evaluation(16).for_real_position(true).symbol().unwrap();
        assert_eq!((uci.as_str(), symbol.as_str(), colour.as_str(), n), ("e2e4", "!", "green", 12));
        assert!((score - (5.0 + 2.0) * 100.0 / 12.0).abs() < 1e-9);
        assert_eq!((av, evaluation.as_str()), (2400, oriented));

        // A changed byte is caught.
        conn.execute("UPDATE entry SET unknown31 = 1 WHERE offset = 4100", []).unwrap();
        drop(conn);
        assert!(!run_verify_export(args(&[&db, &book])).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn weights_follow_the_chosen_policy() {
        let after = BookStats { games: 12, wins: 3, draws: 4, losses: 5, ..BookStats::default() };
        assert_eq!(Weight::Games.raw(&after), 12);
        // The side that played the move is the child's opponent: 5 wins, 4 draws.
        assert_eq!(Weight::Score.raw(&after), 14);
        let red = BookStats { raw_tail: [cbvault_format::ctg::DONT_PLAY, 0, 0], ..after };
        assert_eq!(Weight::Marks.raw(&red), 0);
        assert_eq!(Weight::Marks.raw(&after), 12);
    }
}

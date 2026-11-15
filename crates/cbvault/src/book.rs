//! Read-only ChessBase opening books (`.ctg`): look a position up, read its book
//! moves and statistics, and check a whole book.
//!
//! # How lookups work
//!
//! A book's own index (`.cto`) is a hash table whose hash function is **not
//! established** (see `docs/research/ctg-observations-2026-11-14.md`), so it is
//! not used. [`Book::open`] scans the `.ctg` once and builds an index of its own
//! in memory: one `u64` per position, packing high bits of a hash of the
//! position's key with the number of the page that holds it. A lookup binary-
//! searches that array and then compares the key byte for byte on the page, so
//! a hash collision costs a page read, never a wrong answer.
//!
//! The index costs 8 bytes per position (about 220 MB for a 27.6-million-
//! position book) and one sequential pass over the `.ctg` to build.
//!
//! # Provenance
//!
//! Original cbvault code, clean-room. It consumes `cbvault-format` for bytes and
//! `cbvault-chess` for chess.

use std::collections::HashMap;
use std::path::Path;

use cbvault_chess::ctg::{self as chess_ctg, MoveError, Stored};
use cbvault_format::ctg::position;
use cbvault_format::ctg::{Ctg, Entry, PAGE_SIZE, Stats};
use gigachess::{Board, Move};
use rayon::prelude::*;

pub use cbvault_format::ctg::{Annotation, Colour, Evaluation, Stats as BookStats, colour};
pub use cbvault_format::error::{Error, Result};

/// Pages handed to one worker at a time while the index is built.
const PAGES_PER_TASK: u32 = 2048;

/// An opened book with its in-memory index.
#[derive(Debug)]
pub struct Book {
    ctg: Ctg,
    /// Sorted: `(hash & !page_mask) | page`.
    index: Vec<u64>,
    page_mask: u64,
    damaged: Vec<Damage>,
}

/// A page the index build could not read completely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Damage {
    /// The data page.
    pub page: u32,
    /// What was wrong.
    pub detail: String,
}

/// A position found in the book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookPosition {
    /// Its statistics, from the point of view of the side to move.
    pub stats: Stats,
    /// Its book moves, in stored order.
    pub moves: Vec<BookMove>,
    /// The evaluation set on the move that leads here, oriented for the probed
    /// position (`None` when there is none).
    pub evaluation: Option<Evaluation>,
    /// The entry's byte offset in the `.ctg`.
    pub offset: u64,
}

/// One move of a book position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookMove {
    /// The stored move byte.
    pub byte: u8,
    /// The stored annotation byte: see [`Annotation`] for what is established.
    pub annotation: u8,
    /// The legal move it stands for, or why it could not be decoded.
    pub mv: std::result::Result<Move, MoveError>,
}

impl BookMove {
    /// The annotation byte, split into its parts.
    pub fn annotation(&self) -> Annotation {
        Annotation(self.annotation)
    }
}

/// A hash of a key, to spread keys over the index. FNV-1a with a SplitMix64
/// finaliser: not a format fact, only this index's own choice.
fn key_hash(key: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in key {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

impl Book {
    /// Opens a book (the `.ctg` or its base name) and builds its index.
    ///
    /// A malformed page does not fail the open: its readable entries are
    /// indexed and the page is listed in [`Book::damaged`].
    pub fn open(path: impl AsRef<Path>) -> Result<Book> {
        let ctg = Ctg::open(path)?;
        let pages = ctg.pages();
        let page_bits = 32 - pages.leading_zeros();
        let page_mask = (1u64 << page_bits) - 1;
        let tasks: Vec<(u32, u32)> =
            (1..pages).step_by(PAGES_PER_TASK as usize).map(|s| (s, (s + PAGES_PER_TASK).min(pages))).collect();
        let parts: Vec<(Vec<u64>, Vec<Damage>)> =
            tasks.par_iter().map(|&(start, end)| index_pages(&ctg, start, end, page_mask)).collect();
        let total = parts.iter().map(|(v, _)| v.len()).sum();
        let mut index = Vec::with_capacity(total);
        let mut damaged = Vec::new();
        for (v, d) in parts {
            index.extend_from_slice(&v);
            damaged.extend(d);
        }
        index.par_sort_unstable();
        Ok(Book { ctg, index, page_mask, damaged })
    }

    /// The `.ctg` path.
    pub fn path(&self) -> &Path {
        self.ctg.path()
    }

    /// Positions indexed.
    pub fn positions(&self) -> usize {
        self.index.len()
    }

    /// Pages in the `.ctg`, the header page included.
    pub fn pages(&self) -> u32 {
        self.ctg.pages()
    }

    /// Pages the index build could not read completely.
    pub fn damaged(&self) -> &[Damage] {
        &self.damaged
    }

    /// Bytes the index occupies.
    pub fn index_bytes(&self) -> usize {
        self.index.capacity() * std::mem::size_of::<u64>()
    }

    /// The `.ctg` file itself, for callers that read it page by page.
    pub fn ctg(&self) -> &Ctg {
        &self.ctg
    }

    /// Looks `board` up. `Ok(None)` when the book does not have the position.
    pub fn probe(&self, board: &Board) -> Result<Option<BookPosition>> {
        self.probe_stored(board, &chess_ctg::stored_form(board))
    }

    fn probe_stored(&self, board: &Board, stored: &Stored) -> Result<Option<BookPosition>> {
        for (key, len) in chess_ctg::keys(stored) {
            if let Some(found) = self.find(&key[..len], |entry| book_position(board, stored, entry))? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// The statistics of `board`'s entry, without decoding its moves.
    fn stats_of(&self, board: &Board) -> Result<Option<Stats>> {
        let stored = chess_ctg::stored_form(board);
        for (key, len) in chess_ctg::keys(&stored) {
            if let Some(stats) = self.find(&key[..len], |e| e.stats())? {
                return Ok(Some(stats));
            }
        }
        Ok(None)
    }

    /// Walks the book from the initial position, breadth first, and hands every
    /// real position it reaches to `emit`, once, level by level (`ply` is the
    /// shortest distance from the initial position).
    ///
    /// A book stores positions with White to move, so an entry alone does not
    /// say which real position it stands for. The walk gives every position its
    /// real side to move, and every move its real squares. Entries the walk
    /// never reaches are counted in [`WalkReport::reached`]'s complement.
    ///
    /// Positions are told apart by their PolyGlot Zobrist key. Two different
    /// positions with the same key would be merged, so a second, independent
    /// check of the board is kept for every key, and a mismatch is counted in
    /// [`WalkReport::collisions`] rather than passed over.
    pub fn walk(&self, mut emit: impl FnMut(&WalkedPosition) -> Result<()>) -> Result<WalkReport> {
        let mut report = WalkReport::default();
        let start = Board::startpos();
        let mut seen: HashMap<u64, u32> = HashMap::new();
        seen.insert(start.zobrist(), board_check(&start));
        if self.stats_of(&start)?.is_none() {
            return Ok(report);
        }
        let mut frontier = vec![start];
        let mut ply = 0u32;
        while !frontier.is_empty() {
            let mut next = Vec::new();
            for chunk in frontier.chunks(WALK_CHUNK) {
                let walked: Vec<Result<Option<WalkedPosition>>> =
                    chunk.par_iter().map(|board| self.walk_one(board, ply)).collect();
                for item in walked {
                    let Some(pos) = item? else { continue };
                    emit(&pos)?;
                    report.positions += 1;
                    report.reached.push(pos.found.offset);
                    for (m, kid) in pos.found.moves.iter().zip(&pos.moves) {
                        let Ok(mv) = m.mv else {
                            report.moves_undecoded += 1;
                            continue;
                        };
                        report.moves_decoded += 1;
                        if kid.child.is_none() {
                            report.children_missing += 1;
                            continue;
                        }
                        let mut child = pos.board;
                        if child.play(mv).is_err() {
                            continue;
                        }
                        let check = board_check(&child);
                        match seen.entry(child.zobrist()) {
                            std::collections::hash_map::Entry::Occupied(o) => {
                                if *o.get() != check {
                                    report.collisions += 1;
                                }
                            }
                            std::collections::hash_map::Entry::Vacant(v) => {
                                v.insert(check);
                                next.push(child);
                            }
                        }
                    }
                }
            }
            frontier = next;
            ply += 1;
        }
        report.plies = ply;
        report.reached.par_sort_unstable();
        report.reached.dedup();
        Ok(report)
    }

    fn walk_one(&self, board: &Board, ply: u32) -> Result<Option<WalkedPosition>> {
        let stored = chess_ctg::stored_form(board);
        let Some(found) = self.probe_stored(board, &stored)? else {
            return Ok(None);
        };
        let mut moves = Vec::with_capacity(found.moves.len());
        for m in &found.moves {
            let walked = match m.mv {
                Ok(mv) => {
                    let mut child = *board;
                    if child.play(mv).is_ok() {
                        WalkedMove { child_key: Some(child.zobrist()), child: self.stats_of(&child)? }
                    } else {
                        WalkedMove { child_key: None, child: None }
                    }
                }
                Err(_) => WalkedMove { child_key: None, child: None },
            };
            moves.push(walked);
        }
        Ok(Some(WalkedPosition {
            board: *board,
            key: board.zobrist(),
            ply,
            flipped: stored.flipped,
            mirrored: stored.mirrored,
            found,
            moves,
        }))
    }

    /// Looks up the position after `mv` in `board`. `Ok(None)` when the move is
    /// illegal or the book does not have the position.
    pub fn probe_after(&self, board: &Board, mv: Move) -> Result<Option<BookPosition>> {
        let mut next = *board;
        if next.play(mv).is_err() {
            return Ok(None);
        }
        self.probe(&next)
    }

    /// Whether the book has an entry with exactly `key`.
    pub fn contains_key(&self, key: &[u8]) -> Result<bool> {
        Ok(self.find(key, |_| ())?.is_some())
    }

    /// Finds the entry whose key is `key` and maps it with `f`.
    fn find<T>(&self, key: &[u8], f: impl Fn(&Entry<'_>) -> T) -> Result<Option<T>> {
        let lo = key_hash(key) & !self.page_mask;
        let hi = lo | self.page_mask;
        let start = self.index.partition_point(|&x| x < lo);
        let mut buf = [0u8; PAGE_SIZE];
        let mut last_page = None;
        for &x in self.index[start..].iter().take_while(|&&x| x <= hi) {
            let page = (x & self.page_mask) as u32;
            if last_page == Some(page) {
                continue;
            }
            last_page = Some(page);
            let bytes = match self.ctg.mapped_page(page) {
                Some(b) => b,
                None => {
                    self.ctg.read_page(page, &mut buf)?;
                    &buf[..]
                }
            };
            for entry in self.ctg.entries(page, bytes)? {
                let entry = entry.map_err(|e| e.into_error(self.ctg.path()))?;
                if entry.key == key {
                    return Ok(Some(f(&entry)));
                }
            }
        }
        Ok(None)
    }

    /// Checks every entry of the book: that its key decodes to a valid position
    /// and encodes back to the same bytes, that its move bytes decode to legal
    /// moves, and (with `children`) that the position after each move is in the
    /// book too. Read-only, like everything here.
    pub fn verify(&self, children: bool) -> Result<VerifyReport> {
        let pages = self.ctg.pages();
        let tasks: Vec<(u32, u32)> =
            (1..pages).step_by(PAGES_PER_TASK as usize).map(|s| (s, (s + PAGES_PER_TASK).min(pages))).collect();
        let parts: Vec<Result<VerifyReport>> =
            tasks.par_iter().map(|&(start, end)| self.verify_pages(start, end, children)).collect();
        let mut report = VerifyReport::default();
        for part in parts {
            report.add(&part?);
        }
        Ok(report)
    }

    fn verify_pages(&self, start: u32, end: u32, children: bool) -> Result<VerifyReport> {
        let mut r = VerifyReport::default();
        let mut buf = [0u8; PAGE_SIZE];
        for page in start..end {
            let bytes = match self.ctg.mapped_page(page) {
                Some(b) => b,
                None => {
                    self.ctg.read_page(page, &mut buf)?;
                    &buf[..]
                }
            };
            let Ok(entries) = self.ctg.entries(page, bytes) else {
                r.damaged_pages += 1;
                continue;
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    r.damaged_pages += 1;
                    break;
                };
                r.entries += 1;
                let Ok(raw) = position::decode(entry.key) else {
                    r.bad_keys += 1;
                    continue;
                };
                let Some(board) = chess_ctg::board_of(&raw) else {
                    r.invalid_positions += 1;
                    continue;
                };
                let stored = chess_ctg::stored_form(&board);
                let round_trips = chess_ctg::keys(&stored).iter().any(|(k, l)| &k[..*l] == entry.key);
                if round_trips {
                    r.key_round_trips += 1;
                } else {
                    r.key_mismatches += 1;
                    continue;
                }
                for (byte, _) in entry.move_pairs() {
                    match chess_ctg::decode_byte(&board, &stored, byte) {
                        Ok(mv) => {
                            r.moves_decoded += 1;
                            if mv.promotion().is_some() {
                                r.promotions += 1;
                            }
                            if children {
                                let mut next = board;
                                let found = next.play(mv).is_ok() && self.probe_key_of(&next)?;
                                if found {
                                    r.children_found += 1;
                                } else {
                                    r.children_missing += 1;
                                }
                            }
                        }
                        Err(MoveError::UnknownByte(_)) => r.moves_unknown += 1,
                        Err(MoveError::NoSuchPiece | MoveError::Illegal) => r.moves_illegal += 1,
                    }
                }
            }
        }
        Ok(r)
    }

    fn probe_key_of(&self, board: &Board) -> Result<bool> {
        let stored = chess_ctg::stored_form(board);
        for (key, len) in chess_ctg::keys(&stored) {
            if self.contains_key(&key[..len])? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Indexes data pages `start..end`.
fn index_pages(ctg: &Ctg, start: u32, end: u32, page_mask: u64) -> (Vec<u64>, Vec<Damage>) {
    let mut out = Vec::with_capacity(((end - start) as usize) * 56);
    let mut damaged = Vec::new();
    let mut buf = [0u8; PAGE_SIZE];
    for page in start..end {
        let bytes = match ctg.mapped_page(page) {
            Some(b) => b,
            None => match ctg.read_page(page, &mut buf) {
                Ok(()) => &buf[..],
                Err(e) => {
                    damaged.push(Damage { page, detail: e.to_string() });
                    continue;
                }
            },
        };
        let entries = match ctg.entries(page, bytes) {
            Ok(e) => e,
            Err(e) => {
                damaged.push(Damage { page, detail: e.to_string() });
                continue;
            }
        };
        let mut walk = entries;
        for entry in walk.by_ref() {
            match entry {
                Ok(entry) => out.push((key_hash(entry.key) & !page_mask) | u64::from(page)),
                Err(e) => {
                    damaged.push(Damage { page, detail: e.detail });
                    break;
                }
            }
        }
        if damaged.last().is_none_or(|d| d.page != page) && !walk.closed() {
            damaged.push(Damage { page, detail: "entries do not end at the bytes-used mark".into() });
        }
    }
    (out, damaged)
}

fn book_position(board: &Board, stored: &Stored, entry: &Entry<'_>) -> BookPosition {
    let moves = entry
        .move_pairs()
        .map(|(byte, annotation)| BookMove { byte, annotation, mv: chess_ctg::decode_byte(board, stored, byte) })
        .collect();
    let stats = entry.stats();
    let evaluation = stats.evaluation().map(|e| e.for_real_position(stored.flipped));
    BookPosition { stats, moves, evaluation, offset: entry.offset }
}

/// Positions probed in parallel at a time during [`Book::walk`], which bounds
/// the memory a wide level takes.
const WALK_CHUNK: usize = 1 << 18;

/// A second hash of a board, independent of the Zobrist key: the pieces, the
/// side to move and the castling rights (FNV-1a). The en-passant square is left
/// out on purpose: the Zobrist key counts it only when a capture is possible.
fn board_check(board: &Board) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    let mut mix = |b: u8| {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    };
    for i in 0..64u8 {
        mix(board
            .piece_at(gigachess::Square::new(i))
            .map_or(0, |p| 1 + p.color.index() as u8 * 6 + p.role.index() as u8));
    }
    mix(board.turn().index() as u8);
    mix(board.castling_rights());
    h
}

/// A real position reached by [`Book::walk`].
#[derive(Clone, Debug)]
pub struct WalkedPosition {
    /// The position, with its real side to move.
    pub board: Board,
    /// Its PolyGlot Zobrist key (`Board::zobrist`).
    pub key: u64,
    /// Its shortest distance from the initial position, in plies.
    pub ply: u32,
    /// Whether its entry is stored colour-flipped (Black is to move).
    pub flipped: bool,
    /// Whether its entry is stored mirrored left to right.
    pub mirrored: bool,
    /// The book's entry for it, with its moves in stored order.
    pub found: BookPosition,
    /// For each of `found.moves`, in the same order: where the move leads.
    pub moves: Vec<WalkedMove>,
}

/// Where a book move leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkedMove {
    /// The PolyGlot Zobrist key of the position after the move (`None` when
    /// the move byte did not decode).
    pub child_key: Option<u64>,
    /// The statistics of that position, when the book has it. They are the
    /// move's statistics, from the point of view of the side to move there.
    pub child: Option<Stats>,
}

/// What [`Book::walk`] counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkReport {
    /// Real positions reached and emitted.
    pub positions: u64,
    /// Levels walked: the deepest position is `plies - 1` plies deep.
    pub plies: u32,
    /// Book moves decoded to a legal move.
    pub moves_decoded: u64,
    /// Book moves that did not decode (unknown or illegal byte).
    pub moves_undecoded: u64,
    /// Decoded moves whose resulting position is not in the book.
    pub children_missing: u64,
    /// Keys reached by two boards the second check tells apart.
    pub collisions: u64,
    /// The `.ctg` offsets of the entries reached, sorted, without repeats.
    /// Every other entry is an orphan: no move sequence from the initial
    /// position reaches it.
    pub reached: Vec<u64>,
}

/// What [`Book::verify`] counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// Entries read.
    pub entries: u64,
    /// Pages whose entries could not all be read.
    pub damaged_pages: u64,
    /// Keys that do not decode.
    pub bad_keys: u64,
    /// Keys that decode to something that is not a chess position.
    pub invalid_positions: u64,
    /// Keys that encode back to the same bytes.
    pub key_round_trips: u64,
    /// Keys that encode back to different bytes.
    pub key_mismatches: u64,
    /// Move bytes decoded to a legal move.
    pub moves_decoded: u64,
    /// Move bytes whose meaning is not established.
    pub moves_unknown: u64,
    /// Decoded moves that promote (always to a queen: see `cbvault_chess::ctg`).
    pub promotions: u64,
    /// Move bytes that decode to no legal move.
    pub moves_illegal: u64,
    /// Decoded moves whose resulting position is in the book.
    pub children_found: u64,
    /// Decoded moves whose resulting position is not.
    pub children_missing: u64,
}

impl VerifyReport {
    fn add(&mut self, o: &VerifyReport) {
        self.entries += o.entries;
        self.damaged_pages += o.damaged_pages;
        self.bad_keys += o.bad_keys;
        self.invalid_positions += o.invalid_positions;
        self.key_round_trips += o.key_round_trips;
        self.key_mismatches += o.key_mismatches;
        self.moves_decoded += o.moves_decoded;
        self.moves_unknown += o.moves_unknown;
        self.promotions += o.promotions;
        self.moves_illegal += o.moves_illegal;
        self.children_found += o.children_found;
        self.children_missing += o.children_missing;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_spreads_neighbouring_keys() {
        assert_ne!(key_hash(&[0x02, 0x00]), key_hash(&[0x02, 0x01]));
        assert_ne!(key_hash(&[]) >> 40, key_hash(&[0]) >> 40);
    }

    /// Runs against a real book named by `CBVAULT_TEST_CTG` (never committed).
    /// Without it the test does not run, and says so.
    #[test]
    fn a_real_book_answers_the_start_position() {
        let Ok(path) = std::env::var("CBVAULT_TEST_CTG") else {
            eprintln!("CBVAULT_TEST_CTG is unset: the real-book test DID NOT RUN (it did not pass either)");
            return;
        };
        let book = Book::open(&path).unwrap();
        assert!(book.damaged().is_empty(), "{:?}", &book.damaged()[..book.damaged().len().min(5)]);
        let board = Board::startpos();
        let root = book.probe(&board).unwrap().expect("the start position is in the book");
        assert!(root.stats.games > 0);
        for m in &root.moves {
            let mv = m.mv.unwrap_or_else(|e| panic!("root byte {:#04x}: {e}", m.byte));
            assert!(book.probe_after(&board, mv).unwrap().is_some(), "child after {mv} is missing");
        }
    }
}

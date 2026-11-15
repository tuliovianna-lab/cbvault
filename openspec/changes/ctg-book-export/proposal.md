# Lossless export of ChessBase opening books

## Why

Reading a book in place (`ctg-opening-books`) answers one position at a time,
and only through cbvault. Programs that want a book's statistics need them in
an open file they can query themselves. PolyGlot, the open book format that
engines and GUIs read, keeps only a position, a move and a 16-bit weight. That
loses most of what a ChessBase book holds: wins, draws and losses, both sides'
ratings, move symbols, marks, evaluations, and the fields whose meaning is not
established yet. This is the export half of issue #3.

## What Changes

- **`Book::walk`**: a parallel breadth-first walk from the initial position.
  It gives every reached position its real side to move and its PolyGlot
  Zobrist key (`gigachess::Board::zobrist`), and every move its real squares and
  the statistics of the position it leads to. It counts orphan entries,
  undecoded moves and Zobrist collisions.
- **`cbvault book export --sqlite`** (behind the `sqlite` feature, SQLite
  compiled in): one file that keeps every byte of every entry. The statistics
  are split into integer columns that cover all 33 bytes. The real positions
  and moves sit under PolyGlot keys. The established rules (symbols,
  evaluations, colours) are tables generated from the Rust code. Two views show
  a position's moves as ChessBase does.
- **`cbvault book export --polyglot`**: a PolyGlot `.bin`, with a documented
  weight policy (`games`, `score` or `marks`).
- **`cbvault book verify-export`**: rebuilds every page of the `.ctg` from the
  SQLite file and compares it byte for byte.
- `Stats::to_bytes`, `Ctg::read_header` and `ctg::page_used` in the format
  crate.

## Non-goals

- PGN export: a later change can read it from the SQLite file.
- Writing `.ctg` books. Read-only, like the rest of the project.

## Licensing and provenance

Original MIT code. PolyGlot's Zobrist keys and move encoding come from
`gigachess` (MIT), whose table is taken from the public PolyGlot
specification. The CLI alone gains optional dependencies: `rusqlite` (MIT),
which compiles in SQLite (public domain), and `sha2` (MIT or Apache-2.0).

## Impact

New code paths only. The library crates gain no dependency. `cbvault-cli` gains
two optional dependencies behind the `sqlite` feature.

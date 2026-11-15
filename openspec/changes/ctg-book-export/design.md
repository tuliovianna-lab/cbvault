# Design: SQLite as the lossless format, PolyGlot as a view of it

## Why SQLite

A side file next to a PolyGlot `.bin` would need its own reader, and the two
could drift apart. SQLite is one file. Python reads it with its standard
library and any language can query it. Its schema documents itself.

## Keys

Positions are keyed by their **PolyGlot Zobrist hash**, stored as SQLite's
signed 64-bit integer (the same 64 bits). A consumer finds a position with
`chess.polyglot.zobrist_hash(board)` and needs no CTG logic: no flip, no
mirror, no key codec. Moves use the PolyGlot convention for castling (king →
rook), which is also gigachess's.

## Real positions need a walk

A `.ctg` entry is a position with White to move, possibly colour-flipped and
mirrored, so the entry alone does not say which real position it stands for.
`Book::walk` goes breadth first from the initial position. Each level is probed
in parallel, in chunks of 2^18 positions to bound memory, and deduplicated by
Zobrist key. A second hash of the board (pieces, side to move, castling rights)
is kept per key, so a collision is counted rather than merged silently. Entries
the walk never reaches stay in `entry` with `reached = 0`.

## What "lossless" means

| `.ctg` content | In the export |
|---|---|
| header page | `meta.header_page`, raw |
| entry key, move bytes | `entry.ctg_key`, `entry.moves_raw`, raw |
| 33 statistics bytes | twelve integer columns covering all 33 bytes; `Stats::to_bytes` rebuilds them |
| bytes after a page's bytes-used mark | `page_tail`, with `--with-page-tails` |
| a page the reader cannot walk | `damaged_page.raw`, whole |
| `.cto`, `.ctb` | size and SHA-256 only: they are lookup aids, not book data |

The bytes after the bytes-used mark are not entries of the book. In 2,000 pages
of one book, 27,054 of the 27,286 entries parsed from them had a key that lives
on another page: they look like stale copies. They are exported only on
request. `verify-export` rebuilds every page and compares it up to the
bytes-used mark, or whole with page tails.

## Rules as data

`symbol_rule`, `evaluation_rule` and `colour_rule` are filled by calling the
Rust functions (`Annotation::symbol`, `Evaluation::symbol`, `colour`) on every
input, so the SQL views and `book probe` share one implementation.

## Writing fast

There is one transaction with `journal_mode = OFF`. Positions and moves are
held in memory during the walk and inserted sorted by key: `position` is keyed
by its PolyGlot key (the rowid) and `move` is a `WITHOUT ROWID` table keyed by
`(position_key, ord)`, so neither needs a separate index. A move is stored as
PolyGlot's 16-bit integer; the views spell it in UCI.

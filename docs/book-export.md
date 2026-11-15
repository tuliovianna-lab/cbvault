# Exporting an opening book

`cbvault book export` turns a ChessBase opening book (`.ctg`) into open files:

```text
cbvault book export <book> --sqlite OUT.sqlite [--with-fen] [--with-page-tails]
cbvault book export <book> --polyglot OUT.bin [--weight games|score|marks]
cbvault book verify-export OUT.sqlite <book>
```

Both outputs come from one walk and can be written together. The SQLite writer
needs a build with the `sqlite` feature (`cargo build --release --features
sqlite`). SQLite is compiled in, so no system library is needed. Export never
overwrites an existing file.

## SQLite: every byte, under PolyGlot keys

The SQLite file is the lossless export.

- **The truth, raw.** Every entry of the `.ctg` is a row of `entry`, with its key
  and move bytes as stored. Its 33 statistics bytes are split into twelve
  integer columns that cover all 33 bytes, fields of unknown meaning included.
  The header page is kept whole in `meta`.
- **The positions, real.** The book stores every position with White to move,
  colour-flipped or mirrored. The export walks the book from the initial
  position and gives each position it reaches its real side to move, under its
  **PolyGlot Zobrist key**: the same 64 bits that `chess.polyglot.zobrist_hash`
  (python-chess) or any PolyGlot reader computes. A consumer needs no CTG logic
  to find a position.
- **The rules, as data.** The established meanings (move symbols, evaluation
  signs, the colour ChessBase gives a move) are small tables generated from the
  Rust code that implements them. The two views read them, so the views and
  `cbvault book probe` cannot drift apart.

`cbvault book verify-export` rebuilds every page of the `.ctg` from the SQLite
file and compares it with the original. Pages are compared up to their
bytes-used mark, or whole when the export has `--with-page-tails`. In that case
every page is byte-identical, so the whole `.ctg` can be rebuilt from the
export. On a 2.16 GB book (526,837 pages, 27,592,793 entries, exported with
`--with-page-tails`) the header and all pages came back identical, with no
position lacking an entry, no move differing from its entry bytes and no
missing child, in 463 s. A 2.30 GB book (561,556 pages, 32,330,172 entries)
gave the same result in 558 s.

### Keys

SQLite integers are signed, so a key is stored as `i64` holding the same 64
bits. From Python:

```python
import sqlite3, chess, chess.polyglot

def sql_key(board):
    k = chess.polyglot.zobrist_hash(board)
    return k - (1 << 64) if k >= 1 << 63 else k

db = sqlite3.connect("book.sqlite")
board = chess.Board()
board.push_san("e4")
for row in db.execute(
    "SELECT uci, symbol, evaluation, colour, n, score, draw_pct, av FROM book_moves "
    "WHERE position_key = ? ORDER BY n DESC", (sql_key(board),)):
    print(row)
```

Moves are UCI as `gigachess` writes them: **castling is king → rook** (`e1h1`),
as in PolyGlot. python-chess spells it `e1g1`.

### Tables

| Table | One row per | Columns |
|---|---|---|
| `meta` | fact | `schema_version`, `generator`, `source_name`, the `ctg`/`cto`/`ctb` sizes and SHA-256, `header_page` (page 0, raw), `pages`, `with_fen`, `with_page_tails`, and counts: `entries`, `orphan_entries`, `positions`, `moves`, `plies`, `moves_undecoded`, `moves_out_of_book`, `zobrist_collisions`, `damaged_pages`, `page_tails` |
| `entry` | `.ctg` entry | `offset` (in the `.ctg`; page = offset / 4096), `ctg_key` and `moves_raw` (as stored; the move-section length byte is `len + 1`), `games`, `wins`, `losses`, `draws` (from the stored side to move's view), `raw_score` (offset 12, not established), `rated_games`, `rating_sum`, `opponent_rated_games`, `opponent_rating_sum`, `marks` (offset 30: `0x80` Main move, `0x40` Don't play in tournament), `unknown31` (offset 31, not established), `eval_raw` (offset 32, as stored), `reached` (whether the walk reached it) |
| `position` | real position reached | `key` (the primary key), `entry`, `ply` (shortest distance from the initial position), `flipped`, `mirrored`, `eval_code` (the evaluation oriented for the real colours, or NULL), `fen` (the first four FEN fields, only with `--with-fen`) |
| `move` | book move of a real position | primary key `(position_key, ord)`, where `ord` is the stored order; `polyglot_move` (PolyGlot's 16-bit move: to, from, promotion; NULL when the byte did not decode), `move_byte`, `annotation` (raw: low 5 bits symbol, high 3 bits not established), `child_key` (the position after the move: follow it to walk the tree) |
| `page_tail` | page with stale bytes | `page`, `tail` (the bytes after the bytes-used mark; only with `--with-page-tails`) |
| `damaged_page` | page the reader could not walk | `page`, `detail`, `raw` (the whole page) |
| `symbol_rule`, `evaluation_rule`, `colour_rule` | established rule | `code` → `symbol`; `(marks, code)` → `colour` |

### Views

- **`move_uci`**: `polyglot_move` spelled in UCI.
- **`book_moves`**: a position's moves as ChessBase shows them. Columns:
  - `uci`, `symbol`, `evaluation`, `colour`, `main_move`, `dont_play`;
  - `n` (games with a result after the move) and `games`;
  - `score` (the mover's percentage), `draw_pct`;
  - `av` (the mover's average rating, truncated);
  - `child_in_book`.

  The statistics of a move are those of the position it leads to.
- **`book_positions`**: one row per position, from its side to move's view:
  - `n`, `wins`, `draws`, `losses`;
  - `score`, `draw_pct`;
  - `av`, `opponent_av`;
  - `evaluation`.

### Notes

- **Colour twins.** A `.ctg` entry is a position with White to move, so a
  position and its colour-swapped twin (Black to move, board turned upside
  down) are one entry, with one set of statistics. ChessBase shows the same
  moves for both. Reversed openings reach many twins, for example the English
  Opening as a Sicilian with colours reversed, and every twin's subtree is in
  the book too. In the Fritz Power Book 2025:
  - 25,077,767 of 27,592,793 entries are reached as two real positions;
  - 52,670,829 positions are reached in all.

  Each real position gets its own row, so a lookup by key always finds it.
- **Orphans.** An entry the walk never reaches stays in `entry` with `reached =
  0`. Nothing is dropped.
- **Collisions.** Two different positions with the same Zobrist key would be
  merged. The walk keeps a second, independent check per key and counts any
  mismatch in `zobrist_collisions`.
- **Page tails.** In the probed books most pages hold bytes after their
  bytes-used mark. In 2,000 pages of one book, 27,054 of the 27,286 entries
  parsed from those bytes had a key that lives on another page. They look like
  stale copies, and they are not entries of the book. They are exported only
  with `--with-page-tails`.
- **En passant in `fen`.** The FEN is gigachess's, which names an en-passant
  square after every double step. The key counts it only when a capture is
  possible, as PolyGlot does.

## PolyGlot

The `.bin` has 16 bytes per move, sorted by key: key, move, weight and a zero
learn field. A PolyGlot book holds nothing but the weight, so how it is derived
is a choice of the export:

| `--weight` | Weight of a move |
|---|---|
| `games` (default) | games with a result after the move (ChessBase's N) |
| `score` | half-points scored by the side that plays it |
| `marks` | as `games`, but 0 for a move marked "Don't play in tournament" |

Weights are scaled down per position when the largest does not fit in 16 bits.
Moves that did not decode, or that lead out of the book, are left out.

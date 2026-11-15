# Tasks

## 1. Probe

- [x] 1.1 Bytes after the bytes-used mark: non-zero in most pages; parsed entries there are stale copies
- [x] 1.2 Orphan entries and Zobrist collisions on real books (reported by the export)

## 2. Implement

- [x] 2.1 `Stats::to_bytes`, `Ctg::read_header`, `ctg::page_used`
- [x] 2.2 `Book::walk` (parallel breadth-first, collision check, reached entries)
- [x] 2.3 `book export --sqlite` (feature `sqlite`) and `--polyglot`
- [x] 2.4 `book verify-export`

## 3. Tests

- [x] 3.1 Synthetic book: export, page-for-page rebuild, views, PolyGlot record, a changed byte is caught
- [x] 3.2 Real book: python-chess finds the positions of the PowerBook export, its moves match `book probe`, and it reads the `.bin`
- [x] 3.3 `verify-export` of the whole PowerBook export: header and 526,837 of 526,837 pages identical (whole pages, with tails), 0 positions without an entry, 0 moves differing from their entry, 0 missing children, 463 s
- [x] 3.4 An export of HIARCS, verified the same way: 32,330,172 entries (0 orphans), 62,435,520 positions, 63,356,673 moves, 0 collisions, exported in 238 s; header and 561,556 of 561,556 pages identical, 0 positions without an entry, 0 moves differing, 0 missing children, 558 s

## 4. Documentation

- [x] 4.1 `docs/book-export.md`, provenance row, README and CHANGELOG

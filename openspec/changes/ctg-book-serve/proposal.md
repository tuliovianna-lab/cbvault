# Serve opening books to other programs

## Why

An application that shows a book's moves next to its board (in Python, say)
needs `.ctg` lookups without reimplementing the format or exporting the book
first. Running `cbvault book probe` per position rebuilds the in-memory index
every time: 0.7 s and 221 MB for a 27.6-million-position book. A process that
keeps books open answers each probe in microseconds, and the format's rules
stay in one place.

## What Changes

- **`cbvault book serve <book> [<book>…]`** opens the books and prints one
  ready line. That line lists each book with its name, path, positions, damaged
  pages and index time, plus the books that could not be opened and why. The
  process then answers one request per line until its input closes.
- **Requests** are tab-separated: `id`, book name, `fen` or `moves`, value.
  Tabs need no quoting, because a FEN and SAN moves never contain one.
- **Replies** are the `book probe --json` line with `"id"` first, built by the
  same function, or `{"id":…,"error":…}` for a bad request. A bad request does
  not stop the process.
- `book probe --json` gains the values ChessBase shows, computed by the
  reader:
  - per move: `n`, `score`, `draw_pct`, `av`, `evaluation_symbol`,
    `main_move`, `dont_play`;
  - per position: `n`, `av`, `opponent_av`, `evaluation`, `evaluation_symbol`.

## Non-goals

- A network server: standard input and output are enough for a parent
  process, and they open no port.
- A JSON request format: it would need a parser in the CLI, and the tab
  format is unambiguous for the requests that exist.

## Impact

A new CLI module, `book_serve`. `book probe --json` keeps every field it had
and adds new ones. No new dependency.

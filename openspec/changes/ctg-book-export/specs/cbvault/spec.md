# ctg-book-export

## ADDED Requirements

### Requirement: A book exports to SQLite without losing a byte

The CLI SHALL export every entry of a `.ctg` to a SQLite file: its key and move
bytes raw, and its 33 statistics bytes as integer columns that rebuild them
exactly. It SHALL keep the header page, and on request the bytes after each
page's bytes-used mark. It SHALL NOT overwrite an existing file.

#### Scenario: An export rebuilds the book

- **WHEN** a book is exported with page tails and the export is verified
- **THEN** every page rebuilt from the SQLite file is identical to the original

#### Scenario: A changed byte is caught

- **WHEN** a value in the export differs from the book
- **THEN** verification reports the page as different

### Requirement: Exported positions are real positions under PolyGlot keys

The export SHALL walk the book from the initial position and key each reached
position by its PolyGlot Zobrist hash, with its real side to move and moves in
UCI. Entries the walk does not reach SHALL remain in the export, marked
unreached. Zobrist collisions SHALL be counted, not merged silently.

#### Scenario: A consumer finds a position without CTG logic

- **WHEN** a program computes a position's PolyGlot key
- **THEN** it finds the position's moves and statistics in the export

### Requirement: Established rules travel with the export

The export SHALL carry the established move symbols, evaluation signs and move
colours as tables generated from the reader's own code. Its views SHALL show a
position's moves with N, score, draws, average rating, symbol, evaluation and
colour.

#### Scenario: The view agrees with the reader

- **WHEN** a position's moves are read from the `book_moves` view
- **THEN** they match `cbvault book probe` for that position

### Requirement: A book exports to PolyGlot

The CLI SHALL write a PolyGlot `.bin` sorted by key, with a weight derived by a
stated policy, and SHALL leave out moves that did not decode or that lead out
of the book.

#### Scenario: The initial position is keyed as PolyGlot keys it

- **WHEN** a book with the initial position is exported to PolyGlot
- **THEN** its moves are stored under key `0x463b96181691fc9c`

# ctg-book-serve

## ADDED Requirements

### Requirement: Books are served over standard input and output

The CLI SHALL keep one or more books open and answer one probe per input line
with one output line. The answer SHALL be the same JSON that `book probe --json`
prints for that position, with the request's id. The process SHALL end when its
input closes.

#### Scenario: A served probe equals a one-off probe

- **WHEN** a position is requested from `book serve`
- **THEN** the reply is the `book probe --json` output for it, with the id added

### Requirement: Failures are reported, not fatal

A book that cannot be opened SHALL be listed with its reason in the ready line,
and the other books SHALL still be served. A malformed request, an unknown book
or an illegal move SHALL be answered with an error, and the process SHALL keep
serving.

#### Scenario: A locked book does not stop the others

- **WHEN** one of the books cannot be opened
- **THEN** the ready line lists it under errors
- **AND** probes to the other books are answered

#### Scenario: A bad request is answered with an error

- **WHEN** a request names an unknown book
- **THEN** the reply carries an error and the next request is still answered

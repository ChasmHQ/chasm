---
name: readme-writing
description: Write and edit README and user-facing docs so every command runs and every transcript is real output. Use when creating or editing a README, a docs/ page, a project landing page, or any prose that shows commands and their output.
metadata:
  trigger: Creating or editing README.md, docs/*.md, or any user-facing page containing commands and terminal output
---

# README and user-doc writing

A README is a promise that the commands in it work. The fastest way to break trust is a
command that names a path the reader does not have, or a transcript whose numbers were
typed by hand. Everything below serves that.

## Hard rules

1. **Run every command, paste what comes back.** Never hand-write, hand-edit, or carry
   forward a transcript. Capture at a fixed terminal width (`COLUMNS=110`) so blocks do not
   wrap mid-line, and paste verbatim. If a value is per-run (a random address, a hash, a
   timing), that is fine, but it must be a real one from a real run.

2. **Every path must ship.** Commands point at files a reader gets when they clone. Not
   test fixtures, not `/tmp/scratch`, not a directory that only exists on your machine. If
   there is no runnable example, create one under `examples/` and make the docs use it.

3. **Behavior beats prose.** When a run contradicts the docs, the run is right. Fix the
   sentence, and report the discrepancy separately rather than quietly writing around it.
   Regenerating transcripts is the cheapest bug-finding pass a project gets.

4. **Fences: `bash` for anything a terminal produced**, commands and their output alike.
   Bare fences only for things a terminal never printed, such as a directory tree diagram.

5. **Keep metadata in agreement.** The package description, the repo tagline and the README
   opening line are read as one claim. Change one, check the others.

## Openers

Every section opener must carry payload. Delete or fold anything that only restates the
heading or is too short to inform.

| Bad | Why | Fix |
|---|---|---|
| `To install X, run the following commands:` | Restates the heading `## Install` | Say what the reader ends up with: `Clone the repository and install X as a global command:` |
| `Your script needs no changes.` | Five words, no payload, dangles before the real sentence | Fold into the sentence it introduces with a colon |
| `The tests are written using pytest and can be found in the tests directory.` | The heading and the directory name already said it | `The suite runs under pytest and lives in tests/:` |

**Do not presume the reader's context.** "Your existing script" assumes they have one, and a
first-time reader stalls on "which script?". Name the artifact instead: "a plain web3.py
script, the kind that deploys and calls contracts against an in-process chain".

## Feature bullets

- **Keep the category noun.** If the tagline calls the project a "playground" or a
  "toolkit" but never says *debugger*, *linter*, *proxy*, a cold reader does not learn what
  it is. Pair the evocative word with the plain one.
- **A strong word lands once.** The same word in the tagline and the first bullet reads as
  filler. Pick the stronger position and cut the other.
- **Check the word is not already claimed.** "Solidity playground" means Remix to that
  ecosystem. A term that maps to someone else's product sends the wrong mental model.
- **Every claim must be findable.** A bullet promising a feature the code does not have
  sends readers hunting through docs for it. Verify against the source before shipping it.

## Language

Common non-native tells, and the fix:

- Comma splice joining two full sentences (`Here you are root, it's your playground to X`).
  Split, or subordinate one clause.
- `Whether A or B` with no main clause to land on. It needs a "you can do both" ending.
- A repeated intensifier in one sentence (`if you already know Foundry, you already know X`).
- Participles stacked with no connective (`built for X, running on Y`) where `and` is meant.
- Sentence fragments as standalone lines (`Running on Py-EVM`), and missing terminal periods.
- Consultant vocabulary where a plain verb works: `reason about` for `check`, `leverage` for
  `use`, `surface` for `show`.
- Metaphors the reader has to decode: `altitude` for abstraction level, `go under the
  language` for `go deeper`. Prefer the ordinary word every time.

Style constraints that apply to all prose here: no em-dashes, no emojis, and no semicolons
(split the sentence instead). Code blocks are exempt.

## Editing an existing README

Work from the reader's path, not the file's order. Read each section opener first, since
that is where thin sentences hide. When changing a command, re-run it and re-capture the
output in the same pass, or the block and the command drift apart. If the user has edited
the file between your turns, re-read it before applying an edit rather than assuming your
last version is current.

# customs

Checks a Mojo project's imports, how it builds its tests, and its code for
shapes a known compiler bug miscompiles. Seven rules, in three groups. The
full guide, with examples and how to fix each finding, is
on [magmalake.org/s/customs](https://magmalake.org/s/customs).

**`imports`**

1. **`import-cycle`: no import cycles.** A module imports only modules that
   don't import it back, directly or through others. Mojo compiles a cycle,
   so the compiler won't catch one. Every import counts: one inside a
   function, and one of a package's `__init__.mojo` (so a module of `pkg`
   that does `from pkg import x`, while `pkg/__init__.mojo` imports that
   module, is a cycle).
2. **`import-through-reexport`: import a name from the file that defines
   it.** `from m import x`, and `m.x` after `import m` (or
   `from pkg import m`), name the module where `x` is defined, not one that
   only imports it and passes it on. A package's `__init__.mojo` is the one
   place a re-export may be imported from.
3. **`unused-import`: every name a file imports is used in it.** Outside a
   package's `__init__.mojo`, whose imports are what it re-exports. A
   module that only passes names on (a compatibility shim) becomes a
   package: `shim.mojo` -> `shim/__init__.mojo`, with the same import path.
   Since 0.4.0. To keep an import anyway, as Python's tools do:
   - `from m import x as x` (or `import m as m`): a redundant alias, the
     typing convention ([PEP 484](https://peps.python.org/pep-0484/#stub-files))
     for "imported on purpose";
   - `# noqa: unused-import` on the name's line, or on the first line of
     its statement for all of its names, as flake8 and ruff read a
     [`# noqa`](https://docs.astral.sh/ruff/linters/#error-suppression).

**`tests`**

4. **`functions-in-imported-module`: a file that calls
   `__functions_in_module()` isn't imported.** In Mojo 1.2.0 that list is
   incomplete when its file is imported rather than built as the main file.
   A test file whose `main` is
   `TestSuite.discover_tests[__functions_in_module()]().run()`, imported and
   run by another file (to build every test into one binary), can run some
   of its tests or none, and exit 0. The finding names the importing line.
   Build the tests once by registering them by name instead, as
   [census's `scripts/test.sh`](https://github.com/morca-dev/census/blob/main/scripts/test.sh)
   does: it generates the main under `build/` (which customs skips), imports
   each `def test_*` by name, and checks the count each file runs.
5. **`mojo-run-per-test`: no command runs `mojo run` on more than 3 test
   files.** Each `mojo run` compiles the library code the tests share again.
   In [morca-dev/census](https://github.com/morca-dev/census/issues/207),
   35 `mojo run -I src tests/test_*.mojo` took 665 s on a 16-core Linux
   machine (building each file separately, 663 s), and one binary of every
   test about 210 s; in CI the step went from 550 s to 256 s. The finding
   names the task, CI step or script, and the files it runs. See
   [How it reads tasks, workflows and scripts](#how-it-reads-tasks-workflows-and-scripts)
   for what it sees.
6. **`mojo-build-per-test`: no command runs `mojo build` on more than 3
   test files.** Building each test file into a binary of its own costs what
   `mojo run` per file does (663 s against 665 s in census). It counts
   `mojo build` itself and a wrapper that passes its arguments on to it: a
   shell function (`compile() { mojo build "$@"; }`) or a script
   (`scripts/mojo-build.sh -I src tests/test_x.mojo -o build/test-x`).
   Building several programs (pipelines, a CLI, a test's driver) isn't a
   finding: only test files count. Since 0.3.1.

**`compiler-bugs`**

Code that a known Mojo compiler bug compiles wrong. Each rule names its
upstream issue and the versions it's in, in its findings, and goes when the
issue is fixed. Since 0.5.0.

7. **`append-before-loop-exit`: no `while True:` loop that appends before
   its exit test.** Mojo 1.3.0.dev2026111905 and later, at the default
   optimization level, miscompile

   ```mojo
   while True:
       out.append(k.copy())
       if k == last:
           break
       k = k.next()
   ```

   ([modular/modular#7716](https://github.com/modular/modular/issues/7716)).
   The compiler copies what comes before the exit test to the end of the
   loop with the operands of the iteration before, so the append stores
   through the List's buffer and length from before it grew: elements are
   lost or shifted and freed memory is written, and the program can print a
   wrong result and exit 0. The finding names the append and the test. Do
   the first step before the loop and test at the top, or loop with `for`:

   ```mojo
   out.append(k.copy())
   while k != last:
       k = k.next()
       out.append(k.copy())
   ```

   A `print` in the loop happened to hide it in one program; that's no fix.
   Nightlies 1.3.0.dev2026111105 to 2026111805 compile the same loop wrong
   in another way, at every optimization level: its last iteration's append
   is lost. See [How it reads loops](#how-it-reads-loops) for which loops
   count.

Every rule is `deny` unless [configured](#configuration).

## `# noqa`

A `# noqa` comment on an import's line allows the rules reported there,
as [flake8](https://flake8.pycqa.org/en/latest/user/violations.html#in-line-ignoring-errors)
and ruff read one: `# noqa` alone allows all of them, and
`# noqa: NAME, NAME` only the rules or groups named
(`# noqa: unused-import`, `# noqa: imports`). It's read anywhere in the
comment, in any case. On the first line of a parenthesized import it covers
every name in it; on a name's own line, that name. It works for
`unused-import`, `import-through-reexport` and
`functions-in-imported-module`, not for `import-cycle`: a cycle is fixed,
or allowed for the whole project in the [configuration](#configuration).
On a `while True:` line, it allows `append-before-loop-exit` for that loop.
flake8's codes (`F401`) aren't customs's names, and allow nothing.

```mojo
from .copy import CopyIn                 # noqa: unused-import
from arrow_mlake.arrow import (          # noqa: unused-import
    bit_get,
    bit_set,
)
from json.cpu import parse as parse      # kept: a redundant alias
```

```text
$ customs
src/morca/_internal/ui/layout.mojo:16: import cycle: morca._internal.ui.layout -> morca._internal.web.page -> morca._internal.ui.layout [import-cycle]
    src/morca/_internal/ui/layout.mojo:16: from morca._internal.web.page import Page
    src/morca/_internal/web/page.mojo:8: from morca._internal.ui.layout import with_header_help
src/morca/_internal/usage.mojo:46: from morca._internal.store import Usage: `Usage` is defined in morca._internal.db.run_rows, and morca._internal.store only re-exports it; import it from morca._internal.db.run_rows [import-through-reexport]
...
customs: 259 modules, 384 files: 3 import cycles, 146 imports through a re-export, 106 unused imports, 0 imports of a file that calls __functions_in_module(), 0 commands that run `mojo run` on more than 3 files, 0 commands that run `mojo build` on more than 3 files, 1 `while True:` loop that appends before its exit test
```

## Usage

```text
customs [-I DIR]... [-A|-W|-D NAME]... [-v] [PROJECT]
```

- **PROJECT** (default `.`): its modules are the `.mojo` files under
  `PROJECT/src` (or `PROJECT` without a `src`), named as `mojo build -I src`
  names them: `src/a/b.mojo` is `a.b`, `src/a/__init__.mojo` the package
  `a`. A file is a module only inside packages, every directory above it
  having an `__init__.mojo`, as Mojo imports them. Its other `.mojo` files
  (tests, examples, binaries) are checked as importers. Hidden
  directories, `build`, `target` and `node_modules` are skipped.
- **`-I DIR`**: modules of another project, as `mojo build -I DIR`. Names
  imported from them are checked; their own imports aren't reported.
- **`-A NAME`, `-W NAME`, `-D NAME`**: allow (don't check), warn or deny a
  rule or a group, over the configuration file.
- **`-v`**: also notes what it can't place: each name that isn't found at
  the top level of the module it's imported from (a definition inside a
  `comptime if`, or a local variable that shadows a module's name), each
  `mojo run` whose files it can't tell, and each call before a `while
  True:` loop's exit test that may grow a container. Notes don't fail the
  run.

Each finding ends with its rule's name. It exits 1 when a `deny` rule finds
anything (a `warn` rule's findings are printed as `warning:` and don't
fail), and 2 on a usage, configuration or read error. Under GitHub Actions
(`GITHUB_ACTIONS=true`) it also prints each finding as an `::error` (or
`::warning`) annotation, so the lines show on a pull request.

## Configuration

In the project's `pixi.toml`, after clippy's style: a level per rule or per
group, and each rule's numbers as flat kebab-case keys named after it.

```toml
[tool.customs]
mojo-run-per-test-threshold = 3     # report a command that runs more test files
mojo-build-per-test-threshold = 3   # report a command that builds more test files

[tool.customs.lints]
imports = "deny"                    # group: import-cycle, import-through-reexport, unused-import
tests = "deny"                      # group: functions-in-imported-module, mojo-run-per-test, mojo-build-per-test
compiler-bugs = "deny"              # group: append-before-loop-exit
# The examples' tests are separate programs: they can't share a binary.
mojo-run-per-test = "warn"          # a rule's own level wins over its group's
```

- Levels: `deny` reports and fails, `warn` reports and passes, `allow`
  doesn't check. Every group is `deny` by default.
- A rule's own level wins over its group's, in the file and on the command
  line; the command line (`-A`, `-W`, `-D`) wins over the file.
- `lints` may also be an inline table (`lints = { tests = "warn" }`) or
  dotted keys (`lints.tests = "warn"`).
- An unknown option, rule, group or level is an error (exit 2), so a
  misspelt one doesn't go unnoticed. pixi itself ignores the `[tool]` table.

## In GitHub Actions

```yaml
  customs:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: magmalake/customs@v0.5.0
```

The action downloads the release of its own tag (or `version:`, or the
latest when its ref isn't a version) for the runner's platform and runs it
with `args:` (default `.`). Findings show as annotations on the pull request.

## Example: magmalake

The Mojo repos of [magmalake](https://github.com/magmalake) run customs
on each pull request, as a job their one required check waits for, as in
[zstd.mojo](https://github.com/magmalake/zstd.mojo/blob/main/.github/workflows/ci.yml):

```yaml
  # Imports: no cycles between modules, and each name imported from the
  # module that defines it (github.com/magmalake/customs).
  customs:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: magmalake/customs@v0.5.0

  ci:
    needs: [release, release-check, test, lint-bench, customs]
```

Adding it found two kinds of fixes. In
[postgres.mojo](https://github.com/magmalake/postgres.mojo/pull/20), a cycle:

```text
src/postgres/connection.mojo:103: import cycle: postgres.connection -> postgres.copy -> postgres.connection
    src/postgres/connection.mojo:103: from .copy import CopyIn
    src/postgres/copy.mojo:60: from .connection import _ConnCell
```

`copy` needed the connection's shared cell and the helpers that raise
errors on it, and `connection` needed `CopyIn`; the cell and helpers moved,
unchanged, into a module below both. In
[parquet.mojo](https://github.com/magmalake/parquet.mojo/pull/69), imports
through a compatibility shim:

```text
tests/carrow_check.mojo:42: from parquet.arrow import bit_get: `bit_get` is defined in arrow_mlake.arrow, and parquet.arrow only re-exports it; import it from arrow_mlake.arrow
```

The shim exists for consumers, so it became a package
(`parquet/arrow/__init__.mojo`), where a re-export is allowed, and the
import path stayed the same.

## How it reads Mojo

It's a scanner, not a parser: string literals and comments are blanked
first, then `from … import …` (with parentheses or backslashes over lines)
and `import …` statements are read wherever they are, and a module's names
are what it defines at column 0 with `def`, `fn`, `struct`, `trait`,
`comptime`, `alias` or `var`. A name it can't place is a note, never a
finding.

For `functions-in-imported-module`, an import names a file when it's one of
the project's modules, or a file beside the importer (`import test_a` in
`tests/all.mojo` is `tests/test_a.mojo`, as Mojo finds it next to the file
it builds). A file that only mentions `__functions_in_module()` in a comment
or a string doesn't count.

## How it reads tasks, workflows and scripts

`mojo-run-per-test` and `mojo-build-per-test` are a scanner too. They
read, at PROJECT:

- the tasks of `pixi.toml`: `[tasks]`, `[feature.*.tasks]` and their
  `target.*` forms, as strings, `{ cmd = …, depends-on = … }` tables or
  `[tasks.NAME]` tables; a task is counted with its `depends-on` chain;
- the `run:` steps of `.github/workflows/*.yml` and `*.yaml`;
- the shell scripts those run (`sh x.sh`, `bash x.sh`, `./x.sh`,
  `scripts/x.sh`, `source x.sh`), the code of `bash -c '…'` and of `$(…)`,
  and the tasks run with `pixi run NAME`; and what those run in turn.

In each, the commands are cut into words as a shell would, comments and
here-documents dropped, and a `mojo run X.mojo`, or `mojo X.mojo` (the
same), counts `X.mojo`. Inside `for f in WORDS; do … done`, `mojo run "$f"`
(or `tests/test_$f.mojo`) counts each word, a glob (`tests/test_*.mojo`)
counting each file it matches, as does `$(ls GLOB)`; so do variables set
to such words. `mojo build X.mojo` (and `ouromo build`) counts the same way
for `mojo-build-per-test`, and so does a call to a shell function or a
script that passes its arguments on to `mojo build` (`"$@"`): the call's
arguments are what is built, and a function's `$1`... are its arguments.

Only test files are counted: under a directory `test` or `tests`, or named
`test_*`, and, when the file exists, with a `def test_` at its top level.
A program under `tests/` (a driver a shell test starts, a file that must
fail to compile) isn't one. A task, step or script is reported when what
it runs, leaving out the files of what it runs that is reported already,
is more than the threshold: the place to change.

Where it can't tell, it doesn't report. A `mojo run` whose file it can't
work out (a variable it doesn't know, a matrix value `${{ … }}`, a task
argument `{{ … }}`, `xargs`, `while read`, `find`) is a note with `-v`, not
a count; so is a `mojo build`. It doesn't follow `cd`, `make` or `just`,
scripts named by a variable, a script's own arguments (only a function's),
a task's `cwd`, nor `pyproject.toml`'s `[tool.pixi]`.
`echo` and `printf` lines are skipped, so a `mojo run` in a message isn't
counted.

## How it reads loops

`append-before-loop-exit` reads statements, still a scanner: after the
blanking, a line that an open bracket or a `\` continues is read with the
lines it takes, `;` cuts statements, and each `if`, `for`, `while`, `def`
... has the block indented under it (or what follows its `:` on the line).
In each `while True:` (or `while 1:`) loop, at any depth, it finds the first
statement of the body that can leave the loop or end the iteration: one
that holds a `break` of this loop (not of a loop inside it), a `return`, a
`raise` or a `continue`. When that's an `if` (with its `elif`s and `else`)
that holds a `break`, an `X.append(...)` before it in the body, on a name
the body doesn't declare with `var` (`out`, `self.items`), is a finding.

The compiler decided the rest: each shape was built with 1.3.0.dev2026111905,
2026112017 and 2026112105 at the default optimization level, and run. It
miscompiles an append of an `Int` or of a struct; to a `mut` argument or to
`self.items` in a method; in a `while True:` inside a `for`; with `while 1:`,
with the `break` in the `else`, and with the test at the loop's end. It
doesn't with the test at the top, a `for` with `break`, or the test first;
nor for a `Dict` or `Set` insert, `String +=` or `.write`, `List +=`,
`extend`, `insert` or an item assignment; nor for an append inside an `if`
or an inner `for`, or before a `return`, `raise` or `continue` that comes
first. Whether it does also depends on how much the loop does before the
test (a second append, or a `List[String]`, hid it), which a scanner can't
measure: every such append is reported.

Where it can't tell, it doesn't report. With `-v`, an append inside a block
before the test, or before a `return`, `raise` or `continue`, is a note, and
so is each call there that may grow a container: `push(out, k)`,
`acc.add(k)`, `var n = r.read(buf)`. A function or method that appends
miscompiles as `out.append` does, but customs doesn't know which ones
append. An example in a docstring isn't read.

## Building

```sh
cargo build --release      # target/release/customs
cargo test
```

It has no dependencies. The release builds Linux binaries for musl, which
are static, and macOS ones, which link only the system's `libSystem`: each
is one file to download and run.

## License

Apache License 2.0: see [LICENSE](LICENSE) and [NOTICE](NOTICE).

# customs

Checks a Mojo project's imports. Two rules, fixed for now:

1. **No import cycles.** A module imports only modules that don't import it
   back, directly or through others. Mojo compiles a cycle, so the
   compiler won't catch one. Every import counts: one inside a function,
   and one of a package's `__init__.mojo` (so a module of `pkg` that does
   `from pkg import x`, while `pkg/__init__.mojo` imports that module, is
   a cycle).
2. **Import a name from the file that defines it.** `from m import x`, and
   `m.x` after `import m` (or `from pkg import m`), name the module where
   `x` is defined, not one that only imports it and passes it on. A
   package's `__init__.mojo` is the one place a re-export may be imported
   from.

```text
$ customs
src/morca/_internal/ui/layout.mojo:16: import cycle: morca._internal.ui.layout -> morca._internal.web.page -> morca._internal.ui.layout
    src/morca/_internal/ui/layout.mojo:16: from morca._internal.web.page import Page
    src/morca/_internal/web/page.mojo:8: from morca._internal.ui.layout import with_header_help
src/morca/_internal/usage.mojo:46: from morca._internal.store import Usage: `Usage` is defined in morca._internal.db.run_rows, and morca._internal.store only re-exports it; import it from morca._internal.db.run_rows
...
customs: 259 modules, 384 files: 3 import cycles, 146 imports through a re-export
```

## Usage

```text
customs [-I DIR]... [-v] [PROJECT]
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
- **`-v`**: also notes each name that isn't found at the top level of the
  module it's imported from (a definition inside a `comptime if`, or a
  local variable that shadows a module's name). Notes don't fail the run.

It exits 1 when it finds anything, and 2 on a usage or read error. Under
GitHub Actions (`GITHUB_ACTIONS=true`) it also prints each finding as an
`::error` annotation, so the lines show on a pull request.

## In GitHub Actions

```yaml
  customs:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: magmalake/customs@v0.2.1
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
      - uses: magmalake/customs@v0.2.1

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

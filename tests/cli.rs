//! The binary over small projects written to a temporary directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh directory holding `files` (path, contents).
fn project(files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "customs-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    for (p, src) in files {
        let path = dir.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, src).unwrap();
    }
    dir
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn run(dir: &Path, extra: &[&str]) -> Run {
    let o = Command::new(env!("CARGO_BIN_EXE_customs"))
        .args(extra)
        .arg(dir)
        .current_dir(dir)
        .env_remove("GITHUB_ACTIONS")
        .output()
        .unwrap();
    Run {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

#[test]
fn a_clean_project_passes() {
    let d = project(&[
        ("src/app/__init__.mojo", "from app.core import Thing\n"),
        ("src/app/core.mojo", "struct Thing:\n    pass\n"),
        (
            "src/app/use.mojo",
            "from app.core import Thing\nfrom app import Thing as T\n",
        ),
        (
            "tests/t.mojo",
            "from app import Thing\nfrom app.core import Thing\n",
        ),
    ]);
    let r = run(&d, &[]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(r.out.is_empty(), "{}", r.out);
    assert!(
        r.err
            .contains("3 modules, 4 files: 0 import cycles, 0 imports through a re-export"),
        "{}",
        r.err
    );
}

#[test]
fn a_cycle_is_reported_with_its_imports() {
    let d = project(&[
        ("src/p/__init__.mojo", ""),
        ("src/p/a.mojo", "from p.b import f\ndef g(): pass\n"),
        ("src/p/b.mojo", "def f():\n    from p.c import h\n"),
        ("src/p/c.mojo", "from p.a import g\ndef h(): pass\n"),
        ("src/p/d.mojo", "from p.a import g\n"),
    ]);
    let r = run(&d, &[]);
    assert_eq!(r.code, 1);
    assert!(
        r.out.contains("import cycle: p.a -> p.b -> p.c -> p.a"),
        "{}",
        r.out
    );
    assert!(
        r.out.contains("src/p/b.mojo:2: from p.c import h"),
        "{}",
        r.out
    );
    assert_eq!(r.out.matches("import cycle").count(), 1, "{}", r.out);
}

#[test]
fn importing_a_package_init_from_inside_it_is_a_cycle() {
    let d = project(&[
        ("src/m/__init__.mojo", "from m.ctx import Ctx\n"),
        ("src/m/ctx.mojo", "from m import Other\nstruct Ctx: pass\n"),
        ("src/m/other.mojo", "struct Other: pass\n"),
    ]);
    let r = run(&d, &[]);
    assert!(r.out.contains("import cycle: m -> m.ctx -> m"), "{}", r.out);
}

#[test]
fn a_reexport_is_reported_with_the_defining_module() {
    let d = project(&[
        ("src/q/__init__.mojo", ""),
        ("src/q/defs.mojo", "struct Row: pass\ncomptime N = 1\n"),
        ("src/q/view.mojo", "from q.defs import Row, N\ndef show(): pass\n"),
        ("src/q/page.mojo", "from q.view import Row, show\nimport q.view as v\ndef f():\n    _ = v.N\n    v.show()\n"),
        ("tests/t.mojo", "from q.view import (\n    show,\n    Row,\n)\n"),
    ]);
    let r = run(&d, &[]);
    assert_eq!(r.code, 1);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 3, "{}", r.out);
    assert!(r.out.contains(
        "src/q/page.mojo:1: from q.view import Row: `Row` is defined in q.defs, and q.view only re-exports it; import it from q.defs"
    ), "{}", r.out);
    assert!(
        r.out
            .contains("src/q/page.mojo:4: v.N: `N` is defined in q.defs"),
        "{}",
        r.out
    );
    assert!(
        r.out.contains("tests/t.mojo:1: from q.view import Row"),
        "{}",
        r.out
    );
}

#[test]
fn a_chain_of_reexports_names_the_end() {
    let d = project(&[
        ("src/a.mojo", "def f(): pass\n"),
        ("src/b.mojo", "from a import f\n"),
        ("src/c.mojo", "from b import f as g\n"),
        ("src/d.mojo", "from c import g\n"),
    ]);
    let r = run(&d, &[]);
    assert!(
        r.out
            .contains("src/d.mojo:1: from c import g: `g` is defined in a,"),
        "{}",
        r.out
    );
    assert!(
        r.out
            .contains("src/c.mojo:1: from b import f: `f` is defined in a,"),
        "{}",
        r.out
    );
}

#[test]
fn submodules_relative_imports_and_docstrings() {
    let d = project(&[
        ("src/k/__init__.mojo", ""),
        ("src/k/calls.mojo", "from k.abi import Slots\ndef load(): pass\n"),
        ("src/k/abi.mojo", "struct Slots: pass\n"),
        (
            "src/k/user.mojo",
            "\"\"\"from k.calls import Slots\"\"\"\nfrom k import calls\nfrom .calls import load\n# from k.calls import Slots\ndef f():\n    calls.load()\n    _ = calls.Slots\n",
        ),
    ]);
    let r = run(&d, &[]);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines, vec!["src/k/user.mojo:7: calls.Slots: `Slots` is defined in k.abi, and k.calls only re-exports it; import it from k.abi [import-through-reexport]"], "{}", r.out);
}

#[test]
fn included_modules_are_looked_up_but_not_reported() {
    let lib = project(&[
        ("lib/__init__.mojo", "from lib.tables import Schema\n"),
        (
            "lib/tables.mojo",
            "from lib.schema import Schema\nfrom lib.x import y\n",
        ),
        ("lib/schema.mojo", "struct Schema: pass\n"),
        (
            "lib/x.mojo",
            "from lib.tables import Schema\ndef y(): pass\n",
        ),
    ]);
    let d = project(&[(
        "src/app.mojo",
        "from lib.tables import Schema\nfrom lib import Schema\n",
    )]);
    let r = run(&d, &["-I", lib.to_str().unwrap()]);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 1, "{}", r.out);
    assert!(
        r.out.contains(
            "src/app.mojo:1: from lib.tables import Schema: `Schema` is defined in lib.schema"
        ),
        "{}",
        r.out
    );
}

#[test]
fn unknown_names_are_notes_only_with_verbose() {
    let d = project(&[
        ("src/a.mojo", "def f(): pass\n"),
        ("src/b.mojo", "from a import nothing\n"),
    ]);
    let r = run(&d, &[]);
    assert_eq!(r.code, 0, "{}", r.out);
    let r = run(&d, &["-v"]);
    assert_eq!(r.code, 0);
    assert!(
        r.out
            .contains("src/b.mojo:1: note: from a import nothing: `nothing` isn't defined"),
        "{}",
        r.out
    );
}

#[test]
fn github_annotations() {
    let d = project(&[
        ("src/a.mojo", "from b import f\n"),
        (
            "src/b.mojo",
            "from a import g\ndef f(): pass\ndef g(): pass\n",
        ),
    ]);
    let o = Command::new(env!("CARGO_BIN_EXE_customs"))
        .arg(&d)
        .current_dir(&d)
        .env("GITHUB_ACTIONS", "true")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("::error file=src/a.mojo,line=1,title=customs::import cycle: a -> b -> a [import-cycle]%0Asrc/a.mojo:1: from b import f"), "{out}");
}

#[test]
fn usage_errors_exit_2() {
    let o = Command::new(env!("CARGO_BIN_EXE_customs"))
        .arg("--nope")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = Command::new(env!("CARGO_BIN_EXE_customs"))
        .arg("/does/not/exist")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn only_files_in_packages_are_modules() {
    let d = project(&[
        ("pkg/__init__.mojo", "from pkg.a import f\n"),
        ("pkg/a.mojo", "def f(): pass\n"),
        ("pkg/loose/b.mojo", "def g(): pass\n"),
        ("tests/helpers.mojo", "from tests.t import x\n"),
        ("tests/t.mojo", "from tests.helpers import y\n"),
        ("top.mojo", "from pkg.a import f\n"),
    ]);
    let r = run(&d, &[]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.err.contains("3 modules, 6 files"), "{}", r.err);
}

const FOUR_RUNS: &str = "[tasks]\n# One run per file.\ntest = \"mojo run -I src tests/test_a.mojo && mojo run -I src tests/test_b.mojo && mojo -I src tests/test_c.mojo && mojo run tests/test_d.mojo\"\n";

fn tests_project(pixi: &str) -> PathBuf {
    project(&[
        ("pixi.toml", pixi),
        ("src/p/__init__.mojo", ""),
        ("src/p/a.mojo", "def f(): pass\n"),
        ("tests/test_a.mojo", "from p.a import f\n"),
        ("tests/test_b.mojo", ""),
        ("tests/test_c.mojo", ""),
        ("tests/test_d.mojo", ""),
    ])
}

#[test]
fn mojo_run_per_test_in_a_task() {
    let d = tests_project(FOUR_RUNS);
    let r = run(&d, &[]);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    assert!(
        r.out.contains("pixi.toml:3: task `test` runs `mojo run` on 4 .mojo files, more than 3: each compiles the code they share again; build them once, into one binary [mojo-run-per-test]"),
        "{}",
        r.out
    );
    assert!(
        r.out
            .contains("    pixi.toml:3: mojo run tests/test_a.mojo"),
        "{}",
        r.out
    );
    assert!(r.out.contains("    ... and 1 more"), "{}", r.out);
    assert!(
        r.err.contains("0 import cycles, 0 imports through a re-export, 0 imports of a file that calls __functions_in_module(), 1 command that runs `mojo run` on more than 3 files"),
        "{}",
        r.err
    );
}

#[test]
fn threshold_and_levels_from_pixi_toml_and_the_command_line() {
    let d = tests_project(&format!(
        "{FOUR_RUNS}\n[tool.customs]\nmojo-run-per-test-threshold = 4\n"
    ));
    assert_eq!(run(&d, &[]).code, 0);

    let d = tests_project(&format!("{FOUR_RUNS}\n[tool.customs.lints]\n# Separate programs.\nmojo-run-per-test = \"warn\"\ntests = \"deny\"\n"));
    let r = run(&d, &[]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(
        r.out.contains("pixi.toml:3: warning: task `test`"),
        "{}",
        r.out
    );
    assert!(r.err.contains("on more than 3 files (warn)"), "{}", r.err);
    // The command line over the file; a rule over its group.
    assert_eq!(run(&d, &["-D", "mojo-run-per-test"]).code, 1);
    assert_eq!(run(&d, &["-Dtests"]).code, 1);
    let r = run(&d, &["-A", "tests"]);
    assert_eq!(r.code, 0);
    assert!(r.out.is_empty(), "{}", r.out);
    assert!(!r.err.contains("mojo run"), "{}", r.err);

    let d = tests_project(&format!(
        "{FOUR_RUNS}\n[tool.customs.lints]\ntests = \"allow\"\n"
    ));
    assert_eq!(run(&d, &[]).code, 0);
    assert_eq!(run(&d, &["-W", "tests", "-D", "mojo-run-per-test"]).code, 1);
}

#[test]
fn configuration_errors_exit_2() {
    let d = tests_project("[tool.customs.lints]\nmojo-run-per-tests = \"warn\"\n");
    let r = run(&d, &[]);
    assert_eq!(r.code, 2);
    assert!(
        r.err
            .contains("pixi.toml:2: [tool.customs]: no rule or group `mojo-run-per-tests`"),
        "{}",
        r.err
    );
    let d = tests_project("");
    assert_eq!(run(&d, &["-W", "nothing"]).code, 2);
    assert_eq!(run(&d, &["-W"]).code, 2);
}

#[test]
fn depends_on_scripts_loops_and_workflows() {
    let d = project(&[
        (
            "pixi.toml",
            "[tasks]\none = \"mojo run tests/test_a.mojo\"\ntwo = { cmd = \"mojo run tests/test_b.mojo\", depends-on = [\"one\"] }\n[feature.dev.tasks]\nall = { depends-on = [\"two\", { task = \"three\" }] }\nthree = \"bash -c 'mojo run tests/test_c.mojo; mojo run tests/test_d.mojo'\"\nscript = \"sh scripts/t.sh\"\n",
        ),
        (
            "scripts/t.sh",
            "#!/bin/sh\n# mojo run tests/x.mojo, in a comment\necho \"mojo run tests/test_q.mojo\"\nfor f in tests/test_*.mojo; do\n    mojo run -I src \"$f\" || exit 1\ndone\n",
        ),
        (
            ".github/workflows/ci.yml",
            "jobs:\n  t:\n    steps:\n      - run: pixi run script\n      - name: each\n        run: |\n          for t in a b c d e; do\n            pixi run mojo run tests/test_$t.mojo\n          done\n      - run: mojo run ${{ matrix.test }}\n",
        ),
        ("tests/test_a.mojo", ""),
        ("tests/test_b.mojo", ""),
        ("tests/test_c.mojo", ""),
        ("tests/test_d.mojo", ""),
        ("tests/test_e.mojo", ""),
        ("examples/one.mojo", ""),
    ]);
    let r = run(&d, &["-v"]);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    // `all`: four through depends-on; the script, five through its loop,
    // and not the step that runs it; the step's own loop, five.
    assert!(
        r.out
            .contains("pixi.toml:5: task `all` runs `mojo run` on 4 .mojo files"),
        "{}",
        r.out
    );
    assert!(
        r.out
            .contains("scripts/t.sh:5: this script runs `mojo run` on 5 .mojo files"),
        "{}",
        r.out
    );
    assert!(
        r.out
            .contains(".github/workflows/ci.yml:6: this step runs `mojo run` on 5 .mojo files"),
        "{}",
        r.out
    );
    assert!(
        r.out
            .contains(".github/workflows/ci.yml:8: mojo run tests/test_a.mojo"),
        "{}",
        r.out
    );
    assert!(!r.out.contains("ci.yml:4: this step"), "{}", r.out);
    assert!(!r.out.contains("task `script`"), "{}", r.out);
    assert!(!r.out.contains("test_q"), "{}", r.out);
    assert!(
        r.out.contains(".github/workflows/ci.yml:10: note: mojo-run-per-test: can't tell which files `mojo run ${{ matrix.test }}` runs"),
        "{}",
        r.out
    );
    assert_eq!(r.out.matches("runs `mojo run` on").count(), 3, "{}", r.out);
}

#[test]
fn what_it_cant_tell_is_never_a_finding() {
    let d = project(&[
        (
            "pixi.toml",
            "[tasks]\nt = \"for f in $(find tests -name '*.mojo'); do mojo run $f; done\"\nu = \"ls tests/*.mojo | xargs -n1 mojo run\"\nv = { cmd = \"mojo run {{ file }}\", args = [\"file\"] }\nw = \"mojo run examples/a.mojo && mojo run examples/b.mojo && mojo run examples/c.mojo && mojo run examples/d.mojo\"\nx = \"mojo build tests/test_a.mojo && mojo format tests\"\n",
        ),
        ("tests/test_a.mojo", ""),
        ("tests/test_b.mojo", ""),
        ("tests/test_c.mojo", ""),
        ("tests/test_d.mojo", ""),
        ("examples/a.mojo", ""),
    ]);
    let r = run(&d, &["-v"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(
        r.out.matches("note: mojo-run-per-test").count(),
        3,
        "{}",
        r.out
    );
}

#[test]
fn an_imported_file_that_calls_functions_in_module() {
    let d = project(&[
        ("src/p/__init__.mojo", ""),
        ("src/p/a.mojo", "def f(): pass\n"),
        (
            "tests/test_a.mojo",
            "from std.testing import TestSuite\n\ndef test_x(): pass\n\ndef main() raises:\n    TestSuite.discover_tests[__functions_in_module()]().run()\n",
        ),
        (
            "tests/test_b.mojo",
            "# __functions_in_module() in a comment\ndef main():\n    pass\n",
        ),
        (
            "tests/all.mojo",
            "import test_a\nfrom test_b import main as b\nfrom p.a import f\n\ndef main() raises:\n    test_a.main()\n",
        ),
    ]);
    let r = run(&d, &[]);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 2, "{}", r.out);
    assert!(lines[0].starts_with("tests/all.mojo:1: import test_a: tests/test_a.mojo calls __functions_in_module(), which lists only some of a module's functions when it's imported"), "{}", r.out);
    assert!(
        lines[0].ends_with("[functions-in-imported-module]"),
        "{}",
        r.out
    );
    assert_eq!(lines[1], "    tests/test_a.mojo:6: __functions_in_module()");
    assert!(
        r.err
            .contains("1 import of a file that calls __functions_in_module()"),
        "{}",
        r.err
    );
    assert_eq!(run(&d, &["-A", "functions-in-imported-module"]).code, 0);
}

#[test]
fn a_module_that_calls_functions_in_module_imported_by_name() {
    let d = project(&[
        ("src/p/__init__.mojo", ""),
        (
            "src/p/harness.mojo",
            "def run_all():\n    _ = __functions_in_module()\n",
        ),
        ("src/p/b.mojo", "from p import harness\n"),
        ("src/p/c.mojo", "from .harness import run_all\n"),
    ]);
    let r = run(&d, &[]);
    assert_eq!(
        r.out.matches("[functions-in-imported-module]").count(),
        2,
        "{}",
        r.out
    );
    assert!(
        r.out
            .contains("src/p/b.mojo:1: from p import harness: src/p/harness.mojo calls"),
        "{}",
        r.out
    );
    assert!(
        r.out
            .contains("src/p/c.mojo:1: from .harness import run_all: src/p/harness.mojo calls"),
        "{}",
        r.out
    );
}

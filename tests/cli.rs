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
    assert_eq!(lines, vec!["src/k/user.mojo:7: calls.Slots: `Slots` is defined in k.abi, and k.calls only re-exports it; import it from k.abi"], "{}", r.out);
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
    assert!(out.contains("::error file=src/a.mojo,line=1,title=customs::import cycle: a -> b -> a%0Asrc/a.mojo:1: from b import f"), "{out}");
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

# What an AI agents directory is called

**Normative**, and small. It exists because the answer is not obvious and
because getting it wrong is expensive on one of the two platforms this
repository is developed on.

---

## §1 The names

§1.1 The instruction **file** at the repository root is `AGENTS.md`,
upper-case. That spelling is the cross-tool convention — it sits beside
`README.md` and `LICENSE`, and several tools look for it by that exact
name.

§1.2 `CLAUDE.md` is the same idea for Claude Code specifically, and keeps
its documented spelling for the same reason: a tool looks for it.

§1.3 The **directory** of longer agent guides is `agents/`, lower-case,
like every other directory in this workspace — `crates/`, `spec/`,
`docs/`, `local/`, `xtask/`.

---

## §2 Why the directory is not `AGENTS/`

§2.1 **A file and a directory of the same name cannot coexist on a
case-insensitive filesystem.** macOS defaults to case-insensitive, and this
workspace is developed on macOS and Linux both. `AGENTS.md` and `AGENTS/`
do not collide — the extension separates them — but `AGENTS/` and a future
`AGENTS` file would, and tooling that normalises case has one more way to
surprise somebody.

§2.2 More simply: upper-case at the root is a signal that says *read me
first*, and it earns that by being rare. A directory is not read first; it
is browsed when something in it is needed. `crates/` is not `CRATES/`.

§2.3 Git on a case-insensitive filesystem does not reliably notice a rename
that changes only case, so a directory that starts life as `AGENTS/` is
awkward to correct later. Choosing once, at the start, costs nothing.

---

## §3 What goes where

§3.1 `AGENTS.md` and `CLAUDE.md` are **short**, and say the things that
apply to every task: where the source of truth is, what the gate is, and
the two rules that must never be broken.

§3.2 `agents/` holds the longer per-area guides — one file per subject,
loaded when that subject is the task at hand.

§3.3 No file in either place is a source of truth. `spec/` is
([`index.md`](index.md) §0). An agent file is a map to the specification,
never a summary that could drift from it.

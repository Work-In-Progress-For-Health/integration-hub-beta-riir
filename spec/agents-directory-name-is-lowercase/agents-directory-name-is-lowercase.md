# AI agents directories are named `agents`, lowercase

Any directory holding AI agent definitions is named **`agents`** — all
lowercase, plural, no prefix, no suffix.

---

## §1 The rule

§1.1 An AI agents directory MUST be named exactly `agents`.

§1.2 Not `Agents`, not `AGENTS`, not `.agents`, not `agent`, not
`ai-agents`. One spelling, everywhere.

§1.3 This applies wherever such a directory appears — the workspace root, a
crate, a tooling directory, a nested project.

---

## §2 Why it is worth writing down

§2.1 **Case-insensitive filesystems hide the mistake.** macOS is
case-insensitive by default and Linux is not. A directory committed as
`Agents` resolves fine on a developer's laptop and fails in a Linux
container, and the failure arrives far from the change that caused it.

§2.2 **Tools look for a fixed name.** Anything that discovers agent
definitions by convention finds them or does not; a near-miss spelling
produces "no agents found" rather than an error naming the directory it
expected.

§2.3 **One spelling is greppable.** A repository with `agents/`, `Agents/`
and `.agents/` in it has three things to search for and no way to know
whether the list is complete.

---

## §3 Related, and deliberately not covered

§3.1 `AGENTS.md` — the uppercase Markdown file some tools read for
repository instructions — is a **file**, not a directory, and keeps its own
conventional spelling. `Integration-Hub-Beta/AGENTS.md` is one. This rule
does not rename it.

§3.2 Nothing here says a repository must *have* an agents directory. It says
what one is called if it does.

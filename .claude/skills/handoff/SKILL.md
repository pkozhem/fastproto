---
name: handoff
description: Save context at the end of a session — update .claude/SESSION.md (where we left off) and tick off closed items in TODO.md. Use when the user says "handoff", "write down where we stopped", "save context", "let's wrap up".
disable-model-invocation: true
---

# Handoff: write down where we left off

`.claude/SESSION.md` is loaded into every session automatically (via
`CLAUDE.md`), so it must stay **short — about 50 lines max**. It is not the
project history (that's in git and TODO.md) but a personal note on "what's in
progress right now". The file is not in git: every developer has their own.

## Steps

1. Gather facts: `git log --oneline -5`, `git status --short`, `git branch`, and
   what was done in this session.

2. **Rewrite `.claude/SESSION.md` entirely** using the template below. Drop
   anything no longer relevant (merged, closed). Write it in the language the user
   works in.

   ```markdown
   # Where we left off

   Updated: <date>, HEAD = <short hash> (<commit title>)

   ## In progress
   - what we're doing, on which branch, what's done / what's left

   ## Check status
   - when /gate last ran and with what result (or "not run")

   ## Open questions
   - what's waiting on the user's decision

   ## Next step
   - 1–3 items
   ```

3. **TODO.md** (the user's personal backlog, if it exists): mark closed items
   `[x]` and add a short "Done: ..." note in the file's own style and language.
   Add newly found tasks to the matching section.

4. **Long-lived knowledge.** If the session uncovered something that will stay
   true (a new pitfall, rule, or invariant), don't put it in SESSION.md — propose
   adding it to `CLAUDE.md` instead (show the exact text; don't edit without
   agreement).

5. Don't commit anything. Finish with 2–3 lines to the user: what was recorded.

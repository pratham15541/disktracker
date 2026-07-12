# DiskTracker — Cross-Model Session Prompts

These two prompts are how you start and end every AI coding session, regardless of which
model you're using (Antigravity/Gemini, Claude, etc.). They rely entirely on
`docs/AI_MASTER_PLAN_2.md`, `docs/EPOCH-2.md`  and `docs/PROGRESS.md` — never re-paste the architecture into chat.
Main

## Boot Sequence Prompt (start of every session)

```
Read docs/AI_MASTER_PLAN.md in full to understand the architecture and invariants of this
project. Then read docs/PROGRESS.md to see exactly where the last session left off.

Before replying, also:
1. Run `cargo build --target x86_64-pc-windows-gnu` and report whether it currently compiles.
2. View the actual crates/ directory structure and compare it to what PROGRESS.md claims
   is done. If PROGRESS.md says a loop is complete but the code doesn't match, say so —
   do not assume PROGRESS.md is correct just because it says so.

Do not write any code yet. Reply with:
- A 3-bullet summary of what you're supposed to build in the current active loop
- Confirmation the build state matches what PROGRESS.md claims (or a note if it doesn't)
- Any "Known Deviations" or "Open Issues" from PROGRESS.md that are relevant to this loop
```

This forces verification against the real repo state instead of trusting a markdown claim,
which is the main way cross-model handoffs silently go wrong.

## Handoff Prompt (end of a verified loop)

```
This loop has been manually tested on native Windows and confirmed working. Update
docs/PROGRESS.md:
- Move the current loop to "Verified on Windows? = Yes" with today's date and which model
  built it
- Set "Current Active Loop" to the next loop
- Update "Next Action" with a concrete, specific starting point (not just "start Loop N")
- Add a Session Log entry summarizing exactly what was built and exactly what was tested
- If anything deviated from AI_MASTER_PLAN.md, add it to "Known Deviations" — do not edit
  the master plan itself

Do not mark a loop verified unless the manual Windows test in AI_MASTER_PLAN.md §8 for that
loop actually passed. If it only compiles but wasn't run on Windows, say that explicitly
instead of marking it complete.
```

## Git Discipline

Commits alone aren't precise enough once you're several loops in and switching models. Use
tags, and don't merge to `main` until a loop is Windows-verified:

```bash
# work each loop on its own branch
git checkout -b loop-3-scanner-watcher

# ... agent works, you test manually on Windows ...

# once verified:
git checkout main
git merge loop-3-scanner-watcher
git tag loop-3-verified
git push --tags
```

If a model makes a mess mid-loop:

```bash
# reset to the last verified checkpoint, unambiguously
git reset --hard loop-2-verified
```

Never let an agent commit directly to `main` — only merge after your own manual Windows
verification, per the checklist in `AI_MASTER_PLAN.md` §8 for that loop.

## Why this holds up across models

- **The master plan is the only technical source of truth** — full detail, not a summary,
  so no model has to "remember" nuance from a prior chat; it's on disk.
- **PROGRESS.md is a claim, verified every boot** — not blindly trusted, because agents are
  instructed to check it against the actual repo/build state first.
- **Git tags give you a hard, unambiguous rollback point** per verified loop, so a bad model
  run costs you at most one loop's worth of work, not more.
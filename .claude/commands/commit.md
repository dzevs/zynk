# Commit

Commit the work in progress. Use **lowercase conventional commits** (no emojis, no AI co-author lines): the
title summarizes the change; the body explains what changed and why; add `refs #<issue>` when relevant. Stage
precisely with `git add <path>` — never `git add -A`. Run `just check` first; never commit through a failing
check. If anything is unclear, ask.

**Do NOT push.** Pushing to `main` / `origin` is a separate explicit operator gate (see `WORKFLOW.md`). Stop
after committing and report what landed.

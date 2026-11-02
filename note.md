# 

##

hide conversitions with llms that make it in the repo, we can persist it 
but we want to encrypt it i hink by default

likes of /home/dracon/Dev/dracon-utilities/dracon-system/conversation-2026-09-27-124138.txt

and pi session dumps they can have sensitivie info too

## 2026-11-02: skip-worktree on the 2026-10-28 pi-session export

`pi-session-2026-10-28T15-44-14-957Z_01a1832e-f46b-76e7-80b3-2377bc0b4468.html`
carries a skip-worktree flag, set deliberately: warden age encryption is
randomized, so every `git add` minted a fresh ~69 MB blob and the sync daemon
committed one every few minutes. The flag hides the file from status/diff/add
(`git add -f` still overrides). Undo with
`git update-index --no-skip-worktree -- <file>` — but the churn resumes if you
do. Long term these exports should live outside git history (see the
2026-11-02 audit followups).

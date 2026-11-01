# Report-only tmux persistence health

Use the existing `doctor` JSON/human health surfaces, not a new daemon or
mitigation path. Defaulted GuardPolicy settings select up to eight explicit
absolute sockets and maximum save-evidence age (default 900s, 60–86400s).
An empty list uses the current TMUX socket, or the standard TMUX_TMPDIR/default
socket when outside tmux. This is not a machine-wide socket discovery scan.

Keep separate typed observations:

- Server identity: query only `display-message -p` on an existing owned Unix
  socket, fixed format, bounded output/deadline. Check reported socket, server
  PID/starttime, socket device/inode before/after. Where available, bind the
  reported PID to a listening socket using its bounded proc net/unix table
  and fd links. No broad process scan or tmux command execution hooks.
- Producer identity: the installed tmux-persist-daemon convention uses a
  per-socket POSIX-cksum lock and PID. Read that candidate only, validate UID,
  script argv and TMUX socket AND server PID environment, bracket PID/starttime
  and process state. Missing/mismatched records mean unavailable registration,
  not proof that no other producer exists. T/t means observed stopped; its
  cause is unknown. Sleeping means only an existing producer, never success.
- Save evidence: only the server's explicit @resurrect-dir; do not guess plugin
  defaults from the observer's environment. Safely inspect `last` and a regular
  nonempty snapshot with bounded pane-record inspection. Report age, path and
  metadata identity. This establishes configured-path evidence, not who wrote
  it, successful restoration, pane-content backup, or periodic execution.
  Shared directories across selected servers are explicitly unattributed;
  fresh evidence cannot make a different server healthy. Future times and
  snapshots older than the server's current process generation are unavailable.
- Continuum's optional last-save timestamp is a launch marker only, not a
  successful-save receipt. Old last-snapshot mtimes can be intentional when
  content is unchanged; stale means stale evidence, not a diagnosed defect.

Reports never signal processes, suggest SIGCONT as a mask fix, run save
scripts, restart services, change tmux options or touch live snapshots. There
is no generic stopped-process recovery. Inspection has fixed target, byte,
path and fd-entry budgets; asynchronous tmux commands have deadlines. Kernel
filesystem operations cannot be promised preemptible during a kernel stall.
Failures remain typed unavailable, not zero/healthy. Existing doctor strict
requirements are unchanged; tmux observations are optional.

Fixtures must cover fresh/stale despite a live producer, stopped producers,
producer/server incarnation mismatches, missing provenance, shared evidence,
multiple sockets and unavailable evidence. Protect observable no-mutation
behavior and query-only authority, not incidental log wording or test totals.
Full package tests and strict Clippy remain required; source/tests only.

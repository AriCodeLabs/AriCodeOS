# Incident 0061: The CI stepped aside for nobody, and a red smoke reached develop

- Opened: 2026-11-06. Status: closed 2026-11-07.
- Found by: the CI container's log, then the smoke run before a merge.

## Evidence

The CI's pass stepped aside with no suite waiting:

```
[aricodeos-ci 2026-11-06T11:21:58Z] test-1cpu: stepped aside for another suite after 2m36s
```

The first version of the look took a moment's exclusive `flock` on the waiting file, and the
waiter held a shared one. Two causes made a look see a waiter that was not there:

1. Two jobs of a pass looking at once saw each other. A new host test failed on this.
2. After that was fixed with a mutex, the full unit run still failed: a child just forked by
   another thread inherits the look's open file, and so its lock, until it execs. Only the full
   run forks while the test looks, so the filtered run had passed.

## The process failure

The command that merged the mutex version ran the smoke and then pushed, without making the push
depend on it. Smoke was red, and 33b4501 reached `develop` with a test that fails. Nothing a user
runs was affected: the test only runs in the test tools. It stayed red for minutes.

## What was done

- Roles swapped: a waiter holds the waiting file exclusively and lookers share it. Lookers cannot
  see each other, nor a forked child of a looker. A test looks from eight threads.
- A waiter now polls the suite lock, retrying the waiting file, because a look may hold that for a
  moment.
- Merges push only if the checks before them passed: `&&`, never a new line.

## Twelve hours later

The container had taken up commit 27240b2 before the fix, and a pass keeps the code of the commit
it started on. That commit's look saw a waiter on nearly every try, so the pass stepped aside 451
times over 12 hours without finishing, on a host otherwise idle, and no newer commit was tested. The
report said nothing, because a pass that steps aside is not a failure. Found when looking at why
`develop` had no result; the container was restarted to take up the fixed code.

A pass now stops stepping aside after 5 tries in a row with no scenario done, and says so.

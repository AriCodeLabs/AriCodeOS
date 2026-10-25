# Incident 0038: init relaunched a service it had given up on, and leaked a failed launch's handles

- Opened: 2026-10-25. Status: fixed on `fix/init-supervisor`.
- Found by: the failure-pattern audit of 2026-10-25 (audit C, L4 and L5:
  error paths that skip the cleanup the main path does). Not seen on a
  machine.
- Environment: any boot; the paths run only when a launch fails or a
  service never becomes ready.

## Evidence

Reading `services/init/src/main.rs` and `libs/supervisor` at `df15084`:

- `start_timeout` called `sup.stop()` for a service whose process
  survived a kill, then `announce_death`, whose `sup.died()` set the state
  back to `Stopped` with a pending restart: the next generation started
  next to the old process, both claiming the same device. The same
  happened to any service retired with `Supervisor::stop` that died
  afterwards. The new host test
  `a_retired_or_abandoned_service_is_not_restarted` fails on that code
  (`died` after `stop` returned `Restart`).
- `start_driver` returned early, keeping the handles it held, when
  duplicating the service endpoint or spawning failed: the claim's
  configuration object stayed in init's table, so the device stayed
  claimed for good and no later generation could claim it. The extras
  (`SYSTEM`, the block endpoint), `start_fsd`'s duplicates,
  `spawn_block_client`'s and a test service's endpoint leaked the same
  way.

## Fix

- `Service::stop` marks the service retired: a later death is recorded,
  not restarted, until the next explicit start. `Service::abandon` makes
  a service that outlived a kill `Failed` whatever budget it had; init
  says `still running after a kill: failed, not relaunched`.
- `start_driver`, `start_fsd` and `spawn_block_client` hold what they
  made in a `Granted` guard, which closes it on every failure (closing
  the claim releases the device); the driver extras and the test
  service's endpoint are closed when their spawn fails.

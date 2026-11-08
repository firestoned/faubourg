<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0013: The install script's content is pinned by digest, and a change rolls out one host at a time

**Status:** Accepted · **Date:** 2026-11-08

## Context

A Host's install script comes from a ConfigMap or a Secret in the credentials namespace
(ADR-0005), on an allowlist of source names in the bootstrap policy's parameters,
`faubourg-bootstrap-policy` (A-8, ADR-0008). The controller re-bootstraps a Host when its
bootstrap inputs change: its `address`, its `ssh` and the *name* of its script source (ADR-0009
decision 6). It holds no access to ConfigMaps or Secrets, so it cannot see the script's
*content*. Two problems follow.

- **An edited script reaches no host, until it reaches one by accident.** Editing
  `faubourg-install` in place re-runs nothing. The next Job for any reason (a new Host, a
  changed address) places the new content on that one host. The fleet then runs different
  scripts, and nothing records which.
- **Whoever can write the script source is root on every host** (accepted risk R-1). The
  allowlist bounds *which* objects may be a script, not what they hold. R-1's own revisit
  condition is this design: "decide then whether the script's digest is pinned in A-8 too".

A naive fix makes the second problem worse. If the controller could read the script and
re-bootstrapped on every content change, one edit would run new root code on every host at
once, with no second person involved and no stop when the first host fails.

## Decision

1. **A-8 pins each script source's content.** Every `scriptSources` entry names a source, the
   key a Job mounts, and the sha256 of that key's content:
   `ConfigMap/<name>/<key>@sha256:<64 lowercase hex>` (or `Secret/...`). An entry without a
   digest is malformed, and the policy refuses every Job until it is fixed (fail closed). A
   source and key appear at most once: one pinned script per source at a time. The platform
   administrator writes A-8, as for the bootstrap image's digest; the script maintainer, who
   writes the ConfigMap, cannot pin their own change.
2. **The controller reads A-8, and only A-8.** Its Role in `faubourg-system` gains `get`,
   `list` and `watch` on the one ConfigMap `faubourg-bootstrap-policy` (`resourceNames`). It
   still holds no verb on any other ConfigMap and none on any Secret, and never reads a script.
   A Host whose script source and key have no pinned entry is `Failed`, reason
   `ScriptNotPinned`, and gets no Job. If A-8 is missing or malformed, every Host with
   `bootstrap.method: Script` is `Failed` with that reason.
3. **The pinned digest is a bootstrap input.** It joins `address`, `ssh` and the source in the
   inputs hash (ADR-0009 decision 6). Re-pinning a source therefore makes every Host using it
   due for a new Job, and so does nothing else about the script: an edit without a re-pin
   changes no input.
4. **The image refuses a script that is not the pinned one.** The Job carries the digest as a
   literal `FAUBOURG_SCRIPT_SHA256`. The admission policy admits a Job only if its script
   volume's source, its one key, and that value match an A-8 entry. Before opening any
   connection, the image hashes the mounted script and exits 66 (`ScriptUnpinned`) if it
   differs, a code `podFailurePolicy` fails at once, like 64 and 65. A source edited without a
   re-pin is never placed: the next Job for it fails before reaching a host, and says why.
5. **A re-bootstrap waits for a rollout slot.** A Host that has already completed a bootstrap
   (recorded in its status) and whose inputs change gets its new Job only when a slot is free:
   - at most `--max-rebootstraps` such Jobs run at once, across the cluster (default 1);
   - while any Host's current re-bootstrap Job has failed, no new re-bootstrap starts: the
     rollout halts until that Host's inputs change again or an operator deletes the failed Job.

   A first bootstrap is not limited: a host with nothing on it has nothing to lose. A waiting
   Host keeps its phase and gets condition `BootstrapCurrent=False`, reason `RolloutWaiting`
   or `RolloutHalted`; it is `True` once its current Job has succeeded. The controller counts
   Jobs by its own component label in the credentials namespace, which it already lists.
6. **The default script stays a name.** `--default-script` (`FAUBOURG_DEFAULT_SCRIPT`) names a
   source and key, as now; its digest comes from A-8, like any Host's.

## Alternatives considered

- **Let the controller read the script source.** It could hash the content and re-run on
  change. Rejected: a Secret source can hold secrets, which ADR-0008 keeps from the
  controller; and every edit would roll out with no second party, which leaves R-1 as it is.
- **The digest on each Host.** Explicit per host, but a fleet-wide script change becomes one
  edit per Host, and the inventory role would choose which pinned content runs. A-8 already
  bounds what may run; it is the one place to pin it.
- **Immutable, content-named ConfigMaps** (a hash suffix, as kustomize's generator makes).
  This works today, since a new name is a new input. But each name must be added to the
  allowlist, a new default script means restarting the controller, nothing checks the content
  against anything, and nothing limits the rollout. It stays possible: such a source is
  pinned like any other.
- **Periodic drift checks**, a Job per host on a schedule comparing the host's copy with the
  source. That would also catch tampering on the host, but it is a different question
  (detection, not change). Not decided here.

## Consequences

- R-1 narrows: new root code on the fleet takes the script maintainer *and* the platform
  administrator, and it reaches one host at a time, stopping at the first failure.
- Upgrading to this version changes every Host's inputs hash, so each bootstrapped host
  re-runs its install once, one at a time. The install is idempotent, and a host already
  carrying the pinned script only re-runs it.
- The install file's `scriptSources` ships as a placeholder,
  `ConfigMap/faubourg-install/install.sh@sha256:<digest>`, which matches no digest the image
  will compute, so nothing bootstraps until the administrator pins the real script. The
  installing guide shows how: the sha256 of the exact file the ConfigMap was created from.
- Rolling back is re-pinning the previous digest, with the previous content in the source:
  the same rollout, in the other direction.
- The controller now reads one ConfigMap. ADR-0008's "no ConfigMap access" becomes "no
  ConfigMap access but A-8, read-only". `vap-test` checks it can read A-8 and nothing else.
- `--max-rebootstraps` is cluster-wide. A per-zone limit, or a percentage, is roadmap 02's to
  decide with zone health; until then a large fleet rolls out slowly by design.
- With one replica (ADR-0009 decision 1) the count is exact. Leader election must keep it so:
  only the leader creates Jobs.

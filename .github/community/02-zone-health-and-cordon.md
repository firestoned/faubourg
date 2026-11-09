<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# 02: Zone health and cordon

- [x] Zone controller: `hosts`, `readyHosts`, `cordonedHosts`, `Ready` and its reasons, `StorageModeUnsupported` among them (ADR-0015)
- [x] Cordon label on Providers (`faubourg.firestoned.io/cordoned`, ADR-0003 decision 5)
- [x] banlieue's scheduler skips cordoned Providers (banlieue ADR-0124, v0.5.3: `banlieue.io/unschedulable`, placed VMs stay); faubourg sets that label with its cordon and reserves it in zones (R-4 closed)
- [x] Zone-scoped placement documented for banlieue users (`providerSelector` on `faubourg.firestoned.io/zone`, and the cordon-respecting form), in `docs/concepts.md`

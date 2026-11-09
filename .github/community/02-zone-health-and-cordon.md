<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# 02: Zone health and cordon

- [x] Zone controller: `hosts`, `readyHosts`, `cordonedHosts`, `Ready` and its reasons, `StorageModeUnsupported` among them (ADR-0015)
- [x] Cordon label on Providers (`faubourg.firestoned.io/cordoned`, ADR-0003 decision 5)
- [ ] Propose that banlieue's scheduler skip cordoned Providers by default, or a cordon field (R-4); until then a VM opts in with `DoesNotExist`
- [x] Zone-scoped placement documented for banlieue users (`providerSelector` on `faubourg.firestoned.io/zone`, and the cordon-respecting form), in `docs/concepts.md`

<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Publishing the install script

Edit the settings block of [`host/install.sh`](host/install.sh) (the internal repository
URL, the pinned banlieue version and its sha256), then publish it in the credentials
namespace, where the bootstrap Job runs (ADR-0005), and list it in the admission policy's
`scriptSources` (ADR-0008, [`deploy/admission/`](../deploy/admission/)):

```sh
kubectl -n faubourg-credentials create configmap faubourg-install \
  --from-file=install.sh=examples/host/install.sh --dry-run=client -o yaml | kubectl apply -f -
```

Hosts prepared out of band get the same two files by whatever builds them:
`/opt/faubourg/install.sh` (`root:root 0755`) and `/etc/sudoers.d/faubourg`
([`host/sudoers-faubourg`](host/sudoers-faubourg), `root:root 0440`).

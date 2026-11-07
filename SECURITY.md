<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Security Policy

faubourg is pre-1.0 and under active development. Only `main` receives security fixes.

## Reporting a Vulnerability

Use GitHub [private vulnerability reporting](https://github.com/firestoned/faubourg/security/advisories/new).
Do not open a public issue for a security finding, and do not record specific unremediated
findings in the public [threat model](docs/security/threat-model.md).

## What is in scope

Anything that lets a caller run code as root on a host other than through the root-owned
install script; read or use another host's SSH credential; change which zone a host belongs
to without writing its Host; or make the controller read a credential.

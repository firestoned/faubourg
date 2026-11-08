<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Installing faubourg

!!! warning "Pre-1.0"
    faubourg has been run against one live Cloud Hypervisor host, adopted and then bootstrapped,
    and its API may still change. Read the accepted risks in the
    [threat model](../security/threat-model.md) before installing it on a fleet you depend on.

A release ships one install file, `faubourg-<version>.yaml`, and its Cosign signature bundle
([ADR-0012](../adr/0012-images-signed-and-published-by-digest.md)). The file holds the CRDs,
the RBAC, the admission policies and their parameters, and the controller Deployment, with both
images pinned by digest. The bootstrap image's digest is in two places, the admission policy's
parameters and the controller's environment, written by the same render.

## Before you start

- Kubernetes 1.30 or later (ValidatingAdmissionPolicy), with kube-controller-manager running
  with `--use-service-account-credentials`
  ([ADR-0008](../adr/0008-bootstrap-jobs-constrained-by-admission-policy.md)).
- banlieue installed, with an `External` ProviderClass for Cloud Hypervisor
  ([ADR-0011](../adr/0011-the-hosts-provider-and-its-identity.md)).

## 1. Verify the release

Check the install file's signature: it was signed by this repository's release workflow, for
that release tag.

```sh
VERSION=v0.1.0
gh release download "$VERSION" -R firestoned/faubourg -p "faubourg-$VERSION.yaml*"
cosign verify-blob "faubourg-$VERSION.yaml" \
  --bundle "faubourg-$VERSION.yaml.bundle" \
  --certificate-identity "https://github.com/firestoned/faubourg/.github/workflows/build.yaml@refs/tags/$VERSION" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Then each image it names, by digest:

```sh
for image in $(grep -oE 'ghcr.io/firestoned/faubourg-(controller|bootstrap)@sha256:[0-9a-f]{64}' \
                 "faubourg-$VERSION.yaml" | sort -u); do
  cosign verify "$image" \
    --certificate-identity "https://github.com/firestoned/faubourg/.github/workflows/build.yaml@refs/tags/$VERSION" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com >/dev/null && echo "ok $image"
  gh attestation verify "oci://$image" -R firestoned/faubourg
done
```

A `main` build is signed too, but with `@refs/heads/main` in its certificate: the identity above
accepts release builds only.

## 2. Set what only you know

Edit the install file before applying it:

- `FAUBOURG_PROVIDER_API_SERVER` in the controller Deployment: the `https://` address your hosts
  use to reach the API server. The controller refuses to start without it.
- In `faubourg-bootstrap-policy` (namespace `faubourg-system`): `scriptSources`, the script
  objects that may be mounted, and `apiAudiences`, your API server's token audiences. Leave
  `image` alone: it is the bootstrap digest the controller uses too.

  The shipped `apiAudiences` is kubeadm's and kind's. k0s, for one, issues
  `https://kubernetes.default.svc`. Read yours from a short-lived token; if it is wrong, every
  provider token is refused:

  ```sh
  kubectl -n default create token default --duration=10m \
    | cut -d. -f2 | tr '_-' '/+' | base64 -d 2>/dev/null | jq -c .aud
  ```
- `banlieue-system`, `faubourg-credentials` and `faubourg-system`, if yours differ, in every
  place the file names them.

## 3. Apply it

```sh
kubectl apply -f "faubourg-$VERSION.yaml"
kubectl -n faubourg-system rollout status deploy/faubourg-controller
```

Then create the install script's ConfigMap and the hosts' Secrets in `faubourg-credentials`
([SSH credentials](ssh-credentials.md), [Preparing a host](preparing-a-host.md)), an
`AvailabilityZone`, and your Hosts.

## Building it yourself

`make controller-image bootstrap-image` builds both images with podman. Push them, then render
your own install file from their digests:

```sh
CONTROLLER_IMAGE=registry.internal:5000/faubourg-controller@sha256:<digest> \
BOOTSTRAP_IMAGE=registry.internal:5000/faubourg-bootstrap@sha256:<digest> \
VERSION=dev make release-manifests        # writes release/faubourg-dev.yaml
```

The render refuses an image that is not pinned by digest.

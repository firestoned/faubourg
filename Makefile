# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# All CI logic lives here (rules/github-workflows.md): workflows install tools
# and call these targets, and every target runs identically locally.

.PHONY: all help fmt fmt-check lint test deny check crds crds-check calm-validate calm-diagrams \
	ci-code-changed dco-check dco-check-test check-infra-identifiers check-infra-identifiers-test vap-test controller-test bootstrap-image bootstrap-test e2e-test \
	docs docs-serve docs-clean

CALM_CLI_VERSION  ?= 1.37.0
CALM_ARCH         := docs/architecture/calm/architecture.json
CALM_TEMPLATES    := docs/architecture/calm/templates/mermaid
CALM_DIAGRAMS_OUT := docs/architecture/diagrams
CRD_DIR           := deploy/crds

all: check

help: ## Show this help
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_.-]+:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

# ----- Quality gate ----------------------------------------------------------

fmt: ## Format the workspace
	cargo fmt --all

fmt-check: ## Check formatting without writing
	cargo fmt --all -- --check

lint: ## Clippy, warnings are errors (pedantic is on via workspace lints)
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test: ## Run the whole test suite
	cargo test --workspace --all-features

deny: ## License / advisory / source checks
	@command -v cargo-deny >/dev/null 2>&1 || { echo "Error: cargo-deny not found (cargo install cargo-deny --locked)"; exit 1; }
	cargo deny check

check: fmt lint test deny ## The pre-commit gate: fmt + clippy + test + deny

# ----- CRDs ------------------------------------------------------------------

crds: ## Regenerate deploy/crds from the Rust types (ADR-0003)
	cargo run -q -p faubourg-api --bin crdgen -- --out-dir $(CRD_DIR)

crds-check: crds ## Fail when deploy/crds is stale against the Rust types
	@test -z "$$(git status --porcelain -- $(CRD_DIR))" || { git status --short -- $(CRD_DIR); echo "Error: $(CRD_DIR) is stale, run 'make crds' and commit the result"; exit 1; }

# ----- Architecture (ADD: ADR, then CALM, then TDD) ---------------------------

calm-validate: ## Validate the CALM architecture against the meta-schema
	@command -v npx >/dev/null 2>&1 || { echo "Error: npx not found. Install Node.js"; exit 1; }
	npx --yes @finos/calm-cli@$(CALM_CLI_VERSION) validate -a $(CALM_ARCH) -f pretty

calm-diagrams: ## Render CALM Mermaid diagrams into docs/architecture/diagrams
	@command -v npx >/dev/null 2>&1 || { echo "Error: npx not found. Install Node.js"; exit 1; }
	@mkdir -p $(CALM_DIAGRAMS_OUT)
	@rm -f $(CALM_DIAGRAMS_OUT)/system.md $(CALM_DIAGRAMS_OUT)/flows.md
	npx --yes @finos/calm-cli@$(CALM_CLI_VERSION) template -a $(CALM_ARCH) -d $(CALM_TEMPLATES) -o $(CALM_DIAGRAMS_OUT)
	@for f in $(CALM_DIAGRAMS_OUT)/*.hbs; do [ -e "$$f" ] || continue; mv "$$f" "$${f%.hbs}"; done
	@echo "CALM diagrams written to $(CALM_DIAGRAMS_OUT)/"

# ----- CI gating and repository hygiene --------------------------------------

# Paths whose change needs the Rust jobs in build.yaml. A workflow-level `paths:`
# filter cannot coexist with a required aggregator check (a workflow that never
# triggers reports no context, so the PR blocks forever), so the filter lives
# here and the `changes` job applies it per job. Keep in sync with build.yaml.
CI_CODE_PATHS_RE ?= ^(crates/|deploy/|scripts/vap-test\.sh$$|scripts/controller-test\.sh$$|scripts/bootstrap-test\.sh$$|scripts/e2e-test\.sh$$|examples/host/|images/|test/sshd/|\.containerignore$$|Cargo\.toml$$|Cargo\.lock$$|deny\.toml$$|rust-toolchain\.toml$$|rustfmt\.toml$$|Makefile$$|\.github/workflows/build\.yaml$$)

ci-code-changed: ## Print true/false: does the diff against BASE_REF touch build-relevant paths?
	@base="$${BASE_REF:-origin/main}"; \
	if ! git rev-parse --verify --quiet "$$base" >/dev/null; then echo "true"; exit 0; fi; \
	if ! changed="$$(git diff --name-only "$$base...HEAD" 2>/dev/null)"; then echo "true"; exit 0; fi; \
	if printf '%s\n' "$$changed" | grep -Eq '$(CI_CODE_PATHS_RE)'; then echo "true"; else echo "false"; fi

DCO_RANGE ?= origin/main..HEAD

dco-check: ## Fail unless every commit in DCO_RANGE (default origin/main..HEAD) is signed off by its author
	@scripts/dco-check.sh $(DCO_RANGE)

dco-check-test: ## Test scripts/dco-check.sh against throwaway repositories
	@scripts/dco-check-test.sh

check-infra-identifiers: ## Fail if a tracked file names real infrastructure (plus ~/.config/faubourg/infra-denylist)
	@scripts/check-infra-identifiers.sh

check-infra-identifiers-test: ## Test scripts/check-infra-identifiers.sh against throwaway repositories
	@scripts/check-infra-identifiers-test.sh

# ----- Admission policy (ADR-0008) -------------------------------------------

vap-test: ## The bootstrap admission policy and controller RBAC against a real API server (kind)
	@scripts/vap-test.sh

# ----- Host controller (ADR-0009) --------------------------------------------

controller-test: ## The Host controller, with its own RBAC, against a real API server (kind)
	@scripts/controller-test.sh

# ----- Bootstrap image (ADR-0010) --------------------------------------------

BOOTSTRAP_IMAGE ?= localhost/faubourg-bootstrap:dev

bootstrap-image: ## Build the bootstrap image with podman (one static binary, no shell)
	podman build -f images/bootstrap/Containerfile -t $(BOOTSTRAP_IMAGE) .

bootstrap-test: ## The bootstrap image against a real OpenSSH server (podman)
	@scripts/bootstrap-test.sh

# ----- End to end (ADR-0009 … ADR-0011) --------------------------------------

e2e-test: ## A Host bootstrapped end to end in kind: real image, policies, controller; banlieue simulated
	@scripts/e2e-test.sh

# ----- Documentation (MkDocs Material) ---------------------------------------

POETRY_HINT := Install: curl -sSL https://install.python-poetry.org | python3 -

docs: ## Build the MkDocs site into site/ (strict: a broken link fails the build)
	@command -v poetry >/dev/null 2>&1 || { echo "Error: Poetry not found. $(POETRY_HINT)"; exit 1; }
	@poetry install --no-interaction --quiet
	@poetry run mkdocs build --strict
	@echo "Documentation built at site/index.html"

docs-serve: ## Serve docs locally with live reload at http://127.0.0.1:8000
	@command -v poetry >/dev/null 2>&1 || { echo "Error: Poetry not found. $(POETRY_HINT)"; exit 1; }
	@poetry install --no-interaction --quiet
	@poetry run mkdocs serve --livereload

docs-clean: ## Remove docs build artefacts and the venv
	@rm -rf site/ .venv/

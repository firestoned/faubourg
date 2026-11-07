// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The bootstrap Job builder (ADR-0005 decision 5, ADR-0008), as a pure function.
//!
//! The Job runs in the credentials namespace, mounts only its Host's Secret(s) and the install
//! script source, and has exactly the shape `deploy/admission/bootstrap-policy.yaml` admits:
//! one `bootstrap` container on a digest-pinned image with no command or arguments, literal
//! environment only, read-only volumes, no service account token, and the restricted security
//! profile. The policy is the enforcement; this builder is what makes the controller's own
//! Jobs pass it.
//!
//! The Job carries Secret *names*, never Secret values.

use std::collections::BTreeMap;

use faubourg_api::host::{
    BootstrapMethod, Host, HostKeyPolicy, HostStatus, ScriptRef, ScriptSourceKind, SshSpec,
};
use faubourg_api::validation::Invalid;
use faubourg_api::{GROUP, LABEL_HOST, VERSION};
use k8s_openapi::api::batch::v1::{
    Job, JobSpec, PodFailurePolicy, PodFailurePolicyOnExitCodesRequirement, PodFailurePolicyRule,
};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMapVolumeSource, Container, EnvVar, KeyToPath, PodSecurityContext, PodSpec,
    PodTemplateSpec, ResourceRequirements, SeccompProfile, SecretVolumeSource, SecurityContext,
    Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};

/// Name of the one container.
pub const CONTAINER: &str = "bootstrap";
/// Volume holding the scoped run credential.
pub const VOLUME_CREDENTIALS: &str = "credentials";
/// Volume holding the optional placement credential.
pub const VOLUME_PLACEMENT: &str = "placement";
/// Volume holding the install script.
pub const VOLUME_SCRIPT: &str = "script";
/// Where the run credential is mounted.
pub const MOUNT_CREDENTIALS: &str = "/var/run/faubourg/credentials";
/// Where the placement credential is mounted.
pub const MOUNT_PLACEMENT: &str = "/var/run/faubourg/placement";
/// Where the install script is mounted; the file inside is always `install.sh`.
pub const MOUNT_SCRIPT: &str = "/var/run/faubourg/script";

/// Env: the Host's name, for logs.
pub const ENV_HOST: &str = "FAUBOURG_HOST";
/// Env: the address to reach over SSH.
pub const ENV_ADDRESS: &str = "FAUBOURG_ADDRESS";
/// Env: the SSH port.
pub const ENV_PORT: &str = "FAUBOURG_PORT";
/// Env: `Strict` or `TrustOnFirstUse`. A Secret with `knownHosts` is always strict
/// (ADR-0008 decision 5).
pub const ENV_HOST_KEY_POLICY: &str = "FAUBOURG_HOST_KEY_POLICY";
/// Env: the fingerprint pinned by an earlier `TrustOnFirstUse` run, when there is one.
pub const ENV_ACCEPTED_HOST_KEY: &str = "FAUBOURG_ACCEPTED_HOST_KEY";
/// Env: `true` to remove the placement key from the host once placement succeeds.
pub const ENV_REVOKE_PLACEMENT: &str = "FAUBOURG_REVOKE_PLACEMENT";

/// Exit code: the on-host script differs from the source and there is no placement
/// credential (ADR-0005 decision 4). Fails the Job at once, without retries.
pub const EXIT_SCRIPT_DRIFT: i32 = 64;
/// Exit code: the host key was refused. Fails the Job at once: a retry cannot fix it.
pub const EXIT_HOST_KEY_REFUSED: i32 = 65;

/// Label: what manages the Job.
pub const LABEL_MANAGED_BY: &str = "app.kubernetes.io/managed-by";
/// Label: which part of faubourg the Job is.
pub const LABEL_COMPONENT: &str = "app.kubernetes.io/component";
/// Annotation: the Host generation the Job bootstraps.
pub const ANNOTATION_GENERATION: &str = "faubourg.firestoned.io/generation";

/// Retries for a transient failure (an unreachable host, a dropped connection).
pub const BACKOFF_LIMIT: i32 = 2;
/// Upper bound on one bootstrap, downloads included.
pub const ACTIVE_DEADLINE_SECONDS: i64 = 3600;
/// UID and GID the container runs as (the distroless `nonroot` user).
pub const RUN_AS: i64 = 65532;
/// Longest Job name: it is also the `job-name` label value on the Job's pods.
pub const MAX_JOB_NAME: usize = 63;

/// How the controller is configured to build bootstrap Jobs.
#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapConfig {
    /// The credentials namespace: where the Secrets, the script source and the Job live.
    pub namespace: String,
    /// The bootstrap image, pinned by digest (`repo@sha256:<64 hex>`).
    pub image: String,
    /// The bootstrap pods' `ServiceAccount`, which holds no RBAC.
    pub service_account: String,
    /// The script source for a Host with no `bootstrap.scriptRef`.
    pub default_script: Option<ScriptRef>,
}

/// Why no Job was built. Never carries secret material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobError {
    /// `bootstrap.method: None`: an adopted host has no Job.
    NotScript,
    /// The Host's spec is refused.
    InvalidSpec(Invalid),
    /// The Host has no name, uid or generation yet (it was not read from the API server).
    NotPersisted(&'static str),
    /// The Host's name is longer than a label value allows (63).
    NameTooLong,
    /// Neither the Host nor the controller configuration names a script source.
    NoScriptSource,
    /// The image is not pinned by a sha256 digest.
    ImageNotPinned,
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotScript => f.write_str("bootstrap method None has no Job"),
            Self::InvalidSpec(i) => write!(f, "invalid Host spec: {i}"),
            Self::NotPersisted(field) => write!(f, "the Host has no {field}"),
            Self::NameTooLong => f.write_str("the Host name is longer than 63 characters"),
            Self::NoScriptSource => {
                f.write_str("no bootstrap.scriptRef and no default script configured")
            }
            Self::ImageNotPinned => f.write_str("the bootstrap image is not pinned by digest"),
        }
    }
}

impl std::error::Error for JobError {}

/// The Job name for a Host generation: `bootstrap-<host>-g<generation>`, shortened with a
/// stable hash of the Host name when that would exceed [`MAX_JOB_NAME`].
pub fn job_name(host: &str, generation: i64) -> String {
    let suffix = format!("-g{generation}");
    let full = format!("bootstrap-{host}{suffix}");
    if full.len() <= MAX_JOB_NAME {
        return full;
    }
    // A Host name is a DNS subdomain, so ASCII: slicing by byte is slicing by character.
    let hash = format!("-{:08x}", fnv1a32(host.as_bytes()));
    let room = MAX_JOB_NAME - "bootstrap-".len() - hash.len() - suffix.len();
    let head = host[..room.min(host.len())].trim_end_matches(['-', '.']);
    format!("bootstrap-{head}{hash}{suffix}")
}

/// FNV-1a, 32 bits. Stable across Rust releases, unlike `DefaultHasher`, so a Job name never
/// changes under an upgraded controller.
fn fnv1a32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
    })
}

/// `repo@sha256:<64 lowercase hex>`, with a non-empty repository.
fn is_digest_pinned(image: &str) -> bool {
    image.split_once("@sha256:").is_some_and(|(repo, digest)| {
        !repo.is_empty()
            && digest.len() == 64
            && digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// Builds the bootstrap Job for the Host's current generation.
pub fn bootstrap_job(host: &Host, cfg: &BootstrapConfig) -> Result<Job, JobError> {
    let spec = &host.spec;
    if spec.bootstrap.method != BootstrapMethod::Script {
        return Err(JobError::NotScript);
    }
    spec.validate().map_err(JobError::InvalidSpec)?;
    let name = host
        .metadata
        .name
        .as_deref()
        .ok_or(JobError::NotPersisted("name"))?;
    let uid = host
        .metadata
        .uid
        .as_deref()
        .ok_or(JobError::NotPersisted("uid"))?;
    let generation = host
        .metadata
        .generation
        .ok_or(JobError::NotPersisted("generation"))?;
    if name.len() > 63 {
        return Err(JobError::NameTooLong);
    }
    if !is_digest_pinned(&cfg.image) {
        return Err(JobError::ImageNotPinned);
    }
    let script = spec
        .bootstrap
        .script_ref
        .as_ref()
        .or(cfg.default_script.as_ref())
        .ok_or(JobError::NoScriptSource)?;
    // validate() refuses Script without ssh, so this cannot fail; fail closed regardless.
    let ssh = spec.ssh.as_ref().ok_or(JobError::NotPersisted("ssh"))?;

    let labels: BTreeMap<String, String> = [
        (LABEL_HOST, name),
        (LABEL_COMPONENT, CONTAINER),
        (LABEL_MANAGED_BY, faubourg_api::PROJECT),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let env = environment(name, &spec.address, ssh, host.status.as_ref());

    let mut volumes = vec![secret_volume(VOLUME_CREDENTIALS, &ssh.secret_ref.name)];
    let mut mounts = vec![read_only(VOLUME_CREDENTIALS, MOUNT_CREDENTIALS)];
    if let Some(p) = &ssh.placement_secret_ref {
        volumes.push(secret_volume(VOLUME_PLACEMENT, &p.name));
        mounts.push(read_only(VOLUME_PLACEMENT, MOUNT_PLACEMENT));
    }
    volumes.push(script_volume(script));
    mounts.push(read_only(VOLUME_SCRIPT, MOUNT_SCRIPT));

    let pod = pod_spec(
        container(&cfg.image, env, mounts),
        volumes,
        &cfg.service_account,
    );

    Ok(Job {
        metadata: ObjectMeta {
            name: Some(job_name(name, generation)),
            namespace: Some(cfg.namespace.clone()),
            labels: Some(labels.clone()),
            annotations: Some(BTreeMap::from([(
                ANNOTATION_GENERATION.to_string(),
                generation.to_string(),
            )])),
            owner_references: Some(vec![OwnerReference {
                api_version: format!("{GROUP}/{VERSION}"),
                kind: "Host".into(),
                name: name.into(),
                uid: uid.into(),
                controller: Some(true),
                block_owner_deletion: None,
            }]),
            ..ObjectMeta::default()
        },
        spec: Some(job_spec(labels, pod)),
        status: None,
    })
}

/// The container's environment: connection settings only, all literal values.
fn environment(
    name: &str,
    address: &str,
    ssh: &SshSpec,
    status: Option<&HostStatus>,
) -> Vec<EnvVar> {
    let mut env = vec![
        literal(ENV_HOST, name),
        literal(ENV_ADDRESS, address),
        literal(ENV_PORT, &ssh.port.to_string()),
        literal(
            ENV_HOST_KEY_POLICY,
            match ssh.host_key_policy {
                HostKeyPolicy::Strict => "Strict",
                HostKeyPolicy::TrustOnFirstUse => "TrustOnFirstUse",
            },
        ),
        literal(
            ENV_REVOKE_PLACEMENT,
            if ssh.revoke_after_bootstrap {
                "true"
            } else {
                "false"
            },
        ),
    ];
    if let Some(fp) = status.and_then(|s| s.accepted_host_key.as_deref()) {
        env.push(literal(ENV_ACCEPTED_HOST_KEY, fp));
    }
    env
}

/// The one `bootstrap` container: no command, read-only root, every capability dropped.
fn container(image: &str, env: Vec<EnvVar>, mounts: Vec<VolumeMount>) -> Container {
    Container {
        name: CONTAINER.into(),
        image: Some(image.into()),
        image_pull_policy: Some("IfNotPresent".into()),
        env: Some(env),
        volume_mounts: Some(mounts),
        security_context: Some(SecurityContext {
            run_as_non_root: Some(true),
            allow_privilege_escalation: Some(false),
            privileged: Some(false),
            read_only_root_filesystem: Some(true),
            capabilities: Some(Capabilities {
                drop: Some(vec!["ALL".into()]),
                add: None,
            }),
            ..SecurityContext::default()
        }),
        resources: Some(ResourceRequirements {
            requests: Some(quantities(&[("cpu", "50m"), ("memory", "64Mi")])),
            limits: Some(quantities(&[("memory", "256Mi")])),
            ..ResourceRequirements::default()
        }),
        termination_message_policy: Some("File".into()),
        ..Container::default()
    }
}

/// The pod: no token, no host namespaces, never restarted, the restricted profile.
fn pod_spec(container: Container, volumes: Vec<Volume>, service_account: &str) -> PodSpec {
    PodSpec {
        containers: vec![container],
        volumes: Some(volumes),
        restart_policy: Some("Never".into()),
        service_account_name: Some(service_account.into()),
        automount_service_account_token: Some(false),
        enable_service_links: Some(false),
        security_context: Some(PodSecurityContext {
            run_as_non_root: Some(true),
            run_as_user: Some(RUN_AS),
            run_as_group: Some(RUN_AS),
            seccomp_profile: Some(SeccompProfile {
                type_: "RuntimeDefault".into(),
                ..SeccompProfile::default()
            }),
            ..PodSecurityContext::default()
        }),
        ..PodSpec::default()
    }
}

/// Retries for transient failures; drift and a refused host key fail at once.
fn job_spec(labels: BTreeMap<String, String>, pod: PodSpec) -> JobSpec {
    JobSpec {
        backoff_limit: Some(BACKOFF_LIMIT),
        active_deadline_seconds: Some(ACTIVE_DEADLINE_SECONDS),
        pod_failure_policy: Some(PodFailurePolicy {
            rules: vec![PodFailurePolicyRule {
                action: "FailJob".into(),
                on_exit_codes: Some(PodFailurePolicyOnExitCodesRequirement {
                    container_name: Some(CONTAINER.into()),
                    operator: "In".into(),
                    values: vec![EXIT_SCRIPT_DRIFT, EXIT_HOST_KEY_REFUSED],
                }),
                on_pod_conditions: None,
            }],
        }),
        template: PodTemplateSpec {
            metadata: Some(ObjectMeta {
                labels: Some(labels),
                ..ObjectMeta::default()
            }),
            spec: Some(pod),
        },
        ..JobSpec::default()
    }
}

fn literal(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.into(),
        value: Some(value.into()),
        value_from: None,
    }
}

fn read_only(volume: &str, path: &str) -> VolumeMount {
    VolumeMount {
        name: volume.into(),
        mount_path: path.into(),
        read_only: Some(true),
        ..VolumeMount::default()
    }
}

fn secret_volume(volume: &str, secret: &str) -> Volume {
    Volume {
        name: volume.into(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(secret.into()),
            default_mode: Some(0o400),
            ..SecretVolumeSource::default()
        }),
        ..Volume::default()
    }
}

fn script_volume(r: &ScriptRef) -> Volume {
    let items = Some(vec![KeyToPath {
        key: r.key.clone(),
        path: "install.sh".into(),
        mode: None,
    }]);
    let mut v = Volume {
        name: VOLUME_SCRIPT.into(),
        ..Volume::default()
    };
    match r.kind {
        ScriptSourceKind::ConfigMap => {
            v.config_map = Some(ConfigMapVolumeSource {
                name: r.name.clone(),
                items,
                default_mode: Some(0o444),
                ..ConfigMapVolumeSource::default()
            });
        }
        ScriptSourceKind::Secret => {
            v.secret = Some(SecretVolumeSource {
                secret_name: Some(r.name.clone()),
                items,
                default_mode: Some(0o400),
                ..SecretVolumeSource::default()
            });
        }
    }
    v
}

fn quantities(pairs: &[(&str, &str)]) -> BTreeMap<String, Quantity> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), Quantity((*v).to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use faubourg_api::host::{BootstrapSpec, DEFAULT_SCRIPT_KEY, HostSpec, SecretName};

    const IMAGE: &str = "ghcr.io/firestoned/faubourg-bootstrap@sha256:\
                         0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn cfg() -> BootstrapConfig {
        BootstrapConfig {
            namespace: "faubourg-credentials".into(),
            image: IMAGE.into(),
            service_account: "faubourg-bootstrap".into(),
            default_script: Some(ScriptRef {
                kind: ScriptSourceKind::ConfigMap,
                name: "faubourg-install".into(),
                key: DEFAULT_SCRIPT_KEY.into(),
            }),
        }
    }

    fn host() -> Host {
        let mut h = Host::new(
            "hv01",
            HostSpec {
                zone_ref: "az1".into(),
                address: "hv01.home.example".into(),
                ssh: Some(SshSpec {
                    port: 2222,
                    secret_ref: SecretName {
                        name: "hv01-ssh".into(),
                    },
                    placement_secret_ref: Some(SecretName {
                        name: "hv01-ssh-placement".into(),
                    }),
                    host_key_policy: HostKeyPolicy::Strict,
                    revoke_after_bootstrap: true,
                }),
                bootstrap: BootstrapSpec::default(),
                cordoned: false,
            },
        );
        h.metadata.uid = Some("8f6b8a52-0000-4000-8000-000000000001".into());
        h.metadata.generation = Some(3);
        h
    }

    fn build(h: &Host) -> Job {
        bootstrap_job(h, &cfg()).expect("a Job")
    }

    fn pod(j: &Job) -> &PodSpec {
        j.spec.as_ref().unwrap().template.spec.as_ref().unwrap()
    }

    fn container(j: &Job) -> &Container {
        &pod(j).containers[0]
    }

    fn volumes(j: &Job) -> &[Volume] {
        pod(j).volumes.as_deref().unwrap_or_default()
    }

    fn volume<'a>(j: &'a Job, name: &str) -> Option<&'a Volume> {
        volumes(j).iter().find(|v| v.name == name)
    }

    fn env(j: &Job) -> BTreeMap<String, Option<String>> {
        container(j)
            .env
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|e| (e.name.clone(), e.value.clone()))
            .collect()
    }

    // ----- names --------------------------------------------------------------------------

    #[test]
    fn a_short_host_gets_a_readable_name() {
        assert_eq!(job_name("hv01", 3), "bootstrap-hv01-g3");
    }

    #[test]
    fn a_long_host_name_is_shortened_stably_and_stays_a_label_value() {
        let long = format!("{}.rack-a.home.example", "h".repeat(50));
        let a = job_name(&long, 12);
        assert!(a.len() <= MAX_JOB_NAME, "{a} is {} long", a.len());
        assert_eq!(a, job_name(&long, 12), "stable");
        assert!(a.ends_with("-g12"), "{a}");
        assert!(faubourg_api::validation::is_dns_subdomain(&a), "{a}");
        let b = job_name(&format!("{}.rack-b.home.example", "h".repeat(50)), 12);
        assert_ne!(a, b, "two hosts sharing a long prefix get different Jobs");
    }

    #[test]
    fn each_generation_gets_its_own_job() {
        assert_ne!(job_name("hv01", 3), job_name("hv01", 4));
    }

    #[test]
    fn a_truncated_name_never_ends_in_a_separator() {
        // Valid Host names whose cut point lands on, or just after, a `-` or a `.`.
        for len in 30..50 {
            let name = format!("{}-b.c{}", "a".repeat(len), "d".repeat(20));
            assert!(faubourg_api::validation::is_dns_subdomain(&name), "{name}");
            let j = job_name(&name, 1);
            assert!(faubourg_api::validation::is_dns_subdomain(&j), "{j}");
            assert!(
                !j.contains("-.") && !j.contains(".-") && !j.contains("--g"),
                "{j}"
            );
        }
    }

    // ----- refusals (fail closed) ---------------------------------------------------------

    #[test]
    fn an_adopted_host_has_no_job() {
        let mut h = host();
        h.spec.bootstrap.method = BootstrapMethod::None;
        assert_eq!(bootstrap_job(&h, &cfg()), Err(JobError::NotScript));
    }

    #[test]
    fn an_invalid_spec_has_no_job() {
        let mut h = host();
        h.spec.address = "-oProxyCommand=sh".into();
        assert!(matches!(
            bootstrap_job(&h, &cfg()),
            Err(JobError::InvalidSpec(_))
        ));
    }

    #[test]
    fn a_host_not_read_from_the_api_server_has_no_job() {
        let mut h = host();
        h.metadata.uid = None;
        assert_eq!(
            bootstrap_job(&h, &cfg()),
            Err(JobError::NotPersisted("uid"))
        );
        let mut h = host();
        h.metadata.generation = None;
        assert_eq!(
            bootstrap_job(&h, &cfg()),
            Err(JobError::NotPersisted("generation"))
        );
        let mut h = host();
        h.metadata.name = None;
        assert_eq!(
            bootstrap_job(&h, &cfg()),
            Err(JobError::NotPersisted("name"))
        );
    }

    #[test]
    fn a_host_name_too_long_for_a_label_has_no_job() {
        let mut h = host();
        h.metadata.name = Some("h".repeat(64));
        assert_eq!(bootstrap_job(&h, &cfg()), Err(JobError::NameTooLong));
    }

    #[test]
    fn no_script_source_has_no_job() {
        let c = BootstrapConfig {
            default_script: None,
            ..cfg()
        };
        assert_eq!(bootstrap_job(&host(), &c), Err(JobError::NoScriptSource));
    }

    #[test]
    fn an_image_not_pinned_by_digest_has_no_job() {
        for image in [
            "ghcr.io/firestoned/faubourg-bootstrap:v0.1.0",
            "ghcr.io/firestoned/faubourg-bootstrap@sha256:0123",
            "ghcr.io/firestoned/faubourg-bootstrap@sha256:\
             0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
            "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "",
        ] {
            let c = BootstrapConfig {
                image: image.into(),
                ..cfg()
            };
            assert_eq!(
                bootstrap_job(&host(), &c),
                Err(JobError::ImageNotPinned),
                "{image}"
            );
        }
    }

    // ----- placement, ownership, lifecycle ------------------------------------------------

    #[test]
    fn the_job_lives_in_the_credentials_namespace_and_is_owned_by_its_host() {
        let j = build(&host());
        assert_eq!(
            j.metadata.namespace.as_deref(),
            Some("faubourg-credentials")
        );
        assert_eq!(j.metadata.name.as_deref(), Some("bootstrap-hv01-g3"));
        let owners = j.metadata.owner_references.as_deref().unwrap();
        assert_eq!(owners.len(), 1);
        let o = &owners[0];
        assert_eq!(o.api_version, format!("{GROUP}/{VERSION}"));
        assert_eq!(o.kind, "Host");
        assert_eq!(o.name, "hv01");
        assert_eq!(o.uid, "8f6b8a52-0000-4000-8000-000000000001");
        assert_eq!(o.controller, Some(true));
    }

    #[test]
    fn the_job_and_its_pods_are_labelled_with_their_host() {
        let j = build(&host());
        let spec = j.spec.as_ref().unwrap();
        for labels in [
            j.metadata.labels.as_ref().unwrap(),
            spec.template
                .metadata
                .as_ref()
                .unwrap()
                .labels
                .as_ref()
                .unwrap(),
        ] {
            assert_eq!(labels.get(LABEL_HOST).map(String::as_str), Some("hv01"));
            assert_eq!(
                labels.get(LABEL_COMPONENT).map(String::as_str),
                Some("bootstrap")
            );
            assert_eq!(
                labels.get(LABEL_MANAGED_BY).map(String::as_str),
                Some("faubourg")
            );
        }
        let ann = j.metadata.annotations.as_ref().unwrap();
        assert_eq!(
            ann.get(ANNOTATION_GENERATION).map(String::as_str),
            Some("3")
        );
    }

    #[test]
    fn drift_and_a_refused_host_key_fail_the_job_without_retries() {
        let spec = build(&host()).spec.unwrap();
        assert_eq!(spec.backoff_limit, Some(BACKOFF_LIMIT));
        assert_eq!(spec.active_deadline_seconds, Some(ACTIVE_DEADLINE_SECONDS));
        assert_eq!(
            spec.ttl_seconds_after_finished, None,
            "a finished Job is the record of the result; the controller deletes old generations"
        );
        let rules = spec.pod_failure_policy.unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, "FailJob");
        let codes = rules[0].on_exit_codes.as_ref().unwrap();
        assert_eq!(codes.container_name.as_deref(), Some(CONTAINER));
        assert_eq!(codes.operator, "In");
        assert_eq!(codes.values, vec![EXIT_SCRIPT_DRIFT, EXIT_HOST_KEY_REFUSED]);
    }

    // ----- the shape the admission policy admits (ADR-0008 decision 3) --------------------

    #[test]
    fn one_container_on_the_pinned_image_with_no_command() {
        let j = build(&host());
        let p = pod(&j);
        assert_eq!(p.containers.len(), 1);
        assert!(p.init_containers.is_none());
        assert!(p.ephemeral_containers.is_none());
        let c = container(&j);
        assert_eq!(c.name, CONTAINER);
        assert_eq!(c.image.as_deref(), Some(IMAGE));
        assert!(c.command.is_none());
        assert!(c.args.is_none());
        assert_eq!(c.image_pull_policy.as_deref(), Some("IfNotPresent"));
    }

    #[test]
    fn the_environment_is_literal_and_carries_only_connection_settings() {
        let j = build(&host());
        let c = container(&j);
        assert!(c.env_from.is_none());
        for e in c.env.as_deref().unwrap() {
            assert!(e.value_from.is_none(), "{}", e.name);
        }
        let env = env(&j);
        let want: BTreeMap<String, Option<String>> = [
            (ENV_HOST, "hv01"),
            (ENV_ADDRESS, "hv01.home.example"),
            (ENV_PORT, "2222"),
            (ENV_HOST_KEY_POLICY, "Strict"),
            (ENV_REVOKE_PLACEMENT, "true"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Some(v.to_string())))
        .collect();
        assert_eq!(env, want);
    }

    #[test]
    fn a_pinned_tofu_fingerprint_is_passed_on() {
        let mut h = host();
        h.spec.ssh.as_mut().unwrap().host_key_policy = HostKeyPolicy::TrustOnFirstUse;
        h.status = Some(HostStatus {
            accepted_host_key: Some("SHA256:<fingerprint>".into()),
            ..HostStatus::default()
        });
        let env = env(&build(&h));
        assert_eq!(
            env.get(ENV_HOST_KEY_POLICY),
            Some(&Some("TrustOnFirstUse".into()))
        );
        assert_eq!(
            env.get(ENV_ACCEPTED_HOST_KEY),
            Some(&Some("SHA256:<fingerprint>".into()))
        );
    }

    #[test]
    fn it_mounts_only_its_hosts_secrets_and_the_script() {
        let j = build(&host());
        let names: Vec<&str> = volumes(&j).iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, [VOLUME_CREDENTIALS, VOLUME_PLACEMENT, VOLUME_SCRIPT]);
        let secret = |n: &str| {
            volume(&j, n)
                .and_then(|v| v.secret.as_ref())
                .and_then(|s| s.secret_name.clone())
        };
        assert_eq!(secret(VOLUME_CREDENTIALS).as_deref(), Some("hv01-ssh"));
        assert_eq!(
            secret(VOLUME_PLACEMENT).as_deref(),
            Some("hv01-ssh-placement")
        );
        for n in [VOLUME_CREDENTIALS, VOLUME_PLACEMENT] {
            let s = volume(&j, n).unwrap().secret.as_ref().unwrap();
            assert_eq!(s.default_mode, Some(0o400), "{n}");
            assert_ne!(
                s.optional,
                Some(true),
                "{n}: a missing Secret must not start the pod"
            );
        }
    }

    #[test]
    fn no_placement_credential_no_placement_volume() {
        let mut h = host();
        let ssh = h.spec.ssh.as_mut().unwrap();
        ssh.placement_secret_ref = None;
        ssh.revoke_after_bootstrap = false;
        let j = build(&h);
        assert!(volume(&j, VOLUME_PLACEMENT).is_none());
        let mounts = container(&j).volume_mounts.as_deref().unwrap();
        assert!(mounts.iter().all(|m| m.name != VOLUME_PLACEMENT));
        assert_eq!(
            env(&j).get(ENV_REVOKE_PLACEMENT),
            Some(&Some("false".into()))
        );
    }

    #[test]
    fn the_default_script_is_a_configmap_key_mounted_as_install_sh() {
        let j = build(&host());
        let cm = volume(&j, VOLUME_SCRIPT)
            .unwrap()
            .config_map
            .as_ref()
            .unwrap();
        assert_eq!(cm.name, "faubourg-install");
        let items = cm.items.as_deref().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, DEFAULT_SCRIPT_KEY);
        assert_eq!(items[0].path, "install.sh");
        assert!(volume(&j, VOLUME_SCRIPT).unwrap().secret.is_none());
    }

    #[test]
    fn the_hosts_own_script_ref_wins_and_may_be_a_secret() {
        let mut h = host();
        h.spec.bootstrap.script_ref = Some(ScriptRef {
            kind: ScriptSourceKind::Secret,
            name: "rack-a-install".into(),
            key: "rack-a.sh".into(),
        });
        let j = build(&h);
        let v = volume(&j, VOLUME_SCRIPT).unwrap();
        assert!(v.config_map.is_none());
        let s = v.secret.as_ref().unwrap();
        assert_eq!(s.secret_name.as_deref(), Some("rack-a-install"));
        let items = s.items.as_deref().unwrap();
        assert_eq!(
            (items[0].key.as_str(), items[0].path.as_str()),
            ("rack-a.sh", "install.sh")
        );
    }

    #[test]
    fn every_mount_is_read_only_at_its_fixed_path() {
        let j = build(&host());
        let mounts: Vec<(&str, &str, Option<bool>)> = container(&j)
            .volume_mounts
            .as_deref()
            .unwrap()
            .iter()
            .map(|m| (m.name.as_str(), m.mount_path.as_str(), m.read_only))
            .collect();
        assert_eq!(
            mounts,
            [
                (VOLUME_CREDENTIALS, MOUNT_CREDENTIALS, Some(true)),
                (VOLUME_PLACEMENT, MOUNT_PLACEMENT, Some(true)),
                (VOLUME_SCRIPT, MOUNT_SCRIPT, Some(true)),
            ]
        );
        assert!(container(&j).volume_devices.is_none());
    }

    #[test]
    fn no_token_no_host_namespaces_and_never_restarts() {
        let j = build(&host());
        let p = pod(&j);
        assert_eq!(
            p.service_account_name.as_deref(),
            Some("faubourg-bootstrap")
        );
        assert_eq!(p.automount_service_account_token, Some(false));
        assert_eq!(p.enable_service_links, Some(false));
        assert_eq!(p.restart_policy.as_deref(), Some("Never"));
        for (what, v) in [
            ("hostNetwork", p.host_network),
            ("hostPID", p.host_pid),
            ("hostIPC", p.host_ipc),
            ("shareProcessNamespace", p.share_process_namespace),
        ] {
            assert_ne!(v, Some(true), "{what}");
        }
    }

    #[test]
    fn the_restricted_security_profile() {
        let j = build(&host());
        let ps = pod(&j).security_context.as_ref().unwrap();
        assert_eq!(ps.run_as_non_root, Some(true));
        assert_eq!(ps.run_as_user, Some(RUN_AS));
        assert_eq!(ps.run_as_group, Some(RUN_AS));
        assert_eq!(
            ps.seccomp_profile.as_ref().map(|s| s.type_.as_str()),
            Some("RuntimeDefault")
        );
        let cs = container(&j).security_context.as_ref().unwrap();
        assert_eq!(cs.run_as_non_root, Some(true));
        assert_eq!(cs.allow_privilege_escalation, Some(false));
        assert_eq!(cs.privileged, Some(false));
        assert_eq!(cs.read_only_root_filesystem, Some(true));
        let caps = cs.capabilities.as_ref().unwrap();
        assert_eq!(caps.drop.as_deref(), Some(&["ALL".to_string()][..]));
        assert!(caps.add.is_none());
    }

    #[test]
    fn the_job_names_secrets_but_holds_no_secret_material() {
        let s = serde_json::to_string(&build(&host())).unwrap();
        for forbidden in [
            "privateKey",
            "password",
            "passphrase",
            "valueFrom",
            "envFrom",
            "hostPath",
        ] {
            assert!(!s.contains(forbidden), "{forbidden} in {s}");
        }
    }
}

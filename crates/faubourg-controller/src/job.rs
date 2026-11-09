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
use std::fmt::Write as _;

use faubourg_api::host::{
    BootstrapMethod, Host, HostKeyPolicy, HostSpec, HostStatus, ScriptRef, ScriptSourceKind,
    SshSpec,
};
use faubourg_api::policy::{PinnedScript, Policy, pinned_digest};
use faubourg_api::validation::Invalid;
use faubourg_api::{GROUP, LABEL_HOST, VERSION};
use k8s_openapi::api::batch::v1::{
    Job, JobSpec, PodFailurePolicy, PodFailurePolicyOnExitCodesRequirement, PodFailurePolicyRule,
};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMapProjection, ConfigMapVolumeSource, Container, EnvVar, KeyToPath, Pod,
    PodSecurityContext, PodSpec, PodTemplateSpec, ProjectedVolumeSource, ResourceRequirements,
    SeccompProfile, SecretVolumeSource, SecurityContext, ServiceAccountTokenProjection, Volume,
    VolumeMount, VolumeProjection,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::phase::{JobFailure, JobState};

// The contract with the bootstrap image lives in faubourg-api, so the builder and the image
// cannot disagree on it (ADR-0010).
pub use faubourg_api::bootstrap::{
    CONTAINER, ENV_ACCEPTED_HOST_KEY, ENV_ADDRESS, ENV_HOST, ENV_HOST_KEY_POLICY, ENV_PORT,
    ENV_PROVIDER, ENV_PROVIDER_API_SERVER, ENV_PROVIDER_CA_SHA256, ENV_PROVIDER_NAMESPACE,
    ENV_PROVIDER_SERVICE_ACCOUNT, ENV_REVOKE_PLACEMENT, ENV_SCRIPT_SHA256, EXIT_HOST_KEY_REFUSED,
    EXIT_PROVIDER_CA_UNPINNED, EXIT_SCRIPT_DRIFT, EXIT_SCRIPT_UNPINNED, KUBE_API_CA_FILE,
    KUBE_API_TOKEN_FILE, KUBE_API_TOKEN_SECONDS, KUBE_ROOT_CA_CONFIGMAP, MOUNT_CREDENTIALS,
    MOUNT_KUBE_API, MOUNT_PLACEMENT, MOUNT_PROVIDER_CA, MOUNT_SCRIPT, PROVIDER_CA_FILE,
    SCRIPT_FILE, VOLUME_CREDENTIALS, VOLUME_KUBE_API, VOLUME_PLACEMENT, VOLUME_PROVIDER_CA,
    VOLUME_SCRIPT,
};
use faubourg_api::bootstrap::{Report, is_fingerprint};

/// Label: what manages the Job.
pub const LABEL_MANAGED_BY: &str = "app.kubernetes.io/managed-by";
/// Label: which part of faubourg the Job is.
pub const LABEL_COMPONENT: &str = "app.kubernetes.io/component";
/// Annotation: the Host generation the Job bootstraps.
pub const ANNOTATION_GENERATION: &str = "faubourg.firestoned.io/generation";
/// Annotation: the sha256 of the Host's bootstrap inputs (ADR-0009 decision 6). A Host gets
/// a new Job when this changes, not on every generation.
pub const ANNOTATION_INPUTS: &str = "faubourg.firestoned.io/bootstrap-inputs";
/// Label on a re-bootstrap Job, one the rollout counts (ADR-0013 decision 5).
pub const LABEL_REBOOTSTRAP: &str = "faubourg.firestoned.io/rebootstrap";
/// Label the Job controller puts on a Job's pods.
pub const LABEL_JOB_NAME: &str = "batch.kubernetes.io/job-name";

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
    /// The host's Provider, as the Job needs it (ADR-0011).
    pub provider: ProviderConfig,
    /// At most this many re-bootstrap Jobs run at once, across the cluster (ADR-0013).
    pub max_rebootstraps: u32,
}

/// Where the hosts' Providers live, and how their providers reach the API server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfig {
    /// The `ProviderClass` every host's Provider references; it must be `External`.
    pub class: String,
    /// banlieue's namespace, where the Providers and their `ServiceAccounts` live.
    pub namespace: String,
    /// The API server address the hosts' providers use (`https://...`).
    pub api_server: String,
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
    /// The script source has no digest pinned in the bootstrap policy (ADR-0013 decision 2).
    ScriptNotPinned(PinState),
}

/// Why a script has no pinned digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinState {
    /// The bootstrap policy's parameters are missing, or `scriptSources` is malformed.
    PolicyUnreadable,
    /// The policy pins no digest for this source and key.
    NotPinned,
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
            Self::ScriptNotPinned(PinState::PolicyUnreadable) => f.write_str(
                "the bootstrap policy's scriptSources is missing or malformed, so no script is pinned",
            ),
            Self::ScriptNotPinned(PinState::NotPinned) => {
                f.write_str("the bootstrap policy pins no digest for this Host's script source and key")
            }
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
pub fn is_digest_pinned(image: &str) -> bool {
    image.split_once("@sha256:").is_some_and(|(repo, digest)| {
        !repo.is_empty()
            && digest.len() == 64
            && digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// Builds the bootstrap Job for the Host's current generation.
pub fn bootstrap_job(
    host: &Host,
    cfg: &BootstrapConfig,
    policy: Option<&Policy>,
) -> Result<Job, JobError> {
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
    let digest = pinned(policy, script)?;
    let provider_ca = policy.and_then(|p| p.provider_ca.as_ref());
    // validate() refuses Script without ssh, so this cannot fail; fail closed regardless.
    let ssh = spec.ssh.as_ref().ok_or(JobError::NotPersisted("ssh"))?;
    let inputs = inputs_hash(
        &spec.address,
        ssh,
        script,
        digest,
        provider_ca.map(|c| c.sha256.as_str()),
    );

    let labels: BTreeMap<String, String> = [
        (LABEL_HOST, name),
        (LABEL_COMPONENT, CONTAINER),
        (LABEL_MANAGED_BY, faubourg_api::PROJECT),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let mut env = environment(name, &spec.address, ssh, host.status.as_ref());
    env.extend([
        literal(ENV_SCRIPT_SHA256, digest),
        literal(ENV_PROVIDER, name),
        literal(ENV_PROVIDER_NAMESPACE, &cfg.provider.namespace),
        literal(ENV_PROVIDER_API_SERVER, &cfg.provider.api_server),
    ]);

    if let Some(ca) = provider_ca {
        env.push(literal(ENV_PROVIDER_CA_SHA256, &ca.sha256));
    }
    let (volumes, mounts) = volumes_and_mounts(ssh, script, provider_ca);

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
            annotations: Some(BTreeMap::from([
                (ANNOTATION_GENERATION.to_string(), generation.to_string()),
                (ANNOTATION_INPUTS.to_string(), inputs),
            ])),
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

/// `job`, given the `ServiceAccount` its token is minted for, as banlieue publishes it on the
/// host's Provider (banlieue ADR-0126). Not a bootstrap input: a Job is only created once the
/// name is known, and the name does not change which Job is the Host's current one.
#[must_use]
pub fn with_provider_identity(mut job: Job, service_account: &str) -> Job {
    if let Some(c) = job
        .spec
        .as_mut()
        .and_then(|s| s.template.spec.as_mut())
        .and_then(|p| p.containers.first_mut())
    {
        c.env
            .get_or_insert_default()
            .push(literal(ENV_PROVIDER_SERVICE_ACCOUNT, service_account));
    }
    job
}

/// The sha256 of a Host's bootstrap inputs, as the Job's [`ANNOTATION_INPUTS`] carries it
/// (ADR-0009 decision 6), or `None` for a Host that gets no Job (adopted, or no script
/// source). The inputs are `address`, `ssh` and the resolved script source; `zoneRef`,
/// `cordoned` and the image are not, so cordoning a host or upgrading the controller runs
/// nothing on it.
pub fn bootstrap_inputs(
    spec: &HostSpec,
    default_script: Option<&ScriptRef>,
    policy: Option<&Policy>,
) -> Option<String> {
    if spec.bootstrap.method != BootstrapMethod::Script {
        return None;
    }
    let script = spec.bootstrap.script_ref.as_ref().or(default_script)?;
    let digest = pinned(policy, script).ok()?;
    let provider_ca = policy.and_then(|p| p.provider_ca.as_ref());
    Some(inputs_hash(
        &spec.address,
        spec.ssh.as_ref()?,
        script,
        digest,
        provider_ca.map(|c| c.sha256.as_str()),
    ))
}

/// The digest the bootstrap policy pins for `script` (ADR-0013 decision 2).
fn pinned<'a>(policy: Option<&'a Policy>, script: &ScriptRef) -> Result<&'a str, JobError> {
    let policy = policy.ok_or(JobError::ScriptNotPinned(PinState::PolicyUnreadable))?;
    pinned_digest(&policy.scripts, script).ok_or(JobError::ScriptNotPinned(PinState::NotPinned))
}

/// The canonical encoding is `serde_json` of this struct: field order is declaration order.
/// Changing it re-runs every host's install once; a test pins it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Inputs<'a> {
    address: &'a str,
    ssh: &'a SshSpec,
    script: &'a ScriptRef,
    script_sha256: &'a str,
    // Absent when no provider CA is pinned, so the encoding, and every Host's inputs, are
    // unchanged for a cluster that does not use one (ADR-0016).
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_ca_sha256: Option<&'a str>,
}

fn inputs_hash(
    address: &str,
    ssh: &SshSpec,
    script: &ScriptRef,
    script_sha256: &str,
    provider_ca_sha256: Option<&str>,
) -> String {
    // Serializing plain strings, numbers and enums into a Vec cannot fail.
    let bytes = serde_json::to_vec(&Inputs {
        address,
        ssh,
        script,
        script_sha256,
        provider_ca_sha256,
    })
    .unwrap_or_default();
    Sha256::digest(&bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// True when `job` is controlled by the Host with this UID (ADR-0009 decision 3). A Job with
/// the right labels but another owner, or none, is not the Host's, whoever created it.
pub fn owned_by(job: &Job, host_uid: &str) -> bool {
    job.metadata.owner_references.iter().flatten().any(|r| {
        r.controller == Some(true)
            && r.kind == "Host"
            && r.api_version == format!("{GROUP}/{VERSION}")
            && r.uid == host_uid
    })
}

/// The Job's [`ANNOTATION_INPUTS`], if any.
pub fn job_inputs(job: &Job) -> Option<&str> {
    job.metadata
        .annotations
        .as_ref()?
        .get(ANNOTATION_INPUTS)
        .map(String::as_str)
}

/// The Job's [`ANNOTATION_GENERATION`], or 0 when absent or unreadable.
pub fn job_generation(job: &Job) -> i64 {
    job.metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANNOTATION_GENERATION))
        .and_then(|g| g.parse().ok())
        .unwrap_or(0)
}

/// A Job's state (ADR-0009 decision 8). `pods` may hold any pods; only those the Job
/// controls are read, for the `bootstrap` container's exit code.
pub fn job_state(job: &Job, pods: &[Pod]) -> JobState {
    let conditions = job
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or_default();
    let holds = |t: &str| {
        conditions
            .iter()
            .any(|c| c.type_ == t && c.status == "True")
    };
    if holds("Failed") || holds("FailureTarget") {
        return JobState::Failed(failure(job, pods));
    }
    if holds("Complete") {
        return JobState::Succeeded;
    }
    JobState::Running
}

/// The host key fingerprint a Job's bootstrap container reported in its termination message
/// (ADR-0010 decision 7), read only from pods the Job controls. Anything but a well-formed
/// fingerprint is ignored, so a pod cannot pin junk into a Host.
pub fn reported_host_key(job: &Job, pods: &[Pod]) -> Option<String> {
    let uid = job.metadata.uid.as_deref()?;
    pods.iter()
        .filter(|p| controlled_by(p, uid))
        .filter_map(|p| p.status.as_ref()?.container_statuses.as_ref())
        .flatten()
        .filter(|c| c.name == CONTAINER)
        .filter_map(|c| c.state.as_ref()?.terminated.as_ref()?.message.as_deref())
        .filter_map(|m| serde_json::from_str::<Report>(m).ok()?.host_key)
        .find(|fp| is_fingerprint(fp))
}

fn controlled_by(p: &Pod, job_uid: &str) -> bool {
    p.metadata
        .owner_references
        .iter()
        .flatten()
        .any(|r| r.controller == Some(true) && r.kind == "Job" && r.uid == job_uid)
}

fn failure(job: &Job, pods: &[Pod]) -> JobFailure {
    let Some(uid) = job.metadata.uid.as_deref() else {
        return JobFailure::Other;
    };
    let codes: Vec<i32> = pods
        .iter()
        .filter(|p| controlled_by(p, uid))
        .filter_map(|p| p.status.as_ref()?.container_statuses.as_ref())
        .flatten()
        .filter(|c| c.name == CONTAINER)
        .filter_map(|c| Some(c.state.as_ref()?.terminated.as_ref()?.exit_code))
        .collect();
    if codes.contains(&EXIT_SCRIPT_DRIFT) {
        JobFailure::ScriptDrift
    } else if codes.contains(&EXIT_HOST_KEY_REFUSED) {
        JobFailure::HostKeyRefused
    } else if codes.contains(&EXIT_SCRIPT_UNPINNED) {
        JobFailure::ScriptUnpinned
    } else if codes.contains(&EXIT_PROVIDER_CA_UNPINNED) {
        JobFailure::ProviderCaUnpinned
    } else {
        JobFailure::Other
    }
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
            // Secret volumes are root-owned 0400: without this group the bootstrap user
            // cannot read its own credentials. The kubelet makes them group-readable by it.
            fs_group: Some(RUN_AS),
            seccomp_profile: Some(SeccompProfile {
                type_: "RuntimeDefault".into(),
                ..SeccompProfile::default()
            }),
            ..PodSecurityContext::default()
        }),
        ..PodSpec::default()
    }
}

/// Retries for transient failures; drift, a refused host key and an unpinned script fail at
/// once.
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
                    values: vec![
                        EXIT_SCRIPT_DRIFT,
                        EXIT_HOST_KEY_REFUSED,
                        EXIT_SCRIPT_UNPINNED,
                        EXIT_PROVIDER_CA_UNPINNED,
                    ],
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

/// The Job's volumes and the container's read-only mounts of them: the credentials, the
/// placement credential if any, the script, the provider CA if pinned (ADR-0016), and the pod's
/// own `kube-api` access.
fn volumes_and_mounts(
    ssh: &SshSpec,
    script: &ScriptRef,
    provider_ca: Option<&PinnedScript>,
) -> (Vec<Volume>, Vec<VolumeMount>) {
    let mut volumes = vec![secret_volume(VOLUME_CREDENTIALS, &ssh.secret_ref.name)];
    let mut mounts = vec![read_only(VOLUME_CREDENTIALS, MOUNT_CREDENTIALS)];
    if let Some(p) = &ssh.placement_secret_ref {
        volumes.push(secret_volume(VOLUME_PLACEMENT, &p.name));
        mounts.push(read_only(VOLUME_PLACEMENT, MOUNT_PLACEMENT));
    }
    volumes.push(script_volume(script));
    mounts.push(read_only(VOLUME_SCRIPT, MOUNT_SCRIPT));
    if let Some(ca) = provider_ca {
        volumes.push(provider_ca_volume(&ca.source));
        mounts.push(read_only(VOLUME_PROVIDER_CA, MOUNT_PROVIDER_CA));
    }
    volumes.push(kube_api_volume());
    mounts.push(read_only(VOLUME_KUBE_API, MOUNT_KUBE_API));
    (volumes, mounts)
}

/// The pinned CA bundle's one key, as `ca.crt`. Always a `ConfigMap`: a CA is not secret, and
/// the policy parser refuses anything else.
fn provider_ca_volume(r: &ScriptRef) -> Volume {
    Volume {
        name: VOLUME_PROVIDER_CA.into(),
        config_map: Some(ConfigMapVolumeSource {
            name: r.name.clone(),
            items: Some(vec![KeyToPath {
                key: r.key.clone(),
                path: PROVIDER_CA_FILE.into(),
                ..KeyToPath::default()
            }]),
            default_mode: Some(0o444),
            ..ConfigMapVolumeSource::default()
        }),
        ..Volume::default()
    }
}

fn script_volume(r: &ScriptRef) -> Volume {
    let items = Some(vec![KeyToPath {
        key: r.key.clone(),
        path: SCRIPT_FILE.into(),
        ..KeyToPath::default()
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

/// The bootstrap pod's own API access (ADR-0011 decision 6): a short-lived token for its
/// `ServiceAccount`, the API server's default audience, and the cluster CA. The admission policy
/// admits this volume only in exactly this shape.
fn kube_api_volume() -> Volume {
    Volume {
        name: VOLUME_KUBE_API.into(),
        projected: Some(ProjectedVolumeSource {
            default_mode: Some(0o400),
            sources: Some(vec![
                VolumeProjection {
                    service_account_token: Some(ServiceAccountTokenProjection {
                        path: KUBE_API_TOKEN_FILE.into(),
                        expiration_seconds: Some(KUBE_API_TOKEN_SECONDS),
                        audience: None,
                        ..ServiceAccountTokenProjection::default()
                    }),
                    ..VolumeProjection::default()
                },
                VolumeProjection {
                    config_map: Some(ConfigMapProjection {
                        name: KUBE_ROOT_CA_CONFIGMAP.into(),
                        items: Some(vec![KeyToPath {
                            key: KUBE_API_CA_FILE.into(),
                            path: KUBE_API_CA_FILE.into(),
                            ..KeyToPath::default()
                        }]),
                        optional: None,
                    }),
                    ..VolumeProjection::default()
                },
            ]),
            ..ProjectedVolumeSource::default()
        }),
        ..Volume::default()
    }
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
    use faubourg_api::policy::PinnedScript;

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
            provider: ProviderConfig {
                class: "cloud-hypervisor".into(),
                namespace: "banlieue-system".into(),
                api_server: "https://kube.home.example:6443".into(),
            },
            max_rebootstraps: 1,
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

    /// The default script, pinned as the bootstrap policy would (ADR-0013).
    const SCRIPT_SHA256: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    fn policy(scripts: Vec<PinnedScript>) -> Policy {
        Policy {
            scripts,
            provider_ca: None,
        }
    }

    fn pins() -> Vec<PinnedScript> {
        vec![PinnedScript {
            source: cfg().default_script.unwrap(),
            sha256: SCRIPT_SHA256.into(),
        }]
    }

    /// The name banlieue publishes for the fixture Host's Provider.
    const SA: &str = "banlieue-provider-cloud-hypervisor-hv01";

    fn build(h: &Host) -> Job {
        with_provider_identity(
            bootstrap_job(h, &cfg(), Some(&policy(pins()))).expect("a Job"),
            SA,
        )
    }

    #[test]
    fn the_provider_identity_is_banlieues_and_not_an_input() {
        let bare = bootstrap_job(&host(), &cfg(), Some(&policy(pins()))).unwrap();
        assert!(
            !env(&bare).contains_key(ENV_PROVIDER_SERVICE_ACCOUNT),
            "faubourg derives no name"
        );
        let j = with_provider_identity(bare.clone(), "whatever-banlieue-named-it");
        assert_eq!(
            env(&j).get(ENV_PROVIDER_SERVICE_ACCOUNT),
            Some(&Some("whatever-banlieue-named-it".to_string()))
        );
        assert_eq!(job_inputs(&j), job_inputs(&bare));
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
        assert_eq!(
            bootstrap_job(&h, &cfg(), Some(&policy(pins()))),
            Err(JobError::NotScript)
        );
    }

    #[test]
    fn an_invalid_spec_has_no_job() {
        let mut h = host();
        h.spec.address = "-oProxyCommand=sh".into();
        assert!(matches!(
            bootstrap_job(&h, &cfg(), Some(&policy(pins()))),
            Err(JobError::InvalidSpec(_))
        ));
    }

    #[test]
    fn a_host_not_read_from_the_api_server_has_no_job() {
        let mut h = host();
        h.metadata.uid = None;
        assert_eq!(
            bootstrap_job(&h, &cfg(), Some(&policy(pins()))),
            Err(JobError::NotPersisted("uid"))
        );
        let mut h = host();
        h.metadata.generation = None;
        assert_eq!(
            bootstrap_job(&h, &cfg(), Some(&policy(pins()))),
            Err(JobError::NotPersisted("generation"))
        );
        let mut h = host();
        h.metadata.name = None;
        assert_eq!(
            bootstrap_job(&h, &cfg(), Some(&policy(pins()))),
            Err(JobError::NotPersisted("name"))
        );
    }

    #[test]
    fn a_host_name_too_long_for_a_label_has_no_job() {
        let mut h = host();
        h.metadata.name = Some("h".repeat(64));
        assert_eq!(
            bootstrap_job(&h, &cfg(), Some(&policy(pins()))),
            Err(JobError::NameTooLong)
        );
    }

    #[test]
    fn no_script_source_has_no_job() {
        let c = BootstrapConfig {
            default_script: None,
            ..cfg()
        };
        assert_eq!(
            bootstrap_job(&host(), &c, Some(&policy(pins()))),
            Err(JobError::NoScriptSource)
        );
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
                bootstrap_job(&host(), &c, Some(&policy(pins()))),
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
        assert_eq!(
            codes.values,
            vec![
                EXIT_SCRIPT_DRIFT,
                EXIT_HOST_KEY_REFUSED,
                EXIT_SCRIPT_UNPINNED,
                EXIT_PROVIDER_CA_UNPINNED
            ]
        );
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
            (ENV_PROVIDER, "hv01"),
            (ENV_PROVIDER_NAMESPACE, "banlieue-system"),
            (
                ENV_PROVIDER_SERVICE_ACCOUNT,
                "banlieue-provider-cloud-hypervisor-hv01",
            ),
            (ENV_PROVIDER_API_SERVER, "https://kube.home.example:6443"),
            (ENV_SCRIPT_SHA256, SCRIPT_SHA256),
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
        assert_eq!(
            names,
            [
                VOLUME_CREDENTIALS,
                VOLUME_PLACEMENT,
                VOLUME_SCRIPT,
                VOLUME_KUBE_API
            ]
        );
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
        let mut pinned = pins();
        pinned.push(PinnedScript {
            source: h.spec.bootstrap.script_ref.clone().unwrap(),
            sha256: SCRIPT_SHA256.into(),
        });
        let j = bootstrap_job(&h, &cfg(), Some(&policy(pinned))).unwrap();
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
                (VOLUME_KUBE_API, MOUNT_KUBE_API, Some(true)),
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
            ps.fs_group,
            Some(RUN_AS),
            "or the image cannot read its Secrets"
        );
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

    // ----- bootstrap inputs, ownership, state (ADR-0009) ----------------------------------

    fn inputs(h: &Host) -> Option<String> {
        bootstrap_inputs(
            &h.spec,
            cfg().default_script.as_ref(),
            Some(&policy(pins())),
        )
    }

    #[test]
    fn the_job_carries_its_hosts_bootstrap_inputs() {
        let h = host();
        let j = build(&h);
        assert_eq!(job_inputs(&j), inputs(&h).as_deref());
        assert_eq!(job_generation(&j), h.metadata.generation.unwrap());
        let hash = inputs(&h).unwrap();
        assert_eq!(hash.len(), 64);
        assert!(
            hash.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }

    #[test]
    fn the_inputs_encoding_is_pinned() {
        // Changing this value re-runs every host's install once (ADR-0009). Change it only
        // on purpose, and say so in the commit. Last changed by ADR-0013: the pinned digest.
        assert_eq!(
            inputs(&host()).as_deref(),
            Some("a435908ba9b46498cdf66300974515730b7ec9961409cf369bc72e8c15c37c0f"),
        );
    }

    #[test]
    fn cordon_zone_and_image_are_not_bootstrap_inputs() {
        let base = inputs(&host());
        let mut h = host();
        h.spec.cordoned = true;
        h.spec.zone_ref = "az2".into();
        h.metadata.generation = Some(7);
        assert_eq!(inputs(&h), base);
        let mut other = cfg();
        other.image = IMAGE.replace("0123", "4567");
        assert_eq!(
            bootstrap_job(&h, &other, Some(&policy(pins())))
                .unwrap()
                .metadata
                .annotations
                .unwrap()[ANNOTATION_INPUTS],
            base.unwrap()
        );
    }

    #[test]
    fn address_ssh_and_script_are_bootstrap_inputs() {
        let base = inputs(&host());
        let mut changes: Vec<Host> = Vec::new();
        let mut h = host();
        h.spec.address = "hv02.home.example".into();
        changes.push(h);
        let mut h = host();
        h.spec.ssh.as_mut().unwrap().port = 22;
        changes.push(h);
        let mut h = host();
        h.spec.ssh.as_mut().unwrap().secret_ref.name = "hv01-ssh-2".into();
        changes.push(h);
        let mut h = host();
        h.spec.ssh.as_mut().unwrap().host_key_policy = HostKeyPolicy::TrustOnFirstUse;
        changes.push(h);
        let mut h = host();
        h.spec.ssh.as_mut().unwrap().revoke_after_bootstrap = false;
        changes.push(h);
        let mut h = host();
        h.spec.bootstrap.script_ref = Some(ScriptRef {
            kind: ScriptSourceKind::ConfigMap,
            name: "other-script".into(),
            key: DEFAULT_SCRIPT_KEY.into(),
        });
        changes.push(h);
        for h in &changes {
            assert_ne!(inputs(h), base, "{:?}", h.spec);
        }
    }

    #[test]
    fn the_pinned_digest_is_an_input_and_in_the_environment() {
        let base = inputs(&host());
        let mut repinned = pins();
        repinned[0].sha256 = SCRIPT_SHA256.replace("fedc", "0123");
        let h = host();
        assert_ne!(
            bootstrap_inputs(
                &h.spec,
                cfg().default_script.as_ref(),
                Some(&policy(repinned))
            ),
            base,
            "a re-pin is a new input (ADR-0013 decision 3)"
        );
        assert_eq!(
            env(&build(&h)).get(ENV_SCRIPT_SHA256),
            Some(&Some(SCRIPT_SHA256.to_string()))
        );
    }

    const CA_SHA256: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";

    fn with_ca() -> Policy {
        Policy {
            scripts: pins(),
            provider_ca: Some(PinnedScript {
                source: ScriptRef {
                    kind: ScriptSourceKind::ConfigMap,
                    name: "api-ca".into(),
                    key: "lb.pem".into(),
                },
                sha256: CA_SHA256.into(),
            }),
        }
    }

    #[test]
    fn a_pinned_provider_ca_is_mounted_read_only_with_its_digest() {
        let j = bootstrap_job(&host(), &cfg(), Some(&with_ca())).unwrap();
        let v = volume(&j, VOLUME_PROVIDER_CA).expect("the provider CA volume");
        let cm = v.config_map.as_ref().expect("a ConfigMap, never a Secret");
        assert_eq!(cm.name, "api-ca");
        let items = cm.items.as_deref().unwrap();
        assert_eq!(
            (items[0].key.as_str(), items[0].path.as_str()),
            ("lb.pem", PROVIDER_CA_FILE)
        );
        let mounts = container(&j).volume_mounts.as_deref().unwrap();
        let m = mounts
            .iter()
            .find(|m| m.name == VOLUME_PROVIDER_CA)
            .unwrap();
        assert_eq!(
            (m.mount_path.as_str(), m.read_only),
            (MOUNT_PROVIDER_CA, Some(true))
        );
        assert_eq!(
            env(&j).get(ENV_PROVIDER_CA_SHA256),
            Some(&Some(CA_SHA256.to_string()))
        );
        let plain = build(&host());
        assert!(
            volume(&plain, VOLUME_PROVIDER_CA).is_none(),
            "none without a pin"
        );
        assert!(!env(&plain).contains_key(ENV_PROVIDER_CA_SHA256));
    }

    #[test]
    fn the_provider_ca_digest_is_an_input() {
        let h = host();
        let with = bootstrap_inputs(&h.spec, cfg().default_script.as_ref(), Some(&with_ca()));
        assert_ne!(
            with,
            inputs(&h),
            "setting a CA re-bootstraps (ADR-0016 decision 2)"
        );
        let mut other = with_ca();
        other.provider_ca.as_mut().unwrap().sha256 = CA_SHA256.replace("abcd", "1234");
        assert_ne!(
            bootstrap_inputs(&h.spec, cfg().default_script.as_ref(), Some(&other)),
            with,
            "so does changing it"
        );
    }

    #[test]
    fn an_unpinned_script_gets_no_job() {
        let h = host();
        assert_eq!(
            bootstrap_job(&h, &cfg(), None),
            Err(JobError::ScriptNotPinned(PinState::PolicyUnreadable)),
            "no readable policy, no Job"
        );
        assert_eq!(
            bootstrap_job(&h, &cfg(), Some(&policy(vec![]))),
            Err(JobError::ScriptNotPinned(PinState::NotPinned))
        );
        let mut other_key = pins();
        other_key[0].source.key = "other.sh".into();
        assert_eq!(
            bootstrap_job(&h, &cfg(), Some(&policy(other_key))),
            Err(JobError::ScriptNotPinned(PinState::NotPinned)),
            "the key is part of the pin"
        );
        assert_eq!(
            bootstrap_inputs(&h.spec, cfg().default_script.as_ref(), None),
            None
        );
    }

    #[test]
    fn a_host_with_no_job_has_no_inputs() {
        let mut h = host();
        h.spec.bootstrap.method = BootstrapMethod::None;
        assert_eq!(inputs(&h), None);
        let h = host();
        assert_eq!(bootstrap_inputs(&h.spec, None, Some(&policy(pins()))), None);
    }

    #[test]
    fn only_the_hosts_own_jobs_are_its_jobs() {
        let j = build(&host());
        assert!(owned_by(&j, "8f6b8a52-0000-4000-8000-000000000001"));
        assert!(!owned_by(&j, "8f6b8a52-0000-4000-8000-000000000002"));
        let mut orphan = j.clone();
        orphan.metadata.owner_references = None;
        assert!(!owned_by(&orphan, "8f6b8a52-0000-4000-8000-000000000001"));
        let mut not_controller = j.clone();
        not_controller.metadata.owner_references.as_mut().unwrap()[0].controller = None;
        assert!(!owned_by(
            &not_controller,
            "8f6b8a52-0000-4000-8000-000000000001"
        ));
        let mut other_kind = j;
        other_kind.metadata.owner_references.as_mut().unwrap()[0].kind = "Zone".into();
        assert!(!owned_by(
            &other_kind,
            "8f6b8a52-0000-4000-8000-000000000001"
        ));
    }

    fn with_conditions(types: &[&str]) -> Job {
        use k8s_openapi::api::batch::v1::{JobCondition, JobStatus};
        let mut j = build(&host());
        j.metadata.uid = Some("job-uid".into());
        j.status = Some(JobStatus {
            conditions: Some(
                types
                    .iter()
                    .map(|t| JobCondition {
                        type_: (*t).into(),
                        status: "True".into(),
                        ..JobCondition::default()
                    })
                    .collect(),
            ),
            ..JobStatus::default()
        });
        j
    }

    fn exited(job_uid: &str, code: i32) -> Pod {
        serde_json::from_value(serde_json::json!({
            "metadata": {
                "name": "bootstrap-hv01-g1-abcde",
                "ownerReferences": [{
                    "apiVersion": "batch/v1", "kind": "Job", "name": "bootstrap-hv01-g1",
                    "uid": job_uid, "controller": true
                }]
            },
            "status": { "containerStatuses": [{
                "name": "bootstrap", "image": "x", "imageID": "", "ready": false,
                "restartCount": 0,
                "state": { "terminated": { "exitCode": code } }
            }]}
        }))
        .unwrap()
    }

    #[test]
    fn a_job_with_no_condition_is_running() {
        assert_eq!(job_state(&with_conditions(&[]), &[]), JobState::Running);
        assert_eq!(job_state(&build(&host()), &[]), JobState::Running);
    }

    #[test]
    fn a_complete_job_succeeded() {
        assert_eq!(
            job_state(&with_conditions(&["SuccessCriteriaMet", "Complete"]), &[]),
            JobState::Succeeded
        );
    }

    #[test]
    fn a_failed_job_is_failed_from_its_first_failure_signal() {
        for c in [
            &["FailureTarget"][..],
            &["Failed"],
            &["FailureTarget", "Failed"],
        ] {
            assert_eq!(
                job_state(&with_conditions(c), &[]),
                JobState::Failed(JobFailure::Other),
                "{c:?}"
            );
        }
    }

    #[test]
    fn the_failure_cause_is_the_bootstrap_containers_exit_code() {
        let j = with_conditions(&["Failed"]);
        for (code, want) in [
            (EXIT_SCRIPT_DRIFT, JobFailure::ScriptDrift),
            (EXIT_HOST_KEY_REFUSED, JobFailure::HostKeyRefused),
            (EXIT_SCRIPT_UNPINNED, JobFailure::ScriptUnpinned),
            (EXIT_PROVIDER_CA_UNPINNED, JobFailure::ProviderCaUnpinned),
            (1, JobFailure::Other),
        ] {
            assert_eq!(
                job_state(&j, &[exited("job-uid", code)]),
                JobState::Failed(want),
                "{code}"
            );
        }
    }

    #[test]
    fn another_jobs_pod_says_nothing_about_this_one() {
        let j = with_conditions(&["Failed"]);
        assert_eq!(
            job_state(&j, &[exited("other-uid", EXIT_SCRIPT_DRIFT)]),
            JobState::Failed(JobFailure::Other)
        );
    }

    // ----- the bootstrap pod's own API access (ADR-0011) ---------------------------------

    #[test]
    fn the_kube_api_volume_is_a_short_lived_token_and_the_cluster_ca_only() {
        let j = build(&host());
        let v = volume(&j, VOLUME_KUBE_API).unwrap();
        assert!(v.secret.is_none() && v.config_map.is_none() && v.host_path.is_none());
        let sources = v.projected.as_ref().unwrap().sources.as_deref().unwrap();
        assert_eq!(sources.len(), 2);
        let token = sources[0].service_account_token.as_ref().unwrap();
        assert_eq!(token.expiration_seconds, Some(600));
        assert_eq!(token.audience, None, "the API server's default audience");
        assert_eq!(token.path, KUBE_API_TOKEN_FILE);
        let ca = sources[1].config_map.as_ref().unwrap();
        assert_eq!(ca.name, KUBE_ROOT_CA_CONFIGMAP);
        assert!(
            sources
                .iter()
                .all(|s| s.secret.is_none() && s.downward_api.is_none())
        );
        assert_eq!(
            pod(&j).automount_service_account_token,
            Some(false),
            "the token is only this projection, never the default mount"
        );
    }
}

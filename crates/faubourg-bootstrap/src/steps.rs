// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The bootstrap's steps (ADR-0010 decision 6): check, place, check again, revoke, run.
//! Written against [`Remote`], so the order and every branch is unit-tested without a host;
//! `crate::ssh` is the real implementation.

use crate::remote;

/// Which credential a command runs with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    /// The scoped run credential.
    Run,
    /// The privileged placement credential.
    Placement,
}

/// A finished command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// Its exit status; a command killed by a signal, or that reported none, is non-zero.
    pub status: u32,
    /// Its standard output, when it was not streamed to the log.
    pub stdout: Vec<u8>,
}

/// A host the steps run commands on.
pub trait Remote {
    /// Why a command could not run. `HostKeyRefused` is told apart by [`Remote::refused`].
    type Error: std::error::Error;

    /// Runs `command` as `who`, with `stdin` on its standard input. With `stream`, its output
    /// goes to the log as it comes instead of into [`Output::stdout`].
    fn exec(
        &mut self,
        who: Who,
        command: &str,
        stdin: &[u8],
        stream: bool,
    ) -> impl Future<Output = Result<Output, Self::Error>>;

    /// True when an error was the host key being refused.
    fn refused(&self, e: &Self::Error) -> bool;

    /// True when an error was the host refusing the credential: for the run credential on a
    /// fresh host, the scoped account does not exist yet.
    fn auth_refused(&self, e: &Self::Error) -> bool;
}

/// What the steps need beyond the remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// sha256 of the mounted script, lowercase hex.
    pub script_sha256: String,
    /// The script itself.
    pub script: Vec<u8>,
    /// The run credential's user, for the sudo rule and the account placement creates.
    pub run_user: String,
    /// The run credential's public key (`<type> <blob>`), which placement authorizes for the
    /// run account. `None` for a password run credential, which placement cannot provision.
    pub run_public_key: Option<String>,
    /// The placement credential, when mounted.
    pub placement: Option<Placement>,
    /// The install script's standard input: the provider credential bundle (ADR-0011
    /// decision 9), or nothing.
    pub run_stdin: Vec<u8>,
}

/// The placement credential, as the steps need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Its user is root: no `sudo` around the placement commands.
    pub as_root: bool,
    /// Remove its key from the host once placement succeeds, by this public key blob.
    pub revoke_blob: Option<String>,
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The script ran and exited 0.
    Succeeded,
    /// The script is not in place and nothing may place it (exit 64). Nothing ran.
    ScriptDrift,
    /// The host key was refused (exit 65). Nothing was authenticated.
    HostKeyRefused,
    /// A step failed: which one, and its exit status.
    Failed(Step, u32),
}

/// A step, for logs and [`Outcome::Failed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Creating the run account and authorizing its key.
    PlaceAccount,
    /// Writing the script.
    PlaceScript,
    /// Writing the sudo rule.
    PlaceSudoers,
    /// Removing the placement key.
    Revoke,
    /// The install script itself.
    Run,
}

/// Runs the steps. A transport error other than a refused host key is returned as `Err`.
pub async fn bootstrap<R: Remote>(r: &mut R, plan: &Plan) -> Result<Outcome, R::Error> {
    match steps(r, plan).await {
        Err(e) if r.refused(&e) => Ok(Outcome::HostKeyRefused),
        other => other,
    }
}

async fn steps<R: Remote>(r: &mut R, plan: &Plan) -> Result<Outcome, R::Error> {
    // On a fresh host the run account does not exist yet, so its login is refused. Only
    // placement can fix that; without a placement credential the refusal is the error.
    let in_place = match check(r, plan).await {
        Ok(v) => v,
        Err(e) if r.auth_refused(&e) && plan.placement.is_some() => {
            tracing::info!("the run account cannot log in yet; placement will create it");
            false
        }
        Err(e) => return Err(e),
    };
    if !in_place {
        let Some(p) = &plan.placement else {
            tracing::warn!("the install script is not in place and no placement credential is set");
            return Ok(Outcome::ScriptDrift);
        };
        let Some(key) = &plan.run_public_key else {
            tracing::warn!("a password run credential cannot be provisioned by placement");
            return Ok(Outcome::Failed(Step::PlaceAccount, 0));
        };
        tracing::info!("placing the run account, the install script and the sudo rule");
        let o = r
            .exec(
                Who::Placement,
                &remote::place_account(p.as_root),
                &remote::account_input(&plan.run_user, key),
                false,
            )
            .await?;
        if o.status != 0 {
            return Ok(Outcome::Failed(Step::PlaceAccount, o.status));
        }
        let o = r
            .exec(
                Who::Placement,
                &remote::place_script(p.as_root),
                &plan.script,
                false,
            )
            .await?;
        if o.status != 0 {
            return Ok(Outcome::Failed(Step::PlaceScript, o.status));
        }
        let rule = remote::sudoers_rule(&plan.run_user);
        let o = r
            .exec(
                Who::Placement,
                &remote::place_sudoers(p.as_root),
                rule.as_bytes(),
                false,
            )
            .await?;
        if o.status != 0 {
            return Ok(Outcome::Failed(Step::PlaceSudoers, o.status));
        }
        if !check(r, plan).await? {
            tracing::warn!("the install script is still not in place after placement");
            return Ok(Outcome::ScriptDrift);
        }
        if let Some(blob) = &p.revoke_blob {
            tracing::info!("removing the placement key from the host");
            let o = r
                .exec(Who::Placement, &remote::revoke(), blob.as_bytes(), false)
                .await?;
            if o.status != 0 {
                return Ok(Outcome::Failed(Step::Revoke, o.status));
            }
        }
    }
    tracing::info!("running the install script");
    let o = r.exec(Who::Run, remote::RUN, &plan.run_stdin, true).await?;
    if o.status == 0 {
        Ok(Outcome::Succeeded)
    } else {
        Ok(Outcome::Failed(Step::Run, o.status))
    }
}

async fn check<R: Remote>(r: &mut R, plan: &Plan) -> Result<bool, R::Error> {
    let o = r.exec(Who::Run, remote::CHECK, &[], false).await?;
    Ok(remote::in_place(o.status, &o.stdout, &plan.script_sha256))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Debug, thiserror::Error)]
    enum FakeError {
        #[error("host key refused")]
        Refused,
        #[error("connection reset")]
        Reset,
        #[error("the host refused the credential")]
        AuthRefused,
    }

    /// Answers commands in order and records them.
    struct Fake {
        answers: VecDeque<Result<Output, FakeError>>,
        calls: Vec<(Who, String, Vec<u8>, bool)>,
    }

    impl Fake {
        fn new(answers: Vec<Result<Output, FakeError>>) -> Self {
            Self {
                answers: answers.into(),
                calls: Vec::new(),
            }
        }

        fn commands(&self) -> Vec<(Who, &str)> {
            self.calls
                .iter()
                .map(|(w, c, _, _)| (*w, c.as_str()))
                .collect()
        }
    }

    impl Remote for Fake {
        type Error = FakeError;

        #[allow(clippy::unused_async_trait_impl)] // the Remote trait's signature
        async fn exec(
            &mut self,
            who: Who,
            command: &str,
            stdin: &[u8],
            stream: bool,
        ) -> Result<Output, FakeError> {
            self.calls
                .push((who, command.to_string(), stdin.to_vec(), stream));
            self.answers.pop_front().expect("an answer per command")
        }

        fn refused(&self, e: &FakeError) -> bool {
            matches!(e, FakeError::Refused)
        }

        fn auth_refused(&self, e: &FakeError) -> bool {
            matches!(e, FakeError::AuthRefused)
        }
    }

    const SUM: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[allow(clippy::unnecessary_wraps)] // shaped like the fake's answers
    fn ok(status: u32) -> Result<Output, FakeError> {
        Ok(Output {
            status,
            stdout: Vec::new(),
        })
    }

    #[allow(clippy::unnecessary_wraps)] // shaped like the fake's answers
    fn in_place() -> Result<Output, FakeError> {
        Ok(Output {
            status: 0,
            stdout: format!("0 0 755\n0 0 755\n{SUM}  /opt/faubourg/install.sh\n").into_bytes(),
        })
    }

    fn plan(placement: Option<Placement>) -> Plan {
        Plan {
            script_sha256: SUM.into(),
            script: b"#!/bin/sh\n".to_vec(),
            run_user: "faubourg".into(),
            run_public_key: Some("ssh-ed25519 AAAArunkey".into()),
            placement,
            run_stdin: Vec::new(),
        }
    }

    #[allow(clippy::unnecessary_wraps)] // shaped like Plan::placement
    fn placement(as_root: bool, revoke: bool) -> Option<Placement> {
        Some(Placement {
            as_root,
            revoke_blob: revoke.then(|| "AAAAblob".to_string()),
        })
    }

    fn run<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn a_host_in_place_only_runs_the_script() {
        let mut f = Fake::new(vec![in_place(), ok(0)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(None))).unwrap(),
            Outcome::Succeeded
        );
        assert_eq!(
            f.commands(),
            vec![(Who::Run, remote::CHECK), (Who::Run, remote::RUN)]
        );
        assert!(f.calls[1].3, "the script's output is streamed to the log");
        assert!(
            f.calls.iter().all(|c| c.0 == Who::Run),
            "no privileged login"
        );
    }

    #[test]
    fn drift_without_placement_runs_nothing() {
        let mut f = Fake::new(vec![ok(1)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(None))).unwrap(),
            Outcome::ScriptDrift
        );
        assert_eq!(f.commands(), vec![(Who::Run, remote::CHECK)]);
    }

    #[test]
    fn placement_writes_account_script_and_rule_checks_again_then_runs() {
        let mut f = Fake::new(vec![ok(1), ok(0), ok(0), ok(0), in_place(), ok(0)]);
        let p = plan(placement(false, false));
        assert_eq!(run(bootstrap(&mut f, &p)).unwrap(), Outcome::Succeeded);
        let account = remote::place_account(false);
        let script = remote::place_script(false);
        let sudoers = remote::place_sudoers(false);
        assert_eq!(
            f.commands(),
            vec![
                (Who::Run, remote::CHECK),
                (Who::Placement, account.as_str()),
                (Who::Placement, script.as_str()),
                (Who::Placement, sudoers.as_str()),
                (Who::Run, remote::CHECK),
                (Who::Run, remote::RUN),
            ]
        );
        assert_eq!(
            f.calls[1].2,
            remote::account_input("faubourg", "ssh-ed25519 AAAArunkey"),
            "the account and its public key go on stdin"
        );
        assert_eq!(f.calls[2].2, p.script, "the script goes on stdin");
        assert_eq!(f.calls[3].2, remote::sudoers_rule("faubourg").into_bytes());
    }

    #[test]
    fn a_fresh_host_whose_run_account_is_missing_is_placed() {
        let mut f = Fake::new(vec![
            Err(FakeError::AuthRefused),
            ok(0),
            ok(0),
            ok(0),
            in_place(),
            ok(0),
        ]);
        let p = plan(placement(true, false));
        assert_eq!(run(bootstrap(&mut f, &p)).unwrap(), Outcome::Succeeded);
        assert_eq!(f.commands()[1].1, remote::place_account(true));
    }

    #[test]
    fn a_refused_run_login_without_placement_is_an_error_not_drift() {
        let mut f = Fake::new(vec![Err(FakeError::AuthRefused)]);
        assert!(run(bootstrap(&mut f, &plan(None))).is_err());
    }

    #[test]
    fn a_password_run_credential_cannot_be_provisioned() {
        let mut f = Fake::new(vec![Err(FakeError::AuthRefused)]);
        let mut p = plan(placement(true, false));
        p.run_public_key = None;
        assert_eq!(
            run(bootstrap(&mut f, &p)).unwrap(),
            Outcome::Failed(Step::PlaceAccount, 0)
        );
        assert_eq!(f.calls.len(), 1, "nothing is placed");
    }

    #[test]
    fn revoke_follows_a_successful_placement() {
        let mut f = Fake::new(vec![ok(1), ok(0), ok(0), ok(0), in_place(), ok(0), ok(0)]);
        let p = plan(placement(true, true));
        assert_eq!(run(bootstrap(&mut f, &p)).unwrap(), Outcome::Succeeded);
        let revoke = remote::revoke();
        assert_eq!(f.commands()[5], (Who::Placement, revoke.as_str()));
        assert_eq!(f.calls[5].2, b"AAAAblob");
        assert_eq!(f.commands()[6], (Who::Run, remote::RUN));
    }

    #[test]
    fn no_revoke_when_nothing_was_placed() {
        let mut f = Fake::new(vec![in_place(), ok(0)]);
        run(bootstrap(&mut f, &plan(placement(true, true)))).unwrap();
        assert!(f.calls.iter().all(|c| c.0 == Who::Run));
    }

    #[test]
    fn a_failed_placement_stops_before_the_run() {
        let mut f = Fake::new(vec![ok(1), ok(4)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(placement(false, false)))).unwrap(),
            Outcome::Failed(Step::PlaceAccount, 4)
        );
        let mut f = Fake::new(vec![ok(1), ok(0), ok(1)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(placement(false, false)))).unwrap(),
            Outcome::Failed(Step::PlaceScript, 1)
        );
        let mut f = Fake::new(vec![ok(1), ok(0), ok(0), ok(3)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(placement(false, false)))).unwrap(),
            Outcome::Failed(Step::PlaceSudoers, 3)
        );
        assert!(f.calls.iter().all(|c| c.1 != remote::RUN));
    }

    #[test]
    fn still_not_in_place_after_placement_is_drift() {
        let mut f = Fake::new(vec![ok(1), ok(0), ok(0), ok(0), ok(1)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(placement(false, true)))).unwrap(),
            Outcome::ScriptDrift
        );
        assert_eq!(f.calls.len(), 5, "no revoke and no run");
    }

    #[test]
    fn a_failed_revoke_stops_before_the_run() {
        let mut f = Fake::new(vec![ok(1), ok(0), ok(0), ok(0), in_place(), ok(2)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(placement(true, true)))).unwrap(),
            Outcome::Failed(Step::Revoke, 2)
        );
    }

    #[test]
    fn a_failing_script_is_reported_with_its_status() {
        let mut f = Fake::new(vec![in_place(), ok(7)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(None))).unwrap(),
            Outcome::Failed(Step::Run, 7)
        );
    }

    #[test]
    fn a_refused_host_key_is_its_own_outcome() {
        let mut f = Fake::new(vec![Err(FakeError::Refused)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(None))).unwrap(),
            Outcome::HostKeyRefused
        );
        let mut f = Fake::new(vec![ok(1), Err(FakeError::Refused)]);
        assert_eq!(
            run(bootstrap(&mut f, &plan(placement(true, false)))).unwrap(),
            Outcome::HostKeyRefused,
            "the placement connection saw another key"
        );
    }

    #[test]
    fn other_transport_errors_are_errors() {
        let mut f = Fake::new(vec![Err(FakeError::Reset)]);
        assert!(run(bootstrap(&mut f, &plan(None))).is_err());
    }

    #[test]
    fn the_credential_bundle_goes_to_the_script_and_nowhere_else() {
        let mut f = Fake::new(vec![ok(1), ok(0), ok(0), ok(0), in_place(), ok(0)]);
        let mut p = plan(placement(true, false));
        p.run_stdin = b"faubourg-provider-credential v1\n".to_vec();
        run(bootstrap(&mut f, &p)).unwrap();
        let (last, rest) = f.calls.split_last().unwrap();
        assert_eq!(last.1, remote::RUN);
        assert_eq!(last.2, p.run_stdin);
        assert!(
            rest.iter().all(|c| c.2 != p.run_stdin),
            "never to placement or check"
        );
    }
}

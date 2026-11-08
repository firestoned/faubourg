// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! [`Remote`] over SSH, on russh (ADR-0010 decision 1).
//!
//! The host key is judged in [`Handler::check_server_key`], which russh calls after verifying
//! the key exchange signature and before any authentication is sent. One [`Verifier`] is
//! shared by every connection of the run, so the placement and run connections must see the
//! same key. One connection per credential, opened on first use.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use faubourg_api::credentials::{Auth, SshCredentials};
use russh::client::{self, Handle, Handler};
use russh::keys::{Certificate, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect};
use tokio::io::AsyncWriteExt;

use crate::hostkey::Verifier;
use crate::steps::{Output, Remote, Who};

/// Time to connect, check the host key and authenticate, per connection (ADR-0010 decision 9).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// The most of a command's output kept when it is not streamed.
const MAX_OUTPUT: usize = 64 * 1024;

/// Why a command could not run. Never carries a credential or a command line.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// The host key was refused; nothing was authenticated.
    #[error("the host key was refused")]
    HostKeyRefused,
    /// The credential could not be used: unreadable, wrong passphrase, or an RSA key.
    #[error("the {0} credential cannot be used: {1}")]
    Credential(&'static str, &'static str),
    /// The server refused the credential.
    #[error("the host refused the {0} credential")]
    AuthRefused(&'static str),
    /// No connection within [`CONNECT_TIMEOUT`].
    #[error("timed out connecting as the {0} credential")]
    Timeout(&'static str),
    /// The command has no credential to run as.
    #[error("no {0} credential is mounted")]
    NoCredential(&'static str),
    /// The SSH transport failed.
    #[error("ssh: {0}")]
    Transport(#[from] russh::Error),
    /// Writing the streamed output failed.
    #[error("writing output: {0}")]
    Io(#[from] std::io::Error),
}

struct Client {
    verifier: Arc<Mutex<Verifier>>,
    refused: Arc<Mutex<bool>>,
}

impl Handler for Client {
    type Error = SshError;

    #[allow(clippy::unused_async_trait_impl)] // the Handler trait's signature
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, SshError> {
        // Host certificates are refused until there is a CA to trust (ADR-0010 decision 2).
        let ok = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                self.verifier.lock().is_ok_and(|mut v| v.check(key))
            }
            PublicKeyOrCertificate::Certificate(_) => false,
        };
        if !ok && let Ok(mut r) = self.refused.lock() {
            *r = true;
        }
        Ok(ok)
    }
}

/// A host, reached over SSH with the run and, optionally, placement credentials.
pub struct Ssh {
    address: String,
    port: u16,
    run: SshCredentials,
    placement: Option<SshCredentials>,
    verifier: Arc<Mutex<Verifier>>,
    refused: Arc<Mutex<bool>>,
    sessions: [Option<Handle<Client>>; 2],
}

impl Ssh {
    /// A host to reach at `address:port`, judging its key with `verifier`.
    pub fn new(
        address: String,
        port: u16,
        run: SshCredentials,
        placement: Option<SshCredentials>,
        verifier: Verifier,
    ) -> Self {
        Self {
            address,
            port,
            run,
            placement,
            verifier: Arc::new(Mutex::new(verifier)),
            refused: Arc::new(Mutex::new(false)),
            sessions: [None, None],
        }
    }

    /// The fingerprint of the host key the run accepted, once one was.
    pub fn accepted_host_key(&self) -> Option<String> {
        self.verifier.lock().ok().and_then(|v| v.accepted())
    }

    /// Closes every connection.
    pub async fn close(&mut self) {
        for s in self.sessions.iter_mut().filter_map(Option::take) {
            let _ = s.disconnect(Disconnect::ByApplication, "", "en").await;
        }
    }

    fn was_refused(&self) -> bool {
        self.refused.lock().map_or(true, |r| *r)
    }

    async fn session(&mut self, who: Who) -> Result<&mut Handle<Client>, SshError> {
        let (slot, name) = match who {
            Who::Run => (0, "run"),
            Who::Placement => (1, "placement"),
        };
        if self.sessions[slot].is_none() {
            let cred = match who {
                Who::Run => &self.run,
                Who::Placement => self
                    .placement
                    .as_ref()
                    .ok_or(SshError::NoCredential(name))?,
            };
            let auth = credential(cred, name)?;
            let handler = Client {
                verifier: self.verifier.clone(),
                refused: self.refused.clone(),
            };
            let config = Arc::new(client::Config {
                inactivity_timeout: Some(Duration::from_secs(600)),
                ..client::Config::default()
            });
            let addr = (self.address.as_str(), self.port);
            let connect = async {
                let mut handle = client::connect(config, addr, handler).await?;
                let user = cred.username.clone();
                let result = match auth {
                    Material::Key(key, None) => {
                        handle
                            .authenticate_publickey(
                                user,
                                PrivateKeyWithHashAlg::new(Arc::new(*key), None),
                            )
                            .await?
                    }
                    Material::Key(key, Some(cert)) => {
                        handle
                            .authenticate_openssh_cert(user, Arc::new(*key), *cert)
                            .await?
                    }
                    Material::Password(p) => handle.authenticate_password(user, p).await?,
                };
                if result.success() {
                    Ok(handle)
                } else {
                    Err(SshError::AuthRefused(name))
                }
            };
            let handle = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
                Err(_) => return Err(SshError::Timeout(name)),
                Ok(Err(_)) if self.was_refused() => return Err(SshError::HostKeyRefused),
                Ok(Err(e)) => return Err(e),
                Ok(Ok(h)) => h,
            };
            self.sessions[slot] = Some(handle);
        }
        self.sessions[slot]
            .as_mut()
            .ok_or(SshError::NoCredential(name))
    }
}

enum Material {
    Key(Box<PrivateKey>, Option<Box<Certificate>>),
    Password(String),
}

fn credential(c: &SshCredentials, name: &'static str) -> Result<Material, SshError> {
    match &c.auth {
        Auth::Key {
            private_key,
            passphrase,
            certificate,
        } => {
            let text = std::str::from_utf8(private_key.expose())
                .map_err(|_| SshError::Credential(name, "the private key is not text"))?;
            let pass = passphrase
                .as_ref()
                .map(|p| std::str::from_utf8(p.expose()))
                .transpose()
                .map_err(|_| SshError::Credential(name, "the passphrase is not UTF-8"))?;
            let key = russh::keys::decode_secret_key(text, pass).map_err(|_| {
                SshError::Credential(name, "the private key cannot be read or decrypted")
            })?;
            if key.algorithm().is_rsa() {
                return Err(SshError::Credential(
                    name,
                    "RSA keys are refused (ADR-0010)",
                ));
            }
            let cert = certificate
                .as_ref()
                .map(|c| {
                    std::str::from_utf8(c)
                        .ok()
                        .and_then(|t| Certificate::from_openssh(t.trim()).ok())
                        .map(Box::new)
                        .ok_or(SshError::Credential(name, "the certificate cannot be read"))
                })
                .transpose()?;
            Ok(Material::Key(Box::new(key), cert))
        }
        Auth::Password(p) => std::str::from_utf8(p.expose())
            .map(|s| Material::Password(s.to_string()))
            .map_err(|_| SshError::Credential(name, "the password is not UTF-8")),
    }
}

impl Remote for Ssh {
    type Error = SshError;

    async fn exec(
        &mut self,
        who: Who,
        command: &str,
        stdin: &[u8],
        stream: bool,
    ) -> Result<Output, SshError> {
        let session = self.session(who).await?;
        let mut channel = session.channel_open_session().await?;
        channel.exec(true, command).await?;
        if !stdin.is_empty() {
            channel.data(stdin).await?;
        }
        channel.eof().await?;
        let mut out = Output {
            // A command that reports no status (killed by a signal) has failed.
            status: 255,
            stdout: Vec::new(),
        };
        let mut log = tokio::io::stderr();
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    if stream {
                        log.write_all(&data).await?;
                    } else if out.stdout.len() < MAX_OUTPUT {
                        out.stdout.extend_from_slice(&data);
                    }
                }
                ChannelMsg::ExtendedData { data, .. } if stream => log.write_all(&data).await?,
                ChannelMsg::ExitStatus { exit_status } => out.status = exit_status,
                _ => {}
            }
        }
        log.flush().await?;
        Ok(out)
    }

    fn refused(&self, e: &SshError) -> bool {
        matches!(e, SshError::HostKeyRefused)
    }

    fn auth_refused(&self, e: &SshError) -> bool {
        matches!(e, SshError::AuthRefused(_))
    }
}

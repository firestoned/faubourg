// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The commands the bootstrap sends to a host (ADR-0010 decision 5). Every one is a constant.
//! Nothing from the Host, a Secret or the script is ever part of a command string: data
//! reaches the host on the channel's standard input.
//!
//! The programs are POSIX `sh`, use no single quote (they are wrapped in one), and set their
//! own `PATH`, so neither the login shell nor the account's environment changes what runs.

use faubourg_api::host::HOST_SCRIPT_PATH;

/// Reads the owner, group and mode of `/opt/faubourg` and the script, then the script's
/// sha256. Run with the scoped credential, which can read but never write them.
pub const CHECK: &str = "sh -c 'PATH=/usr/bin:/bin; export LC_ALL=C; \
stat -c \"%u %g %a\" /opt/faubourg /opt/faubourg/install.sh && \
sha256sum /opt/faubourg/install.sh'";

/// Runs the script under the one sudo rule (ADR-0005 decision 2).
pub const RUN: &str = "sudo -n /opt/faubourg/install.sh";

/// Writes the script from stdin to `/opt/faubourg/install.sh`, `root:root 0755`, through a
/// temporary file in the same directory, so a reader never sees half a script.
const PLACE_SCRIPT_PROGRAM: &str = "set -eu; PATH=/usr/sbin:/usr/bin:/sbin:/bin; umask 077; \
mkdir -p /opt/faubourg; chown 0:0 /opt/faubourg; chmod 0755 /opt/faubourg; \
t=$(mktemp /opt/faubourg/.install.XXXXXX); \
cat > \"$t\" || { rm -f \"$t\"; exit 1; }; \
chown 0:0 \"$t\"; chmod 0755 \"$t\"; mv -f \"$t\" /opt/faubourg/install.sh";

/// Writes the sudoers drop-in from stdin to `/etc/sudoers.d/faubourg`, `root:root 0440`,
/// only if `visudo` accepts it. The temporary name starts with a dot, which sudo ignores.
const PLACE_SUDOERS_PROGRAM: &str = "set -eu; PATH=/usr/sbin:/usr/bin:/sbin:/bin; umask 077; \
t=$(mktemp /etc/sudoers.d/.faubourg.XXXXXX); \
cat > \"$t\" || { rm -f \"$t\"; exit 1; }; \
chown 0:0 \"$t\"; chmod 0440 \"$t\"; \
visudo -cf \"$t\" >/dev/null || { rm -f \"$t\"; exit 3; }; \
mv -f \"$t\" /etc/sudoers.d/faubourg";

/// Removes every `authorized_keys` line of the logged-in account holding the key blob read
/// from stdin. A `grep` error leaves the file untouched.
const REVOKE_PROGRAM: &str = "set -u; PATH=/usr/bin:/bin; f=\"$HOME/.ssh/authorized_keys\"; \
[ -f \"$f\" ] || exit 0; k=$(cat); [ -n \"$k\" ] || exit 2; \
t=$(mktemp \"$f.XXXXXX\") || exit 2; \
grep -vF -e \"$k\" \"$f\" > \"$t\"; rc=$?; \
if [ \"$rc\" -gt 1 ]; then rm -f \"$t\"; exit 2; fi; \
chmod 0600 \"$t\" && mv -f \"$t\" \"$f\"";

/// The placement command for the script: as root directly, or through `sudo -n`.
pub fn place_script(as_root: bool) -> String {
    privileged(as_root, PLACE_SCRIPT_PROGRAM)
}

/// The placement command for the sudoers drop-in.
pub fn place_sudoers(as_root: bool) -> String {
    privileged(as_root, PLACE_SUDOERS_PROGRAM)
}

/// The revoke command. It runs as the placement account itself: it edits that account's own
/// `authorized_keys`.
pub fn revoke() -> String {
    format!("sh -c '{REVOKE_PROGRAM}'")
}

fn privileged(as_root: bool, program: &str) -> String {
    if as_root {
        format!("sh -c '{program}'")
    } else {
        format!("sudo -n sh -c '{program}'")
    }
}

/// The one sudo rule (ADR-0005 decision 2), for a run account whose name `SshCredentials`
/// already validated as a plain login name.
pub fn sudoers_rule(run_user: &str) -> String {
    format!("{run_user} ALL=(root) NOPASSWD: {HOST_SCRIPT_PATH} \"\"\n")
}

/// Whether [`CHECK`]'s output shows the script in place: both paths `root:root` mode
/// `0755`, and the script's sha256 equal to `want` (lowercase hex). Anything else, including
/// output it cannot read, is not in place.
pub fn in_place(status: u32, stdout: &[u8], want: &str) -> bool {
    if status != 0 {
        return false;
    }
    let Ok(out) = std::str::from_utf8(stdout) else {
        return false;
    };
    let lines: Vec<&str> = out.lines().collect();
    let [dir, file, sum] = lines.as_slice() else {
        return false;
    };
    let owned = |l: &str| l.trim() == "0 0 755";
    let hash = sum.split_whitespace().next().unwrap_or_default();
    owned(dir) && owned(file) && hash == want
}

/// The base64 blob of an OpenSSH public key line (`<type> <blob> [comment]`).
pub fn key_blob(openssh_public: &str) -> Option<&str> {
    openssh_public.split_whitespace().nth(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUM: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn check_output(dir: &str, file: &str, sum: &str) -> Vec<u8> {
        format!("{dir}\n{file}\n{sum}  /opt/faubourg/install.sh\n").into_bytes()
    }

    #[test]
    fn programs_survive_single_quoting() {
        for p in [PLACE_SCRIPT_PROGRAM, PLACE_SUDOERS_PROGRAM, REVOKE_PROGRAM] {
            assert!(!p.contains('\''), "{p}");
        }
        assert_eq!(CHECK.matches('\'').count(), 2);
    }

    #[test]
    fn placement_goes_through_sudo_unless_root() {
        assert!(place_script(true).starts_with("sh -c '"));
        assert!(place_script(false).starts_with("sudo -n sh -c '"));
        assert!(place_sudoers(false).starts_with("sudo -n sh -c '"));
        assert!(!revoke().contains("sudo"));
    }

    #[test]
    fn the_paths_are_the_ones_adr_0005_names() {
        assert_eq!(HOST_SCRIPT_PATH, "/opt/faubourg/install.sh");
        assert!(RUN.ends_with(HOST_SCRIPT_PATH));
        assert!(PLACE_SCRIPT_PROGRAM.contains("mv -f \"$t\" /opt/faubourg/install.sh"));
        assert!(PLACE_SUDOERS_PROGRAM.contains("visudo -cf"));
    }

    #[test]
    fn the_sudo_rule_allows_the_script_and_no_arguments() {
        assert_eq!(
            sudoers_rule("faubourg"),
            "faubourg ALL=(root) NOPASSWD: /opt/faubourg/install.sh \"\"\n"
        );
    }

    #[test]
    fn in_place_needs_owner_mode_and_hash() {
        assert!(in_place(0, &check_output("0 0 755", "0 0 755", SUM), SUM));
        assert!(!in_place(1, &check_output("0 0 755", "0 0 755", SUM), SUM));
        assert!(!in_place(
            0,
            &check_output("0 0 755", "0 0 755", &"0".repeat(64)),
            SUM
        ));
        assert!(!in_place(0, &check_output("0 0 777", "0 0 755", SUM), SUM));
        assert!(!in_place(
            0,
            &check_output("0 0 755", "1000 0 755", SUM),
            SUM
        ));
        assert!(!in_place(0, &check_output("0 0 755", "0 0 775", SUM), SUM));
        assert!(!in_place(0, b"", SUM));
        assert!(!in_place(0, b"0 0 755\n0 0 755\n", SUM));
        assert!(!in_place(0, &[0xff, 0xfe], SUM));
    }

    #[test]
    fn the_key_blob_is_the_second_field() {
        assert_eq!(key_blob("ssh-ed25519 AAAAblob comment"), Some("AAAAblob"));
        assert_eq!(key_blob("ssh-ed25519"), None);
    }
}

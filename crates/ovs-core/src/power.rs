//! Restarting and stopping the machine.
//!
//! The two actions the Service Console offers, and the only
//! place in the server that runs anything outside itself.
//!
//! # It asks the init system, not the kernel
//!
//! `systemctl reboot` lets the machine stop what it is doing first: the show is
//! written down, the database is closed, the units come down in the order they
//! were brought up. Calling the kernel directly would be faster and would lose
//! whatever had not reached the disk, which on a server whose whole job is to
//! remember what was on stage is the wrong trade.
//!
//! # A refusal is an answer
//!
//! A server running as an unprivileged user cannot reboot the machine unless it
//! has been given permission to, and that is a deployment decision rather than
//! something to work around. When it cannot, it says so, with what it tried —
//! an operator can act on that, where a button that quietly does nothing is
//! something they will press repeatedly at the worst possible moment.
//!
//! The decision is made in `deploy/polkit/50-openvideoserver-power.rules`, and
//! both installation paths lay it down. It has to be made somewhere: logind
//! wants `auth_admin_keep` from any caller that is not an active local session,
//! and a system service has no session to be active in — so without that file
//! every press is refused with exit status 1, which is what happened on the
//! reference machine until 2026-09-25.

use std::process::Command;

use ovs_proto::http::PowerAction;

/// What running a power command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Whether the machine took the instruction.
    pub accepted: bool,
    /// What was run, or why nothing was.
    pub detail: String,
}

/// Returns the command that performs an action on this platform.
///
/// Separate from running it so that what would be run is testable on a machine
/// nobody wants rebooted.
#[must_use]
pub fn command_for(action: PowerAction) -> (&'static str, Vec<&'static str>) {
    if cfg!(windows) {
        // The development platform. `-t 0` is "now", and without `-f` Windows
        // will wait for an application that refuses to close, which on a server
        // means waiting forever for nobody.
        let switch = match action {
            PowerAction::Reboot => "-r",
            PowerAction::Shutdown => "-s",
        };
        return ("shutdown", vec![switch, "-t", "0", "-f"]);
    }

    match action {
        PowerAction::Reboot => ("systemctl", vec!["reboot"]),
        PowerAction::Shutdown => ("systemctl", vec!["poweroff"]),
    }
}

/// Asks the machine to reboot or stop.
///
/// Returns what happened. Nothing here waits: the process that is asking is one
/// of the things about to be stopped.
#[must_use]
pub fn request(action: PowerAction) -> Outcome {
    let (program, arguments) = command_for(action);
    let printable = format!("{program} {}", arguments.join(" "));

    // **`output`, not `status`.** Standard error is where the machine says why,
    // and letting it go to the journal put the one useful sentence somewhere the
    // person standing at the screen cannot see it.
    match Command::new(program).args(&arguments).output() {
        Ok(result) if result.status.success() => Outcome {
            accepted: true,
            detail: printable,
        },
        Ok(result) => Outcome {
            accepted: false,
            detail: refused(&printable, &result.status.to_string(), &result.stderr),
        },
        Err(error) => Outcome {
            accepted: false,
            detail: format!("`{printable}` could not be run: {error}"),
        },
    }
}

/// What to say about a power command the machine would not run.
///
/// **With what it actually said.** This used to be `check the unit's
/// permissions` and nothing else, which is a guess printed as an answer. The
/// reason is on `systemctl`'s standard error, it is specific, and the two
/// readings want different fixes: `Interactive authentication required` is a
/// polkit rule that is not matching, and `Please retry operation after closing
/// inhibitors` is something holding a shutdown lock. An operator standing in
/// front of the machine is the person who has to know which.
///
/// The **last** line that says anything, because `systemctl` prints a warning
/// about not being able to set the wall message first and the refusal after it.
/// Taking one line rather than all of them also bounds what reaches the screen
/// without a length to argue about.
fn refused(printable: &str, status: &str, stderr: &[u8]) -> String {
    let said = String::from_utf8_lossy(stderr);
    let reason = said.lines().map(str::trim).rfind(|line| !line.is_empty());

    reason.map_or_else(
        || format!("`{printable}` was refused ({status}); check the unit's permissions"),
        |reason| format!("`{printable}` was refused ({status}): {reason}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_action_has_its_own_command() {
        // A power endpoint whose two actions ran the same thing would be one an
        // operator could not trust, and the difference is not something to
        // discover on the night.
        let reboot = command_for(PowerAction::Reboot);
        let shutdown = command_for(PowerAction::Shutdown);
        assert_ne!(reboot, shutdown);
    }

    #[test]
    fn a_refusal_says_what_the_machine_said() {
        // **The two that matter, and they are not the same fault.** One is a
        // polkit rule that does not match; the other is something holding a
        // shutdown lock. Until 2026-09-26 both came out as "check the unit's
        // permissions", which is neither.
        let polkit = refused(
            "systemctl reboot",
            "exit status: 1",
            b"Call to Reboot failed: Interactive authentication required.\n",
        );
        assert!(
            polkit.contains("Interactive authentication required"),
            "the reason was thrown away: {polkit}"
        );

        // `systemctl` says it cannot set the wall message first. The refusal is
        // the line after it, and the one worth showing.
        let inhibited = refused(
            "systemctl poweroff",
            "exit status: 1",
            b"Failed to set wall message, ignoring: Access denied\n\
              Please retry operation after closing inhibitors and logging out other users.\n",
        );
        assert!(
            inhibited.contains("closing inhibitors"),
            "the wall-message warning was shown instead of the refusal: {inhibited}"
        );
        assert!(
            !inhibited.contains("wall message"),
            "the warning was shown as well as the refusal: {inhibited}"
        );

        // Nothing said: the old wording, which is a guess and is labelled as
        // advice rather than as what happened.
        let quiet = refused("systemctl reboot", "exit status: 1", b"   \n\n");
        assert!(quiet.contains("check the unit's permissions"), "{quiet}");
        assert!(quiet.contains("exit status: 1"), "{quiet}");
    }

    #[test]
    fn the_machine_is_asked_through_its_init_system() {
        // Rather than through the kernel: the show has to be written down
        // before the lights go out.
        if cfg!(windows) {
            return;
        }
        for action in [PowerAction::Reboot, PowerAction::Shutdown] {
            assert_eq!(command_for(action).0, "systemctl");
        }
    }
}

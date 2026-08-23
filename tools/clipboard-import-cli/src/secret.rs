//! Getting a `.rayconfig` password without leaving it lying around.
//!
//! Never a `--password` flag: a process's arguments are readable by every other
//! process on the machine, and the shell writes them to its history. The
//! password comes from a terminal that is not echoing it, or from a pipe when
//! there is no terminal to ask.

use std::io::{BufRead, IsTerminal, Write};

use clipboard_import::RayconfigSecret;

use crate::CliFailure;

/// Largest password this will read. Long enough for any passphrase, short
/// enough that a redirected file cannot be slurped whole by accident.
const MAX_PASSWORD_BYTES: usize = 1024;

/// Reads the password for an encrypted export.
///
/// `from_stdin` forces the pipe path even on a terminal, which is what a script
/// needs; without it a terminal is prompted and a pipe is read.
pub(crate) fn read_password(from_stdin: bool) -> Result<RayconfigSecret, CliFailure> {
    let stdin = std::io::stdin();
    if from_stdin || !stdin.is_terminal() {
        return read_line(stdin.lock());
    }
    prompt_without_echo()
}

fn read_line(reader: impl BufRead) -> Result<RayconfigSecret, CliFailure> {
    let mut line = String::new();
    // One byte over the limit is read on purpose: it is how an oversized line
    // is told apart from one that exactly fits.
    std::io::BufRead::read_line(&mut reader.take(MAX_PASSWORD_BYTES as u64 + 1), &mut line)
        .map_err(|_| CliFailure::new("password_unreadable"))?;
    if line.len() > MAX_PASSWORD_BYTES {
        return Err(CliFailure::new("password_too_long"));
    }
    // A trailing newline belongs to the transport, not the password.
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    if line.is_empty() {
        return Err(CliFailure::new("password_missing"));
    }
    Ok(RayconfigSecret::new(line))
}

/// Asks on the terminal with echo turned off, and turns it back on afterwards.
#[cfg(unix)]
fn prompt_without_echo() -> Result<RayconfigSecret, CliFailure> {
    use rustix::termios::{OptionalActions, tcgetattr, tcsetattr};

    let stdin = std::io::stdin();
    let mut stderr = std::io::stderr();
    // The prompt goes to stderr so that stdout stays machine-readable.
    let _ = write!(stderr, "Password for the .rayconfig file: ");
    let _ = stderr.flush();

    let original = tcgetattr(&stdin).map_err(|_| CliFailure::new("password_unreadable"))?;
    let mut quiet = original.clone();
    quiet.local_modes -= rustix::termios::LocalModes::ECHO;
    tcsetattr(&stdin, OptionalActions::Flush, &quiet)
        .map_err(|_| CliFailure::new("password_unreadable"))?;

    let secret = read_line(stdin.lock());

    // Restore the terminal even when the read failed; leaving a shell with echo
    // off is a worse outcome than the error being reported.
    let _ = tcsetattr(&stdin, OptionalActions::Flush, &original);
    let _ = writeln!(stderr);
    secret
}

#[cfg(not(unix))]
fn prompt_without_echo() -> Result<RayconfigSecret, CliFailure> {
    // No portable way to silence the terminal here, and echoing a password is
    // not an acceptable fallback. The pipe path still works.
    Err(CliFailure::new("password_prompt_unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_newline_is_transport_rather_than_password() {
        let Ok(secret) = read_line(&b"synthetic-password\n"[..]) else {
            panic!("a newline-terminated password must be accepted");
        };

        assert!(!format!("{secret:?}").contains("synthetic-password"));
    }

    #[test]
    fn an_empty_line_is_refused_rather_than_tried() {
        assert!(read_line(&b"\n"[..]).is_err());
        assert!(read_line(&b""[..]).is_err());
    }

    #[test]
    fn bounded_password_read_refuses_an_oversized_line() {
        let long = "x".repeat(MAX_PASSWORD_BYTES + 1);

        assert!(read_line(long.as_bytes()).is_err());
    }

    #[test]
    fn a_password_at_the_limit_is_accepted() {
        let exact = "x".repeat(MAX_PASSWORD_BYTES);

        assert!(read_line(exact.as_bytes()).is_ok());
    }

    #[test]
    fn a_failure_names_a_code_and_nothing_from_the_password() {
        let Err(failure) = read_line(&b"\n"[..]) else {
            panic!("an empty password must be refused");
        };

        assert_eq!(failure.code(), "password_missing");
    }
}

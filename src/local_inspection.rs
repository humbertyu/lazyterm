//! Bounded output collection for local process inspection tools.
use std::{
    io::{self, Read},
    os::fd::AsRawFd,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

struct InspectionChild(Child);

impl Drop for InspectionChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(crate) fn output(command: &mut Command) -> io::Result<Output> {
    output_with_timeout(command, TIMEOUT)
}

fn output_with_timeout(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    let mut child = InspectionChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut stdout = child.0.stdout.take().expect("piped stdout");
    let mut stderr = child.0.stderr.take().expect("piped stderr");
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_closed = false;
    let mut err_closed = false;
    let mut status = None;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Local process inspection timed out; retry after checking system load.",
            ));
        }
        if !out_closed {
            out_closed = drain(&mut stdout, &mut out)?;
        }
        if !err_closed {
            err_closed = drain(&mut stderr, &mut err)?;
        }
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if let Some(status) = status
            && out_closed
            && err_closed
        {
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    // Yield between streams and deadline checks even if a tool writes continuously.
    for _ in 0..8 {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(size) => {
                if bytes.len() + size > OUTPUT_LIMIT {
                    return Err(io::Error::other(
                        "Local process inspection output exceeded its limit.",
                    ));
                }
                bytes.extend_from_slice(&buffer[..size]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn helper(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "local_inspection::tests::inspection_process",
                "--nocapture",
            ])
            .env("LAZYTERM_INSPECTION_TEST", mode);
        command
    }

    #[test]
    fn blocked_inspection_times_out_and_a_subsequent_query_succeeds() {
        let started = Instant::now();
        let error =
            output_with_timeout(&mut helper("block"), Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        let result = output(&mut helper("output")).unwrap();
        assert!(result.status.success());
        assert!(
            result
                .stdout
                .windows(128 * 1024)
                .any(|bytes| bytes.iter().all(|byte| *byte == b'o'))
        );
        assert!(
            result
                .stderr
                .windows(128 * 1024)
                .any(|bytes| bytes.iter().all(|byte| *byte == b'e'))
        );
    }

    #[test]
    fn nonzero_exit_and_excessive_output_are_not_successful_empty_snapshots() {
        let result = output(&mut helper("failure")).unwrap();
        assert!(!result.status.success());
        // Large output can take longer on small CI pipes; test the cap independently
        // of the production deadline, which has its own blocked-query test.
        let error =
            output_with_timeout(&mut helper("overflow"), Duration::from_secs(30)).unwrap_err();
        assert!(error.to_string().contains("exceeded"));
    }

    #[test]
    #[ignore = "local inspection subprocess helper"]
    fn inspection_process() {
        match std::env::var("LAZYTERM_INSPECTION_TEST").as_deref() {
            Ok("block") => std::thread::sleep(Duration::from_secs(60)),
            Ok("output") => {
                io::stdout().write_all(&vec![b'o'; 128 * 1024]).unwrap();
                io::stderr().write_all(&vec![b'e'; 128 * 1024]).unwrap();
            }
            Ok("overflow") => io::stdout()
                .write_all(&vec![b'o'; OUTPUT_LIMIT + 1])
                .unwrap(),
            Ok("failure") => std::process::exit(7),
            _ => {}
        }
    }
}

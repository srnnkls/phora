//! Build programs and hooks, run with their output captured.

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::sync::progress::{CommandRun, CommandSubject, ProgressSink};

pub(crate) fn run_captured(
    sink: &dyn ProgressSink,
    subject: CommandSubject,
    display: &str,
    command: &mut Command,
) -> std::io::Result<ExitStatus> {
    sink.command_started(&subject, display);
    let started = Instant::now();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut stream| {
        let output = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(read) = stream.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                output
                    .lock()
                    .expect("child output poisoned")
                    .extend_from_slice(&chunk[..read]);
            }
        })
    })
    .collect();
    let status = child.wait()?;
    for reader in readers {
        let _ = reader.join();
    }
    let output =
        String::from_utf8_lossy(&output.lock().expect("child output poisoned")).into_owned();
    sink.command_finished(&CommandRun {
        subject,
        command: display.to_owned(),
        success: status.success(),
        elapsed: started.elapsed(),
        output,
    });
    Ok(status)
}

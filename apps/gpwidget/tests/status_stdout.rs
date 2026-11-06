//! `gpwidget status` must not abort when the parent has already closed stdout.
//! That is the Omarchy bar path during suspend / shell restart.

use std::io::{self, Read};
use std::process::{Command, Stdio};

#[test]
fn status_exits_zero_when_stdout_is_already_closed() {
  let runtime = tempfile::tempdir().unwrap();
  let (reader, writer) = io::pipe().unwrap();
  drop(reader);

  let mut child = Command::new(env!("CARGO_BIN_EXE_gpwidget"))
    .arg("status")
    .env("XDG_RUNTIME_DIR", runtime.path())
    .stdout(Stdio::from(writer))
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();

  let status = child.wait().unwrap();
  let mut stderr = String::new();
  if let Some(mut pipe) = child.stderr.take() {
    let _ = pipe.read_to_string(&mut stderr);
  }

  assert_eq!(
    status.code(),
    Some(0),
    "gpwidget status must exit 0 when stdout is gone; stderr={stderr:?} wait={status:?}"
  );
}

#[test]
fn status_still_prints_stack_down_when_stdout_is_open() {
  let runtime = tempfile::tempdir().unwrap();

  let output = Command::new(env!("CARGO_BIN_EXE_gpwidget"))
    .arg("status")
    .env("XDG_RUNTIME_DIR", runtime.path())
    .output()
    .unwrap();

  assert!(
    output.status.success(),
    "stderr={}",
    String::from_utf8_lossy(&output.stderr)
  );

  let stdout = String::from_utf8(output.stdout).unwrap();
  assert!(
    stdout.contains(r#""state":"stack-down""#),
    "expected stack-down snapshot, got {stdout:?}"
  );
}

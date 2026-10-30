//! Compatibility commands are thin launchers for `bb`, installed next to it, so the release
//! ships one engine instead of several copies of it.

use std::ffi::OsString;
use std::process::Command;

/// Run `bb PREFIX... ARGS...` and exit with its status.
pub fn exec_bb(prefix: &[&str]) -> ! {
    let bb = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(format!("bb{}", std::env::consts::EXE_SUFFIX)))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| "bb".into());
    let args: Vec<OsString> = prefix.iter().map(OsString::from).chain(std::env::args_os().skip(1)).collect();
    let mut command = Command::new(&bb);
    command.args(&args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = command.exec();
        eprintln!("could not start {}: {err}", bb.display());
        std::process::exit(127);
    }
    #[cfg(not(unix))]
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("could not start {}: {err}", bb.display());
            std::process::exit(127);
        }
    }
}

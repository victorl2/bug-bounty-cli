//! `bb`: the unified command line.

fn main() {
    // Exit quietly when output is cut short (`bb programs | head`), like other Unix tools.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    std::process::exit(bb_cli::cli::main(std::env::args_os().skip(1).collect()));
}

//! `bug-bounty`: runs `bb`.

#[path = "../shim.rs"]
mod shim;

fn main() {
    shim::exec_bb(&[]);
}

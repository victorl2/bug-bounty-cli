//! `immunefi`: runs `bb immunefi`.

#[path = "../shim.rs"]
mod shim;

fn main() {
    shim::exec_bb(&["immunefi"]);
}

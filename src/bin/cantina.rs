//! `cantina`: runs `bb cantina`.

#[path = "../shim.rs"]
mod shim;

fn main() {
    shim::exec_bb(&["cantina"]);
}

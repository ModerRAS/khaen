//! khaen-motu: MOTU UltraLite-mk5 CueMix 5 control CLI (T40).
//!
//! See cli::HELP / the crate README for commands and exit codes.
#![forbid(unsafe_code)]

fn main() {
    std::process::exit(khaen_motu_cuemix::cli::run_from_env());
}

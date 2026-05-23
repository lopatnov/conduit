mod admin;
mod cli;
mod config;
mod filter;
mod handler;
mod proxy;
mod server;
mod upload;
mod util;

fn main() {
    println!("conduit {}", env!("CARGO_PKG_VERSION"));
}

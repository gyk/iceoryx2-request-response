mod client;
mod server;

mod common;
mod events;
mod messages;

use clap::Parser;

#[derive(Parser)]
#[command(about)]
struct Args {
    /// Run as server
    #[arg(long)]
    server: bool,
}

fn main() {
    let args = Args::parse();
    if args.server {
        server::run().unwrap();
    } else {
        client::run().unwrap();
    }
}

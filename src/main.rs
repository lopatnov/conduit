use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process;

use clap::Parser;
use conduit::cli::args::{Cli, Command, UpstreamsCommand};
use conduit::config::{self, validate};
use conduit::server::builder;

fn main() {
    let cli = Cli::parse();

    match cli.command {
        None => run_server(&cli.config),
        Some(Command::Validate(_)) => cmd_validate(&cli.config),
        Some(Command::Fmt(args)) => cmd_fmt(&cli.config, args.write),
        Some(Command::Init(_)) => unimplemented_cmd("init"),
        Some(Command::Probe(_)) => unimplemented_cmd("probe"),
        Some(Command::Reload(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            admin_post("reload", &addr);
        }
        Some(Command::Status(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            admin_get("status", &addr);
        }
        Some(Command::Shutdown(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            admin_post("shutdown", &addr);
        }
        Some(Command::Upstreams(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            match args.command {
                None => admin_get("upstreams", &addr),
                Some(UpstreamsCommand::Add(_)) => unimplemented_cmd("upstreams add"),
                Some(UpstreamsCommand::Remove(_)) => unimplemented_cmd("upstreams remove"),
                Some(UpstreamsCommand::Weight(_)) => unimplemented_cmd("upstreams weight"),
            }
        }
    }
}

fn run_server(config_path: &str) {
    let path = Path::new(config_path);
    let cfg = match config::load_config(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            process::exit(1);
        }
    };
    let errors = validate::validate(&cfg);
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("config error at {}: {}", e.path, e.message);
        }
        process::exit(1);
    }
    if let Err(e) = builder::run_server(cfg) {
        eprintln!("server error: {e}");
        process::exit(1);
    }
}

fn cmd_validate(config_path: &str) {
    let path = Path::new(config_path);
    let app = match config::load_config(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    let errors = validate::validate(&app);
    if errors.is_empty() {
        println!("Config is valid.");
    } else {
        for e in &errors {
            eprintln!("error at {}: {}", e.path, e.message);
        }
        process::exit(1);
    }
}

fn cmd_fmt(config_path: &str, write: bool) {
    let path = Path::new(config_path);
    let app = match config::load_config(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    let formatted = match serde_json::to_string_pretty(&app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error serializing config: {e}");
            process::exit(1);
        }
    };
    if write {
        if let Err(e) = std::fs::write(path, &formatted) {
            eprintln!("error writing {}: {e}", path.display());
            process::exit(1);
        }
        println!("Formatted {} in place.", path.display());
    } else {
        println!("{formatted}");
    }
}

fn resolve_admin(flag: Option<&str>) -> String {
    flag.map(ToOwned::to_owned)
        .or_else(|| std::env::var("CONDUIT_ADMIN").ok())
        .unwrap_or_else(|| "127.0.0.1:2019".to_owned())
}

fn admin_get(path: &str, addr: &str) {
    match http_get(path, addr) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

fn admin_post(path: &str, addr: &str) {
    match http_post(path, addr) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

fn http_get(path: &str, addr: &str) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect(addr)?;
    write!(stream, "GET /{path} HTTP/1.0\r\nHost: {addr}\r\n\r\n")?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(extract_body(&response))
}

fn http_post(path: &str, addr: &str) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect(addr)?;
    write!(
        stream,
        "POST /{path} HTTP/1.0\r\nHost: {addr}\r\nContent-Length: 0\r\n\r\n"
    )?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(extract_body(&response))
}

fn extract_body(response: &str) -> String {
    response
        .find("\r\n\r\n")
        .map(|pos| response[pos + 4..].to_owned())
        .unwrap_or_else(|| response.to_owned())
}

fn unimplemented_cmd(name: &str) -> ! {
    eprintln!("'{name}' is not yet implemented.");
    process::exit(1);
}

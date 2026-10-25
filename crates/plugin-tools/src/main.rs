use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::io::{Read, Write};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    value: String,
}

fn run() -> Result<(), &'static str> {
    let mut input = String::new();
    std::io::stdin()
        .take(65537)
        .read_to_string(&mut input)
        .map_err(|_| "Cannot read key operation")?;
    if input.len() > 65536 {
        return Err("Key request too large");
    }
    let request: Request = serde_json::from_str(&input).map_err(|_| "Invalid key request")?;
    let mut bytes = STANDARD
        .decode(request.value)
        .map_err(|_| "Invalid key encoding")?;
    let result = match request.operation.as_str() {
        "seal" => wonderland_secret::seal(&bytes),
        "unseal" => wonderland_secret::unseal(&bytes),
        _ => return Err("Unsupported key operation"),
    };
    bytes.fill(0);
    let mut protected =
        result.map_err(|_| "KEY_STORE_UNAVAILABLE: Windows user key protection failed")?;
    let output = serde_json::json!({"value":STANDARD.encode(&protected)});
    protected.fill(0);
    serde_json::to_writer(std::io::stdout().lock(), &output)
        .map_err(|_| "Cannot write key result")?;
    std::io::stdout()
        .flush()
        .map_err(|_| "Cannot write key result")?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

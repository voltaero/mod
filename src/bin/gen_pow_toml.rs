use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

fn print_usage(program: &str) {
    eprintln!(
        "Usage: {program} [--out <file>] [--count <n>] [--zeros <0..64>] [--seed-start <u32>] [--nonce <u64>]\n\
         Defaults:\n\
         --out pow_tasks.rustforge.toml\n\
         --count 1000000\n\
         --zeros 1\n\
         --seed-start 0\n\
         --nonce 0"
    );
}

fn parse_arg<T: std::str::FromStr>(
    args: &[String],
    i: &mut usize,
    name: &str,
) -> Result<T, String> {
    *i += 1;
    let v = args
        .get(*i)
        .ok_or_else(|| format!("missing value for {name}"))?;
    v.parse::<T>()
        .map_err(|_| format!("invalid value for {name}: {v}"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let program = args.first().map(String::as_str).unwrap_or("gen_pow_toml");

    let mut out = PathBuf::from("pow_tasks.rustforge.toml");
    let mut count: u64 = 1_000_000;
    let mut zeros: u8 = 1;
    let mut seed_start: u32 = 0;
    let mut nonce: u64 = 0;

    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_usage(program);
                return Ok(());
            }
            "--out" => out = PathBuf::from(parse_arg::<String>(&args, &mut i, "--out")?),
            "--count" => count = parse_arg::<u64>(&args, &mut i, "--count")?,
            "--zeros" => zeros = parse_arg::<u8>(&args, &mut i, "--zeros")?,
            "--seed-start" => seed_start = parse_arg::<u32>(&args, &mut i, "--seed-start")?,
            "--nonce" => nonce = parse_arg::<u64>(&args, &mut i, "--nonce")?,
            other => return Err(format!("unknown argument: {other}").into()),
        }
        i += 1;
    }

    if zeros > 64 {
        return Err("--zeros must be in 0..=64".into());
    }

    if count > (u32::MAX as u64 + 1) {
        return Err("--count too large for u32 seeds (max 4294967296)".into());
    }

    let file = File::create(&out)?;
    let mut w = BufWriter::new(file);

    for idx in 0..count {
        let seed = seed_start.wrapping_add(idx as u32);
        writeln!(w, "[[\"engine_mod:pow\"]]")?;
        writeln!(w, "seed = {seed}")?;
        writeln!(w, "zeros = {zeros}")?;
        writeln!(w, "nonce = {nonce}")?;
    }

    w.flush()?;

    eprintln!(
        "Wrote {} tasks to {} (zeros={}, seed_start={}, nonce={})",
        count,
        out.display(),
        zeros,
        seed_start,
        nonce
    );

    Ok(())
}

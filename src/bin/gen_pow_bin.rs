use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use engine_core::PowTask;
use enginelib::task::Task;

const MAGIC: &[u8; 4] = b"RFPK";
const VERSION: u8 = 2;
const FRAME_TASK: u8 = 0x01;
const FRAME_END: u8 = 0xFF;

fn print_usage(program: &str) {
    eprintln!(
        "Usage: {program} [--out <file>] [--count <n>] [--zeros <0..64>] [--seed-start <u32>] [--nonce <u64>]\n\
         Defaults:\n\
         --out pow_tasks.rfpk\n\
         --count 1000000\n\
         --zeros 1\n\
         --seed-start 0\n\
         --nonce 0\n\
\n\
         Output format (stream):\n\
         Header: 'RFPK' + 0x02\n\
         Frames: 0x01 task records, terminated by 0xFF"
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

#[inline]
fn write_u16_le<W: Write>(w: &mut W, value: u16) -> std::io::Result<()> {
    w.write_all(&value.to_le_bytes())
}

#[inline]
fn write_u32_le<W: Write>(w: &mut W, value: u32) -> std::io::Result<()> {
    w.write_all(&value.to_le_bytes())
}

#[inline]
fn write_u64_le<W: Write>(w: &mut W, value: u64) -> std::io::Result<()> {
    w.write_all(&value.to_le_bytes())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let program = args.first().map(String::as_str).unwrap_or("gen_pow_bin");

    let mut out = PathBuf::from("pow_tasks.rfpk");
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

    let namespace = b"engine_mod";
    let task = b"pow";
    let id: &[u8] = b"";

    let namespace_len = u16::try_from(namespace.len())?;
    let task_len = u16::try_from(task.len())?;
    let id_len = u32::try_from(id.len())?;

    let file = File::create(&out)?;
    let mut w = BufWriter::new(file);

    // Header: magic + version
    w.write_all(MAGIC)?;
    w.write_all(&[VERSION])?;

    let mut seed = seed_start;

    for idx in 0..count {
        let payload = PowTask { seed, zeros, nonce }.to_bytes();
        let payload_len = u64::try_from(payload.len())?;

        // Task frame
        w.write_all(&[FRAME_TASK])?;
        write_u16_le(&mut w, namespace_len)?;
        write_u16_le(&mut w, task_len)?;
        write_u32_le(&mut w, id_len)?;
        write_u64_le(&mut w, payload_len)?;
        w.write_all(namespace)?;
        w.write_all(task)?;
        w.write_all(id)?;
        w.write_all(&payload)?;

        seed = seed.wrapping_add(1);

        if (idx + 1) % 10_000_000 == 0 {
            eprintln!("wrote {} / {} tasks...", idx + 1, count);
        }
    }

    // End frame
    w.write_all(&[FRAME_END])?;
    w.flush()?;

    eprintln!(
        "Done. Wrote {} tasks to {} in RFPK v2 stream format (zeros={}, seed_start={}, nonce={})",
        count,
        out.display(),
        zeros,
        seed_start,
        nonce
    );

    Ok(())
}

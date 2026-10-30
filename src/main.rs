use anyhow::{bail, Context, Result};
use clap::Parser;
use solana_shred_decoder::{decode::Decoder, wire};
use std::{
    io::{BufReader, BufWriter, Read, Write},
    net::{IpAddr, SocketAddr, UdpSocket},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    version,
    about = "Reassemble raw Solana Merkle shreds and decode entries/transactions"
)]
struct Cli {
    /// Exclusive UDP listener; no SO_REUSEPORT or existing-service changes.
    #[arg(long, default_value = "0.0.0.0:20004")]
    bind: SocketAddr,
    /// Replay length-prefixed datagrams instead of binding UDP.
    #[arg(long, conflicts_with = "capture")]
    input: Option<PathBuf>,
    /// Record raw datagrams with u32 little-endian length prefixes.
    #[arg(long)]
    capture: Option<PathBuf>,
    /// Optional expected sender IP; repeat to allow multiple sources.
    #[arg(long)]
    source_ip: Vec<IpAddr>,
    #[arg(long, default_value_t = 0)]
    duration: u64,
    #[arg(long, default_value_t = 32)]
    receive_buffer_mib: usize,
    #[arg(long, default_value_t = 16)]
    max_slots: usize,
    #[arg(long, default_value_t = 64)]
    max_buffer_mib: usize,
    #[arg(long, default_value_t = 15)]
    slot_ttl: u64,
    /// Include the serialized transaction as base64 in decoded JSON.
    #[arg(long)]
    raw_transactions: bool,
    /// Create a JSONL output file; refuses to overwrite an existing file.
    #[arg(long)]
    output: Option<PathBuf>,
}
fn create(path: &PathBuf) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .context("Cannot create output file")
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.max_slots == 0
        || cli.max_buffer_mib == 0
        || cli.max_buffer_mib > 4096
        || cli.slot_ttl == 0
        || cli.receive_buffer_mib == 0
        || cli.receive_buffer_mib > 1024
    {
        bail!("Invalid buffer, slot or TTL limits");
    }
    let mut decoder = Decoder::new(
        cli.max_slots,
        cli.max_buffer_mib * 1024 * 1024,
        Duration::from_secs(cli.slot_ttl),
        cli.raw_transactions,
    );
    let output: Box<dyn Write> = if let Some(path) = &cli.output {
        Box::new(create(path)?)
    } else {
        Box::new(std::io::stdout())
    };
    let mut output = BufWriter::new(output);
    let mut capture = cli
        .capture
        .as_ref()
        .map(create)
        .transpose()?
        .map(BufWriter::new);
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let start = Instant::now();
    let mut next_report = start;
    let mut ignored = 0u64;
    let mut errors_shown = 0u64;
    let mut consume = |packet: &[u8]| -> Result<()> {
        if let Some(code) = wire::verification(packet) {
            eprintln!("Endpoint verification code: {code}");
            return Ok(());
        }
        if let Some(writer) = capture.as_mut() {
            writer.write_all(&(packet.len() as u32).to_le_bytes())?;
            writer.write_all(packet)?;
        }
        match decoder.push(packet) {
            Ok(entries) => {
                for entry in entries {
                    serde_json::to_writer(&mut output, &entry)?;
                    writeln!(&mut output)?;
                }
            }
            Err(error) => {
                if errors_shown < 5 {
                    eprintln!("Rejected datagram: {error}");
                    errors_shown += 1;
                }
            }
        }
        if Instant::now() >= next_report {
            output.flush()?;
            eprintln!("{}", serde_json::to_string(&decoder.stats)?);
            next_report = Instant::now() + Duration::from_secs(5);
        }
        Ok(())
    };
    if let Some(path) = &cli.input {
        let mut input = BufReader::new(std::fs::File::open(path).context("Cannot open capture")?);
        loop {
            if stop.load(Ordering::Relaxed)
                || (cli.duration > 0 && start.elapsed() >= Duration::from_secs(cli.duration))
            {
                break;
            }
            let mut length = [0; 4];
            match input.read(&mut length[..1]) {
                Ok(0) => break,
                Ok(_) => input.read_exact(&mut length[1..])?,
                Err(e) => return Err(e.into()),
            };
            let length = u32::from_le_bytes(length) as usize;
            if !(1..=65535).contains(&length) {
                bail!("Invalid capture datagram length");
            }
            let mut packet = vec![0; length];
            input
                .read_exact(&mut packet)
                .context("Truncated capture datagram")?;
            consume(&packet)?;
        }
    } else {
        let socket = socket2::Socket::new(
            socket2::Domain::for_address(cli.bind),
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        socket.set_recv_buffer_size(cli.receive_buffer_mib * 1024 * 1024)?;
        socket
            .bind(&cli.bind.into())
            .context("UDP port unavailable; use a free port")?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        eprintln!(
            "Listening on {} (effective SO_RCVBUF={} bytes)",
            cli.bind,
            socket.recv_buffer_size()?
        );
        let socket: UdpSocket = socket.into();
        let mut packet = [0u8; 65535];
        while !stop.load(Ordering::Relaxed)
            && (cli.duration == 0 || start.elapsed() < Duration::from_secs(cli.duration))
        {
            match socket.recv_from(&mut packet) {
                Ok((len, peer)) => {
                    if cli.source_ip.is_empty() || cli.source_ip.contains(&peer.ip()) {
                        consume(&packet[..len])?;
                    } else {
                        ignored += 1;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    output.flush()?;
    if let Some(mut writer) = capture {
        writer.flush()?;
    }
    eprintln!(
        "Final: {} ignored_sender_packets={ignored}",
        serde_json::to_string(&decoder.stats)?
    );
    if decoder.stats.batches == 0 {
        bail!("No complete entry batch decoded; check input, missing shreds and diagnostics");
    }
    Ok(())
}

//! Small synthetic decoding demo. Signatures are placeholders, not valid proofs.
use anyhow::{Context, Result};
use solana_message::{Message, MessageHeader, VersionedMessage};
use solana_shred_decoder::{
    decode::Entry,
    wire::{DATA_BYTES, DATA_HEADER},
};
use solana_transaction::versioned::VersionedTransaction;
use std::io::Write;
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("Usage: cargo run --example make_capture -- demo.dat")?;
    let tx = VersionedTransaction {
        signatures: vec![Default::default()],
        message: VersionedMessage::Legacy(Message {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 0,
            },
            account_keys: vec![Default::default()],
            ..Default::default()
        }),
    };
    let bytes = wincode::serialize(&vec![
        Entry {
            num_hashes: 3,
            hash: [1; 32],
            transactions: vec![tx],
        },
        Entry {
            num_hashes: 1,
            hash: [2; 32],
            transactions: vec![],
        },
    ])?;
    let mut packet = vec![0u8; DATA_BYTES];
    packet[64] = 0x90;
    packet[65..73].copy_from_slice(&42u64.to_le_bytes());
    packet[77..79].copy_from_slice(&1u16.to_le_bytes());
    packet[83..85].copy_from_slice(&1u16.to_le_bytes());
    packet[85] = 0xc0;
    packet[86..88].copy_from_slice(&((DATA_HEADER + bytes.len()) as u16).to_le_bytes());
    packet[DATA_HEADER..DATA_HEADER + bytes.len()].copy_from_slice(&bytes);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    file.write_all(&(packet.len() as u32).to_le_bytes())?;
    file.write_all(&packet)?;
    eprintln!("Created synthetic capture (one transaction, one tick; placeholder signatures)");
    Ok(())
}

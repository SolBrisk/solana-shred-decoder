use reed_solomon_erasure::galois_8::ReedSolomon;
use solana_message::{Message, MessageHeader, VersionedMessage};
use solana_shred_decoder::{
    decode::{entries, Decoder, Entry},
    wire::{self, Shred, CODE_BYTES, CODE_HEADER, DATA_BYTES, DATA_HEADER},
};
use solana_transaction::versioned::VersionedTransaction;
use std::time::Duration;

fn transaction(version: u8) -> VersionedTransaction {
    let header = MessageHeader {
        num_required_signatures: 1,
        num_readonly_signed_accounts: 0,
        num_readonly_unsigned_accounts: 0,
    };
    let account_keys = vec![Default::default()];
    let message = match version {
        0 => VersionedMessage::Legacy(Message {
            header,
            account_keys,
            ..Default::default()
        }),
        1 => VersionedMessage::V0(solana_message::v0::Message {
            header,
            account_keys,
            ..Default::default()
        }),
        _ => VersionedMessage::V1(solana_message::v1::Message {
            header,
            account_keys,
            ..Default::default()
        }),
    };
    VersionedTransaction {
        signatures: vec![Default::default()],
        message,
    }
}

// Synthetic wire fixture builder independent of the production parser/recovery
// path. 2 data + 2 coding shards, chained Merkle, depth 2.
fn fixture(fec_index: u32, resigned: bool, versions: &[u8]) -> Vec<Vec<u8>> {
    let batch = vec![Entry {
        num_hashes: 7,
        hash: [9; 32],
        transactions: versions.iter().map(|v| transaction(*v)).collect(),
    }];
    let bytes = wincode::serialize(&batch).unwrap();
    let proof = 2;
    let extra = usize::from(resigned) * 64;
    let data_chain = DATA_BYTES - proof * 20 - extra - 32;
    let code_chain = CODE_BYTES - proof * 20 - extra - 32;
    let capacity = data_chain - DATA_HEADER;
    assert!(bytes.len() <= capacity * 2);
    let split = bytes.len() / 2;
    let chunks = [&bytes[..split], &bytes[split..]];
    let mut packets = Vec::new();
    let mut shards = Vec::new();
    for (pos, chunk) in chunks.iter().enumerate() {
        let mut p = vec![0; DATA_BYTES];
        p[..64].fill(7);
        p[64] = (if resigned { 0xb0 } else { 0x90 }) | proof as u8;
        p[65..73].copy_from_slice(&42u64.to_le_bytes());
        p[73..77].copy_from_slice(&(fec_index + pos as u32).to_le_bytes());
        p[77..79].copy_from_slice(&1u16.to_le_bytes());
        p[79..83].copy_from_slice(&fec_index.to_le_bytes());
        p[83..85].copy_from_slice(&1u16.to_le_bytes());
        p[85] = if pos == 1 { 0x40 } else { 0 };
        p[86..88].copy_from_slice(&((DATA_HEADER + chunk.len()) as u16).to_le_bytes());
        p[88..88 + chunk.len()].copy_from_slice(chunk);
        p[data_chain..data_chain + 32].fill(3);
        shards.push(p[64..data_chain].to_vec());
        packets.push(p);
    }
    shards.push(vec![0; code_chain - CODE_HEADER]);
    shards.push(vec![0; code_chain - CODE_HEADER]);
    ReedSolomon::new(2, 2).unwrap().encode(&mut shards).unwrap();
    for pos in 0..2 {
        let mut p = vec![0; CODE_BYTES];
        p[..64].fill(7);
        p[64] = (if resigned { 0x70 } else { 0x60 }) | proof as u8;
        p[65..73].copy_from_slice(&42u64.to_le_bytes());
        p[73..77].copy_from_slice(&(100 + pos as u32).to_le_bytes());
        p[77..79].copy_from_slice(&1u16.to_le_bytes());
        p[79..83].copy_from_slice(&fec_index.to_le_bytes());
        p[83..85].copy_from_slice(&2u16.to_le_bytes());
        p[85..87].copy_from_slice(&2u16.to_le_bytes());
        p[87..89].copy_from_slice(&(pos as u16).to_le_bytes());
        p[89..code_chain].copy_from_slice(&shards[2 + pos]);
        p[code_chain..code_chain + 32].fill(3);
        packets.push(p);
    }
    let leaves = packets
        .iter()
        .enumerate()
        .map(|(i, p)| {
            wire::leaf(
                &p[64..if i < 2 {
                    data_chain + 32
                } else {
                    code_chain + 32
                }],
            )
        })
        .collect::<Vec<_>>();
    let branches = [
        wire::join(&leaves[0], &leaves[1]),
        wire::join(&leaves[2], &leaves[3]),
    ];
    for (i, p) in packets.iter_mut().enumerate() {
        let offset = if i < 2 {
            data_chain + 32
        } else {
            code_chain + 32
        };
        p[offset..offset + 20].copy_from_slice(&leaves[i ^ 1][..20]);
        p[offset + 20..offset + 40].copy_from_slice(&branches[(i / 2) ^ 1][..20]);
    }
    packets
}
fn decoder() -> Decoder {
    Decoder::new(16, 64 * 1024 * 1024, Duration::from_secs(15), true)
}

#[test]
fn decodes_legacy_v0_v1_and_emits_once() {
    let packets = fixture(0, false, &[0, 1, 2]);
    let mut d = decoder();
    assert!(d.push(&packets[1]).unwrap().is_empty());
    let result = d.push(&packets[0]).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["transactions"].as_array().unwrap().len(), 3);
    assert!(d.push(&packets[0]).unwrap().is_empty());
    assert_eq!(d.stats.transactions, 3);
}
#[test]
fn recovers_a_missing_data_shred_from_coding() {
    for resigned in [false, true] {
        let packets = fixture(0, resigned, &[0]);
        let mut d = decoder();
        assert!(d.push(&packets[0]).unwrap().is_empty());
        let result = d.push(&packets[2]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(d.stats.recovered_data_shreds, 1);
        assert_eq!(d.stats.fec_failures, 0);
        assert_eq!(result[0]["transactions"].as_array().unwrap().len(), 1);
    }
}
#[test]
fn recovers_from_coding_only() {
    let packets = fixture(0, false, &[1]);
    let mut d = decoder();
    assert!(d.push(&packets[2]).unwrap().is_empty());
    let result = d.push(&packets[3]).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(d.stats.recovered_data_shreds, 2);
}
#[test]
fn incompatible_merkle_tails_disable_recovery_but_keep_direct_data() {
    let packets = fixture(0, false, &[]);
    let mut d = decoder();
    d.push(&packets[0]).unwrap();
    let mut altered = packets[1].clone();
    altered[1100] ^= 1;
    assert_eq!(d.push(&altered).unwrap().len(), 1);
    assert_eq!(d.stats.fec_identity_conflicts, 1);
    assert_eq!(d.stats.conflicts, 0);
}
#[test]
fn conflicting_application_data_quarantines_slot() {
    let packets = fixture(0, false, &[]);
    let mut d = decoder();
    d.push(&packets[0]).unwrap();
    let mut corrupt = packets[0].clone();
    corrupt[90] ^= 1;
    assert!(d.push(&corrupt).is_err());
    assert_eq!(d.stats.conflicts, 1);
    assert!(d.push(&packets[1]).unwrap().is_empty());
}
#[test]
fn late_join_skips_incomplete_first_batch() {
    let first = fixture(0, false, &[0]);
    let second = fixture(2, false, &[1]);
    let mut d = decoder();
    assert!(d.push(&first[1]).unwrap().is_empty());
    assert!(d.push(&second[0]).unwrap().is_empty());
    assert_eq!(d.push(&second[1]).unwrap().len(), 1);
}
#[test]
fn repair_nonce_does_not_change_identity() {
    let packets = fixture(0, false, &[]);
    let mut repair = packets[0].clone();
    repair.extend(123u32.to_le_bytes());
    assert_eq!(
        Shred::parse(&packets[0]).unwrap().identity(),
        Shred::parse(&repair).unwrap().identity()
    );
}
#[test]
fn entry_decoder_rejects_invalid_transaction() {
    let tx = VersionedTransaction {
        signatures: vec![],
        message: transaction(0).message,
    };
    let bytes = wincode::serialize(&vec![Entry {
        num_hashes: 1,
        hash: [0; 32],
        transactions: vec![tx],
    }])
    .unwrap();
    assert!(entries(&bytes).is_err());
}

#[test]
fn zero_padded_entry_batch_decodes_but_nonzero_tail_is_rejected() {
    let batch = vec![Entry {
        num_hashes: 1,
        hash: [2; 32],
        transactions: vec![transaction(0)],
    }];
    let mut bytes = wincode::serialize(&batch).unwrap();
    bytes.resize(32_768, 0);
    assert_eq!(entries(&bytes).unwrap().len(), 1);
    *bytes.last_mut().unwrap() = 1;
    assert!(entries(&bytes).is_err());
}

#[test]
fn replay_cli_decodes_capture_and_refuses_existing_output() {
    use std::io::Write;
    let directory =
        std::env::temp_dir().join(format!("solbrisk-decoder-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let input = directory.join("fixture.dat");
    let output_path = directory.join("entries.jsonl");
    let mut file = std::fs::File::create(&input).unwrap();
    for packet in fixture(0, false, &[0, 1, 2]) {
        file.write_all(&(packet.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&packet).unwrap();
    }
    drop(file);
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_solana-shred-decoder"))
            .arg("--input")
            .arg(&input)
            .arg("--output")
            .arg(&output_path)
            .arg("--raw-transactions")
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(&output_path).unwrap();
    let rows = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["transactions"].as_array().unwrap().len(), 3);
    assert!(rows[0]["transactions"][0]["transaction_base64"].is_string());
    assert!(!run().status.success());
    assert_eq!(std::fs::read_to_string(&output_path).unwrap(), text);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn udp_cli_receives_live_packets_and_verification_controls() {
    use std::{
        net::UdpSocket,
        process::{Command, Stdio},
        thread,
    };
    let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let child = Command::new(env!("CARGO_BIN_EXE_solana-shred-decoder"))
        .args(["--bind", &address.to_string(), "--duration", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    thread::sleep(Duration::from_millis(200));
    sender
        .send_to(
            b"SHREDHARBOR_VERIFY demo 0123456789abcdef0123456789abcdef0123456789abcdef",
            address,
        )
        .unwrap();
    for _ in 0..10 {
        for packet in fixture(0, false, &[0]) {
            sender.send_to(&packet, address).unwrap();
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let rows = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["transactions"].as_array().unwrap().len(), 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("Endpoint verification code:"));
}

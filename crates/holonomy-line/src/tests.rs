#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use holonomy_seal::{
    create_capsule_for, generate_container_id, Capsule, DEMKey, KdfParamsRecord, MasterKeys,
    ParamsClass,
};

use super::*;

struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let id = generate_container_id().unwrap();
    let suffix = id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = std::env::temp_dir().join(format!("holonomy-{label}-{suffix}"));
    fs::create_dir(&path).unwrap();
    TempDir(path)
}

fn temp_path(label: &str) -> TempPath {
    let id = generate_container_id().unwrap();
    let suffix = id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    TempPath(std::env::temp_dir().join(format!("holonomy-{label}-{suffix}.wavefunction")))
}

fn test_params() -> KdfParamsRecord {
    KdfParamsRecord {
        m_kib: 8_192,
        t: 1,
        p: 1,
    }
}

fn test_header(salt: [u8; 64], capsule: Capsule) -> LineHeader {
    LineHeader::new(test_params(), salt, capsule).unwrap()
}

fn test_material(passphrase: &[u8], salt: [u8; 64]) -> (MasterKeys, Capsule, DEMKey) {
    let keys = MasterKeys::derive(passphrase, &salt, None, ParamsClass::Test).unwrap();
    let (capsule, dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    (keys, capsule, dem)
}

fn files_equal_prefix(left: &Path, right: &Path, length: u64) -> io::Result<bool> {
    let mut left = fs::File::open(left)?;
    let mut right = fs::File::open(right)?;
    let mut remaining = length;
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    while remaining != 0 {
        let count = remaining.min(left_buffer.len() as u64) as usize;
        left.read_exact(&mut left_buffer[..count])?;
        right.read_exact(&mut right_buffer[..count])?;
        if left_buffer[..count] != right_buffer[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    Ok(true)
}

fn assert_tail_write_trace(
    trace: &AppendIoSnapshot,
    old_len: u64,
    new_len: u64,
    receipt: AppendReceipt,
) {
    assert_eq!(trace.bytes_written, receipt.bytes_appended);
    assert_eq!(trace.syncs.len(), 2);
    assert!(trace.syncs.iter().all(|sync| sync.succeeded));
    assert!(trace.syncs[0].bytes_written < trace.syncs[1].bytes_written);
    assert_eq!(trace.syncs[1].bytes_written, receipt.bytes_appended);
    assert!(trace.truncations.is_empty());

    let mut expected_offset = old_len;
    for write in &trace.writes {
        assert_eq!(write.offset, expected_offset, "append write offset");
        expected_offset += write.bytes as u64;
    }
    assert_eq!(expected_offset, new_len);
}

#[derive(Debug, Clone, Copy)]
struct RsyncStats {
    literal_data: u64,
    bytes_sent: u64,
    bytes_received: u64,
}

fn parse_rsync_stat(output: &str, label: &str) -> u64 {
    let prefix = format!("{label}:");
    let line = output
        .lines()
        .find(|line| line.trim_start().starts_with(&prefix))
        .unwrap_or_else(|| panic!("rsync --stats output omitted {label}:\n{output}"));
    line.trim_start()
        .split_once(':')
        .expect("stat line has a colon")
        .1
        .split_whitespace()
        .next()
        .expect("stat line has a number")
        .replace(',', "")
        .parse()
        .unwrap_or_else(|_| panic!("invalid rsync {label} value: {line}"))
}

fn rsync_transfer(source: &Path, destination: &str, port: u16) -> (RsyncStats, String) {
    let output = Command::new("rsync")
        .args([
            "--archive",
            "--stats",
            "--no-whole-file",
            "--block-size=65536",
            "--timeout=60",
        ])
        .arg(format!("--port={port}"))
        .env("LC_ALL", "C")
        .arg(source)
        .arg(destination)
        .output()
        .expect("rsync must be installed for the L-10 qualification");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "rsync failed: {text}");
    (
        RsyncStats {
            literal_data: parse_rsync_stat(&text, "Literal data"),
            bytes_sent: parse_rsync_stat(&text, "Total bytes sent"),
            bytes_received: parse_rsync_stat(&text, "Total bytes received"),
        },
        text,
    )
}

struct RsyncDaemon(Child);

impl Drop for RsyncDaemon {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn wait_for_rsync(port: u16, daemon: &mut Child) {
    for _ in 0..100 {
        if let Some(status) = daemon.try_wait().expect("check rsync daemon") {
            panic!("rsync daemon exited before accepting connections: {status}");
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("rsync daemon did not listen on 127.0.0.1:{port}");
}

#[test]
fn create_does_not_replace_an_existing_path() {
    let path = temp_path("create-no-replace");
    let salt = [0x2F; 64];
    let (_keys, capsule, dem) = test_material(b"create no replace", salt);
    let header = test_header(salt, capsule);
    let stage = parent_directory(&path.0).join(stage_name(header.file_id()));
    let existing = b"preserve this destination";
    fs::write(&path.0, existing).unwrap();

    assert!(matches!(
        WorldLine::create(&path.0, header, dem.duplicate()),
        Err(LineError::Io(_))
    ));
    assert_eq!(fs::read(&path.0).unwrap().as_slice(), existing);
    assert!(!stage.exists());
}

#[test]
fn create_refuses_its_reserved_staging_path() {
    let salt = [0x30; 64];
    let (_keys, capsule, dem) = test_material(b"reserved staging path", salt);
    let header = test_header(salt, capsule);
    let path = TempPath(std::env::temp_dir().join(stage_name(header.file_id())));
    let existing = b"preserve reserved-name file";
    fs::write(&path.0, existing).unwrap();

    assert!(matches!(
        WorldLine::create(&path.0, header, dem.duplicate()),
        Err(LineError::InvalidCreationDestination)
    ));
    assert_eq!(fs::read(&path.0).unwrap().as_slice(), existing);
}

#[test]
fn a_committed_document_order_survives_a_reopen() {
    let path = temp_path("document-order");
    let salt = [0x5A; 64];
    let (_keys, capsule, dem) = test_material(b"document order", salt);
    let header = test_header(salt, capsule);
    let mut line = WorldLine::create(&path.0, header, dem.duplicate()).unwrap();
    line.append_segments(&[b"world", b"hello "]).unwrap();

    // The document reads `hello world` although the second leaf was appended last (OQ-29).
    let expected = vec![
        LineSlab {
            segment: 1,
            start: 0,
            len: 6,
        },
        LineSlab {
            segment: 0,
            start: 0,
            len: 5,
        },
    ];
    line.commit_manifest(&expected).unwrap();
    assert_eq!(line.document_slabs(), expected);
    assert_eq!(line.recovery_info().trailing_bytes, 0);
    assert_eq!(line.witness_count(), 3);
    drop(line);

    let reopened = WorldLine::open(&path.0, dem).unwrap();
    assert_eq!(reopened.document_slabs(), expected);
    assert_eq!(reopened.recovery_info().trailing_bytes, 0);
    assert_eq!(&reopened.open_segment(1).unwrap()[..], b"hello ");
}

#[test]
fn append_reopen_and_verify_round_trip() {
    let path = temp_path("round-trip");
    let salt = [0x31; 64];
    let (_keys, capsule, dem) = test_material(b"round trip", salt);
    let header = test_header(salt, capsule);
    let mut line = WorldLine::create(&path.0, header, dem.duplicate()).unwrap();

    let pieces: [&[u8]; 2] = [&b"first sealed slab"[..], &b"second sealed slab"[..]];
    let receipt = line.append_segments(&pieces).unwrap();
    assert_eq!(receipt.segment_count, 2);
    assert_eq!(receipt.witness_sequence, 1);
    assert_eq!(line.segment_count(), 2);
    assert_eq!(&line.open_segment(0).unwrap()[..], b"first sealed slab");
    assert_eq!(&line.open_segment(1).unwrap()[..], b"second sealed slab");

    let report = line.verify().unwrap();
    assert_eq!(report.segments_verified, 2);
    assert_eq!(report.plaintext_bytes_verified, 35);
    assert_eq!(report.witnesses_verified, 2);
    drop(line);

    let reopened = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(reopened.segment_count(), 2);
    assert_eq!(reopened.segment_root(), receipt.segment_root);
    assert!(!reopened.recovery_info().needs_recovery());
}

#[test]
fn append_writes_only_new_tail_records() {
    let path = temp_path("tail-only");
    let salt = [0x32; 64];
    let (_keys, capsule, dem) = test_material(b"tail only", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"old segment"[..]]).unwrap();
    let before = fs::read(&path.0).unwrap();
    let old_len = before.len() as u64;

    let probe = AppendIoProbeGuard::install(None, None, false);
    let receipt = line.append_segments(&[&b"new tail segment"[..]]).unwrap();
    let trace = probe.snapshot();
    drop(probe);
    let after = fs::read(&path.0).unwrap();
    let new_len = after.len() as u64;
    assert_eq!(new_len - old_len, receipt.bytes_appended);
    assert_eq!(&after[..old_len as usize], &before);
    assert_eq!(receipt.segment_count, 2);
    assert_tail_write_trace(&trace, old_len, new_len, receipt);
}

#[test]
fn stale_writers_and_same_length_metadata_tampering_cannot_append() {
    let path = temp_path("stale-writer");
    let salt = [0x40; 64];
    let (_keys, capsule, dem) = test_material(b"stale writer", salt);
    let mut original =
        WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    original.append_segments(&[&b"first commit"[..]]).unwrap();
    let mut stale = WorldLine::open(&path.0, dem.duplicate()).unwrap();

    original.append_segments(&[&b"second commit"[..]]).unwrap();
    assert!(matches!(
        stale.append_segments(&[&b"stale append"[..]]),
        Err(LineError::ConcurrentModification)
    ));
    drop(stale);
    drop(original);

    let mut tampered = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    let tag_offset = tampered.segments[0].ciphertext_offset - 32;
    let before_len = fs::metadata(&path.0).unwrap().len();
    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path.0)
            .unwrap();
        file.seek(SeekFrom::Start(tag_offset)).unwrap();
        let mut tag_byte = [0u8; 1];
        file.read_exact(&mut tag_byte).unwrap();
        tag_byte[0] ^= 1;
        file.seek(SeekFrom::Start(tag_offset)).unwrap();
        file.write_all(&tag_byte).unwrap();
        file.sync_all().unwrap();
    }
    assert_eq!(fs::metadata(&path.0).unwrap().len(), before_len);
    assert!(matches!(
        tampered.append_segments(&[&b"must not append"[..]]),
        Err(LineError::ConcurrentModification)
    ));
    assert_eq!(fs::metadata(&path.0).unwrap().len(), before_len);
}

#[test]
fn shared_open_lock_serializes_exclusive_append_lock() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let path = temp_path("lock-contention");
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path.0)
        .unwrap();
    let shared_lock = FileReadLock::acquire(&file).unwrap();
    let worker_path = path.0.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let file = open_worldline_file(&worker_path, true, true).unwrap();
        let _exclusive_lock = FileAppendLock::acquire(&file).unwrap();
        acquired_tx.send(()).unwrap();
    });

    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        acquired_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(shared_lock);
    acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    worker.join().unwrap();
}

#[test]
fn a_simulated_thirty_million_word_history_still_appends_only_the_new_tail() {
    const FULL_SEGMENT_WORDS: usize = 32_768;
    const FULL_SEGMENTS: usize = 915;
    const FINAL_SEGMENT_WORDS: usize = 17_280;
    const WORDS: usize = FULL_SEGMENTS * FULL_SEGMENT_WORDS + FINAL_SEGMENT_WORDS;
    assert_eq!(WORDS, 30_000_000);

    let path = temp_path("thirty-million-words");
    let salt = [0x3D; 64];
    let (_keys, capsule, dem) = test_material(b"large history", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();

    // A one-letter word plus a space is two bytes; reusing one buffer keeps the test's
    // plaintext working set bounded while the encrypted file represents 30 million words.
    let full_segment = b"a ".repeat(FULL_SEGMENT_WORDS);
    let final_segment = b"a ".repeat(FINAL_SEGMENT_WORDS);
    let mut history = vec![full_segment.as_slice(); FULL_SEGMENTS];
    history.push(final_segment.as_slice());
    line.append_segments(&history).unwrap();

    let before_len = fs::metadata(&path.0).unwrap().len();
    let snapshot = temp_path("tail-snapshot");
    fs::copy(&path.0, &snapshot.0).unwrap();
    let probe = AppendIoProbeGuard::install(None, None, false);
    let receipt = line.append_segments(&[&b"new tail edit"[..]]).unwrap();
    let trace = probe.snapshot();
    drop(probe);
    let after_len = fs::metadata(&path.0).unwrap().len();
    assert_eq!(after_len - before_len, receipt.bytes_appended);
    assert!(files_equal_prefix(&path.0, &snapshot.0, before_len).unwrap());
    assert_tail_write_trace(&trace, before_len, after_len, receipt);

    drop(line);
    let reopened = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(reopened.segment_count(), (FULL_SEGMENTS + 1 + 1) as u64);
    assert_eq!(
        &reopened.open_segment((FULL_SEGMENTS + 1) as u64).unwrap()[..],
        b"new tail edit"
    );
}

#[test]
#[ignore = "requires rsync and a local daemon; run the named-client qualification explicitly"]
fn rsync_measures_append_transfer_over_a_named_local_daemon() {
    let version = Command::new("rsync")
        .arg("--version")
        .output()
        .expect("rsync must be installed for the L-10 qualification");
    assert!(version.status.success());
    let version = String::from_utf8_lossy(&version.stdout)
        .lines()
        .next()
        .unwrap_or("rsync version unknown")
        .to_owned();

    let directory = temp_dir("rsync-qualification");
    let remote_root = directory.0.join("remote");
    fs::create_dir(&remote_root).unwrap();
    let source = directory.0.join("history.wavefunction");
    let config = directory.0.join("rsyncd.conf");
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    fs::write(
        &config,
        format!(
            "address = 127.0.0.1\nport = {port}\npid file = {}\nlock file = {}\nuse chroot = no\n[holonomy]\npath = {}\nread only = no\nlist = no\n",
            directory.0.join("rsyncd.pid").display(),
            directory.0.join("rsyncd.lock").display(),
            remote_root.display()
        ),
    )
    .unwrap();
    let mut daemon = RsyncDaemon(
        Command::new("rsync")
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--config")
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start local rsync daemon"),
    );
    wait_for_rsync(port, &mut daemon.0);

    const TOTAL_WORDS: usize = 30_000_000;
    const FULL_SEGMENT_WORDS: usize = 32_768;
    const FULL_SEGMENTS: usize = 915;
    const FINAL_SEGMENT_WORDS: usize = TOTAL_WORDS - FULL_SEGMENTS * FULL_SEGMENT_WORDS;

    let salt = [0x5A; 64];
    let (_keys, capsule, dem) = test_material(b"rsync 30M qualification", salt);
    let mut line = WorldLine::create(&source, test_header(salt, capsule), dem.duplicate()).unwrap();
    let full_segment = b"a ".repeat(FULL_SEGMENT_WORDS);
    let final_segment = b"a ".repeat(FINAL_SEGMENT_WORDS);
    let mut history = vec![full_segment.as_slice(); FULL_SEGMENTS];
    history.push(final_segment.as_slice());
    line.append_segments(&history).unwrap();

    let destination = format!("rsync://127.0.0.1:{port}/holonomy/history.wavefunction");
    let (initial_stats, _) = rsync_transfer(&source, &destination, port);
    let old_len = fs::metadata(&source).unwrap().len();
    let remote_file = remote_root.join("history.wavefunction");
    assert_eq!(fs::metadata(&remote_file).unwrap().len(), old_len);
    assert!(files_equal_prefix(&source, &remote_file, old_len).unwrap());

    let appended_segment = vec![b'z'; MAX_SEGMENT_BYTES as usize];
    let segments: [&[u8]; 4] = [
        &appended_segment,
        &appended_segment,
        &appended_segment,
        &appended_segment,
    ];
    let receipt = line.append_segments(&segments).unwrap();
    let new_len = fs::metadata(&source).unwrap().len();
    assert_eq!(new_len - old_len, receipt.bytes_appended);
    let (delta_stats, _) = rsync_transfer(&source, &destination, port);
    assert_eq!(fs::metadata(&remote_file).unwrap().len(), new_len);
    assert!(files_equal_prefix(&source, &remote_file, new_len).unwrap());

    // A partial final checksum block can require retransmitting up to one block of old tail;
    // report the raw client/server totals separately so protocol overhead is not hidden behind
    // the payload-only `Literal data` statistic.
    let delta_network_bytes = delta_stats.bytes_sent + delta_stats.bytes_received;
    let initial_network_bytes = initial_stats.bytes_sent + initial_stats.bytes_received;
    assert!(
        delta_stats.literal_data <= receipt.bytes_appended + MAX_SEGMENT_BYTES as u64,
        "rsync sent unexpectedly much literal file data: {delta_stats:?}, append={}",
        receipt.bytes_appended
    );
    assert!(
        delta_network_bytes <= receipt.bytes_appended.saturating_mul(8),
        "rsync application-layer traffic exceeded 8x the appended record bytes: network={delta_network_bytes}, append={}",
        receipt.bytes_appended
    );
    eprintln!(
        "HOLONOMY_RSYNC_MEASUREMENT client={version}; transport=local-rsync-daemon-over-TCP; block_size=65536; old_file_bytes={old_len}; new_file_bytes={new_len}; appended_record_bytes={}; initial_literal_data={}; initial_client_bytes_sent={}; initial_client_bytes_received={}; initial_application_bytes={initial_network_bytes}; delta_literal_data={}; delta_client_bytes_sent={}; delta_client_bytes_received={}; delta_application_bytes={delta_network_bytes}",
        receipt.bytes_appended,
        initial_stats.literal_data,
        initial_stats.bytes_sent,
        initial_stats.bytes_received,
        delta_stats.literal_data,
        delta_stats.bytes_sent,
        delta_stats.bytes_received,
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires ext4 loop ENOSPC; see scripts/qualify-ext4-enospc.sh"]
fn real_enospc_preserves_an_authenticated_witness_on_ext4() {
    let storage_dir = PathBuf::from(
        std::env::var_os("HOLONOMY_ENOSPC_DIR")
            .expect("set HOLONOMY_ENOSPC_DIR to the mounted ext4 qualification filesystem"),
    );
    let fs_type = Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "--target"])
        .arg(&storage_dir)
        .output()
        .expect("findmnt is required to verify the qualification filesystem");
    assert!(fs_type.status.success(), "findmnt failed: {fs_type:?}");
    let fs_type = String::from_utf8_lossy(&fs_type.stdout).trim().to_owned();
    assert_eq!(fs_type, "ext4", "L-6 test must run on ext4, not {fs_type}");

    let mount_source = Command::new("findmnt")
        .args(["-n", "-o", "SOURCE", "--target"])
        .arg(&storage_dir)
        .output()
        .expect("findmnt is required to verify the bounded loop device");
    assert!(
        mount_source.status.success(),
        "findmnt source lookup failed"
    );
    let mount_source = String::from_utf8_lossy(&mount_source.stdout)
        .trim()
        .to_owned();
    assert!(
        mount_source.starts_with("/dev/loop"),
        "L-6 rehearsal refuses non-loop devices, got {mount_source}"
    );

    let filesystem_space = Command::new("df")
        .args(["-B1", "--output=size,avail"])
        .arg(&storage_dir)
        .output()
        .expect("df is required to verify the bounded filesystem");
    assert!(
        filesystem_space.status.success(),
        "df failed: {filesystem_space:?}"
    );
    let filesystem_space = String::from_utf8_lossy(&filesystem_space.stdout);
    let filesystem_space = filesystem_space
        .lines()
        .last()
        .expect("df reports a filesystem row");
    let mut filesystem_space = filesystem_space.split_whitespace();
    let filesystem_bytes: u64 = filesystem_space
        .next()
        .expect("df reports filesystem size")
        .parse()
        .expect("df filesystem size is numeric");
    let initial_free_bytes: u64 = filesystem_space
        .next()
        .expect("df reports free bytes")
        .parse()
        .expect("df free bytes are numeric");
    assert!(
        filesystem_bytes <= 256 * 1024 * 1024,
        "L-6 rehearsal refuses filesystems larger than 256 MiB, got {filesystem_bytes} bytes"
    );

    let block_size_output = Command::new("stat")
        .args(["-f", "-c", "%S", "--"])
        .arg(&storage_dir)
        .output()
        .expect("stat is required to query the ext4 block size");
    assert!(
        block_size_output.status.success(),
        "stat filesystem query failed: {block_size_output:?}"
    );
    let filesystem_block_bytes: u64 = String::from_utf8_lossy(&block_size_output.stdout)
        .trim()
        .parse()
        .expect("stat reports a numeric filesystem block size");
    assert!(
        filesystem_block_bytes.is_power_of_two()
            && filesystem_block_bytes <= u64::from(MAX_SEGMENT_BYTES),
        "L-6 requires a power-of-two filesystem block no larger than a segment, got {filesystem_block_bytes} bytes"
    );

    let id = generate_container_id().unwrap();
    let suffix = id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = TempPath(storage_dir.join(format!("holonomy-enospc-{suffix}.wavefunction")));
    let filler_path = TempPath(storage_dir.join(format!("holonomy-enospc-fill-{suffix}.bin")));
    let salt = [0x6B; 64];
    let (_keys, capsule, dem) = test_material(b"real ENOSPC qualification", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();

    // End the committed prefix on a block boundary. Otherwise a tiny append can fit in the
    // already allocated final block even after the filler has received ENOSPC.
    let next_segment_count = line
        .segment_count()
        .checked_add(1)
        .expect("genesis leaves room for one segment");
    let genesis_end = fs::metadata(&path.0).unwrap().len();
    let record_frame_overhead =
        (RECORD_HEADER_BYTES + RECORD_FIXED_BYTES + AEAD_TAG_BYTES as usize) as u64;
    let next_append_overhead = 2 * record_frame_overhead
        + WITNESS_BASE_BYTES as u64
        + u64::from(next_segment_count.count_ones()) * 32;
    let mut initial_payload_bytes = (filesystem_block_bytes
        - (genesis_end + next_append_overhead) % filesystem_block_bytes)
        % filesystem_block_bytes;
    if initial_payload_bytes == 0 {
        initial_payload_bytes = filesystem_block_bytes;
    }
    assert!(
        initial_payload_bytes <= u64::from(MAX_SEGMENT_BYTES),
        "block-alignment segment exceeds the format limit"
    );
    let initial_payload = vec![0x6B; initial_payload_bytes as usize];
    line.append_segments(&[&initial_payload[..]]).unwrap();
    let baseline_committed_end = fs::metadata(&path.0).unwrap().len();
    assert_eq!(
        baseline_committed_end % filesystem_block_bytes,
        0,
        "prior authenticated witness must end on a fresh allocation block"
    );

    let mut filler = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&filler_path.0)
        .unwrap();
    let chunk = vec![0x5A; 1024 * 1024];
    let filler_enospc_phase = loop {
        match filler.write_all(&chunk) {
            Ok(()) => match filler.sync_all() {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(libc::ENOSPC) => break "sync",
                Err(error) => panic!("filler sync failed for a non-ENOSPC reason: {error}"),
            },
            Err(error) if error.raw_os_error() == Some(libc::ENOSPC) => break "write",
            Err(error) => panic!("filler write failed for a non-ENOSPC reason: {error}"),
        }
    };
    let query_available_bytes = || {
        let output = Command::new("df")
            .args(["-B1", "--output=avail"])
            .arg(&storage_dir)
            .output()
            .expect("df is required to measure remaining filesystem space");
        assert!(
            output.status.success(),
            "df available-space query failed: {output:?}"
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .expect("df reports available bytes")
            .trim()
            .parse::<u64>()
            .expect("df available bytes are numeric")
    };
    let available_bytes_after_fill = query_available_bytes();
    let filler_bytes = fs::metadata(&filler_path.0).unwrap().len();
    assert!(filler_bytes > 0, "the filler must consume filesystem space");

    // Request more segment payload than the entire bounded filesystem can hold. Repeated
    // borrowed slices keep memory bounded while ensuring any inode preallocation is exhausted.
    let disk_full_payload = vec![0xD4; MAX_SEGMENT_BYTES as usize];
    let disk_full_segment_count =
        usize::try_from(filesystem_bytes / u64::from(MAX_SEGMENT_BYTES) + 1)
            .expect("the bounded filesystem yields a representable segment count");
    let disk_full_segments = vec![&disk_full_payload[..]; disk_full_segment_count];
    let requested_payload_bytes = u64::try_from(disk_full_segment_count)
        .expect("the bounded filesystem yields a representable segment count")
        .checked_mul(u64::from(MAX_SEGMENT_BYTES))
        .expect("requested qualification payload length fits in u64");
    assert!(
        requested_payload_bytes > filesystem_bytes
            && requested_payload_bytes > available_bytes_after_fill,
        "the append request must exceed total filesystem size and remaining space"
    );
    let committed_segments_before_failure = line.segment_count();
    let append_result = line.append_segments(&disk_full_segments);
    assert_eq!(committed_segments_before_failure, 1);
    assert!(
        matches!(
            &append_result,
            Err(LineError::Io(error))
                if error.raw_os_error() == Some(libc::ENOSPC)
                    || error.kind() == io::ErrorKind::WriteZero
        ),
        "expected the append itself to return ENOSPC, got {append_result:?}"
    );
    drop(line);
    drop(filler);
    fs::remove_file(&filler_path.0).unwrap();

    let recovered = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(
        recovered.segment_count(),
        committed_segments_before_failure,
        "the failed append must leave the last successful witness authoritative"
    );
    let verification = recovered.verify().unwrap();
    assert_eq!(verification.segments_verified, recovered.segment_count());
    assert_eq!(
        &recovered.open_segment(0).unwrap()[..],
        &initial_payload[..]
    );
    let recovered_tail_bytes = recovered.recovery_info().trailing_bytes;
    eprintln!(
        "HOLONOMY_ENOSPC_MEASUREMENT filesystem={fs_type}; mount_source={mount_source}; filesystem_bytes={filesystem_bytes}; filesystem_block_bytes={filesystem_block_bytes}; initial_free_bytes={initial_free_bytes}; baseline_end_aligned={}; filler_bytes={filler_bytes}; filler_enospc_phase={filler_enospc_phase}; available_bytes_after_fill={available_bytes_after_fill}; requested_full_segments={disk_full_segment_count}; requested_payload_bytes={requested_payload_bytes}; append_error={append_result:?}; recovered_segments={}; recovered_tail_bytes={recovered_tail_bytes}; verified_segments={}",
        baseline_committed_end % filesystem_block_bytes == 0,
        recovered.segment_count(),
        verification.segments_verified,
    );
}

#[test]
fn incomplete_tail_recovers_to_last_witness_then_is_removed_on_append() {
    let path = temp_path("crash-tail");
    let salt = [0x33; 64];
    let (_keys, capsule, dem) = test_material(b"crash tail", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"committed"[..]]).unwrap();

    let orphan = encode_segment_record(
        &line.header,
        &line.epoch,
        &dem,
        line.mmr.count(),
        &line.last_segment_tag,
        b"not committed",
    )
    .unwrap();
    let mut prospective_mmr = line.mmr.clone();
    prospective_mmr.append(orphan.leaf).unwrap();
    let (torn_witness, _) = encode_witness_record(
        &line.header,
        &line.epoch,
        &dem,
        line.next_witness_sequence,
        &line.last_witness_tag,
        &orphan.tag,
        &prospective_mmr,
        None,
    )
    .unwrap();
    let orphan_bytes = orphan.frame.len() as u64 + 23;
    {
        let mut file = OpenOptions::new().append(true).open(&path.0).unwrap();
        file.write_all(&orphan.frame).unwrap();
        file.write_all(&torn_witness[..23]).unwrap();
        file.sync_all().unwrap();
    }
    drop(line);

    let mut recovered = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(recovered.segment_count(), 1);
    assert_eq!(recovered.recovery_info().trailing_bytes, orphan_bytes);
    assert_eq!(&recovered.open_segment(0).unwrap()[..], b"committed");

    let receipt = recovered
        .append_segments(&[&b"after recovery"[..]])
        .unwrap();
    assert_eq!(receipt.segment_count, 2);
    assert!(!recovered.recovery_info().needs_recovery());
    drop(recovered);

    let reopened = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(reopened.segment_count(), 2);
    assert_eq!(&reopened.open_segment(1).unwrap()[..], b"after recovery");
    assert!(torn_witness.len() > 23);
}

#[test]
fn every_byte_cut_of_an_append_recovers_to_the_last_witness() {
    let path = temp_path("crash-matrix");
    let salt = [0x3A; 64];
    let (_keys, capsule, dem) = test_material(b"crash matrix", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    let baseline = fs::read(&path.0).unwrap();
    let receipt = line
        .append_segments(&[&b"first new slab"[..], &b"second new slab"[..]])
        .unwrap();
    let complete = fs::read(&path.0).unwrap();
    let tail = &complete[baseline.len()..];
    assert_eq!(tail.len() as u64, receipt.bytes_appended);
    drop(line);

    // Every byte prefix models a crash between two writes, including cuts within a frame
    // header, fixed metadata, ciphertext, and the final commit witness.
    for (cut, _) in tail.iter().enumerate() {
        let mut candidate = baseline.clone();
        candidate.extend_from_slice(&tail[..cut]);
        fs::write(&path.0, candidate).unwrap();

        let recovered = WorldLine::open(&path.0, dem.duplicate())
            .unwrap_or_else(|error| panic!("open failed after cut at byte {cut}: {error}"));
        assert_eq!(recovered.segment_count(), 0, "cut at byte {cut}");
        assert_eq!(
            recovered.recovery_info().trailing_bytes,
            cut as u64,
            "cut at byte {cut}"
        );
        assert_eq!(recovered.verify().unwrap().segments_verified, 0);
    }

    fs::write(&path.0, complete).unwrap();
    let committed = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(committed.segment_count(), 2);
    assert!(!committed.recovery_info().needs_recovery());
}

#[test]
fn injected_short_write_at_every_byte_reopens_to_the_last_commit() {
    let path = temp_path("injected-short-write-matrix");
    let salt = [0x42; 64];
    let (_keys, capsule, dem) = test_material(b"injected short writes", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"last committed segment"[..]])
        .unwrap();
    let baseline = fs::read(&path.0).unwrap();
    let new_segments: [&[u8]; 2] = [
        &b"first pending segment"[..],
        &b"second pending segment"[..],
    ];
    let sample = line.append_segments(&new_segments).unwrap();
    assert_eq!(
        fs::metadata(&path.0).unwrap().len() - baseline.len() as u64,
        sample.bytes_appended
    );
    drop(line);

    // Inject a short-write failure through the append writer at each byte position. Each error
    // poisons the handle; reopening must authenticate the prior witness and report only the
    // uncommitted tail, never expose either pending segment as committed.
    for cut in 0..sample.bytes_appended {
        fs::write(&path.0, &baseline).unwrap();
        let mut candidate = WorldLine::open(&path.0, dem.duplicate()).unwrap();
        let probe = AppendIoProbeGuard::install(Some(cut), None, false);
        let result = candidate.append_segments(&new_segments);
        let trace = probe.snapshot();
        drop(probe);

        assert!(
            matches!(result, Err(LineError::Io(_))),
            "cut after {cut} bytes"
        );
        assert_eq!(trace.bytes_written, cut, "cut after {cut} bytes");
        assert!(matches!(
            candidate.append_segments(&[&b"must reopen"[..]]),
            Err(LineError::ReopenRequired)
        ));
        drop(candidate);

        let recovered = WorldLine::open(&path.0, dem.duplicate())
            .unwrap_or_else(|error| panic!("open failed after injected cut {cut}: {error}"));
        assert_eq!(recovered.segment_count(), 1, "cut after {cut} bytes");
        assert_eq!(
            recovered.recovery_info().trailing_bytes,
            cut,
            "cut after {cut} bytes"
        );
        assert_eq!(
            &recovered.open_segment(0).unwrap()[..],
            b"last committed segment"
        );
        assert_eq!(recovered.verify().unwrap().segments_verified, 1);
    }
}

#[test]
fn injected_sync_failures_leave_only_authenticated_commit_states() {
    for fail_on_sync in [1, 2] {
        let path = temp_path("injected-sync-failure");
        let salt = [0x43; 64];
        let (_keys, capsule, dem) = test_material(b"injected sync failures", salt);
        let mut line =
            WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
        line.append_segments(&[&b"last committed segment"[..]])
            .unwrap();
        let baseline = fs::read(&path.0).unwrap();

        let probe = AppendIoProbeGuard::install(None, Some(fail_on_sync), false);
        let result = line.append_segments(&[&b"pending segment"[..]]);
        let trace = probe.snapshot();
        drop(probe);

        assert_eq!(trace.syncs.len(), fail_on_sync);
        assert!(!trace.syncs[fail_on_sync - 1].succeeded);
        assert_eq!(
            trace.syncs.last().unwrap().bytes_written,
            trace.bytes_written
        );
        if fail_on_sync == 1 {
            assert!(matches!(result, Err(LineError::Io(_))));
            assert_eq!(trace.syncs[0].bytes_written, trace.bytes_written);
        } else {
            assert!(trace.syncs[0].succeeded);
            assert!(matches!(result, Err(LineError::DurabilityUncertain)));
        }
        assert!(matches!(
            line.append_segments(&[&b"must reopen"[..]]),
            Err(LineError::ReopenRequired)
        ));
        drop(line);

        let written = fs::read(&path.0).unwrap();
        assert_eq!(&written[..baseline.len()], &baseline);
        assert_eq!(
            written.len() as u64 - baseline.len() as u64,
            trace.bytes_written
        );
        let observed = WorldLine::open(&path.0, dem.duplicate()).unwrap();
        if fail_on_sync == 1 {
            assert_eq!(observed.segment_count(), 1);
            assert_eq!(observed.recovery_info().trailing_bytes, trace.bytes_written);
            assert_eq!(observed.verify().unwrap().segments_verified, 1);
        } else {
            // A failed final sync can leave either the old commit or the complete new commit
            // durable. This run observes the latter; both states must authenticate cleanly.
            assert_eq!(observed.segment_count(), 2);
            assert!(!observed.recovery_info().needs_recovery());
            assert_eq!(observed.verify().unwrap().segments_verified, 2);
            drop(observed);
            fs::write(&path.0, &baseline).unwrap(); // model loss of the unsynced commit tail
            let lost_tail = WorldLine::open(&path.0, dem.duplicate()).unwrap();
            assert_eq!(lost_tail.segment_count(), 1);
            assert_eq!(lost_tail.verify().unwrap().segments_verified, 1);
        }
    }
}

#[test]
fn injected_recovery_truncate_failures_preserve_the_last_commit() {
    for fail_truncate in [true, false] {
        let path = temp_path("injected-recovery-truncate");
        let salt = [0x44; 64];
        let (_keys, capsule, dem) = test_material(b"injected recovery truncate", salt);
        let mut line =
            WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
        line.append_segments(&[&b"committed before recovery"[..]])
            .unwrap();
        let committed_end = line.committed_end;
        drop(line);
        let orphan_tail = b"W6RC\x01\x00"; // an incomplete record header at EOF
        OpenOptions::new()
            .append(true)
            .open(&path.0)
            .unwrap()
            .write_all(orphan_tail)
            .unwrap();

        let mut recovering = WorldLine::open(&path.0, dem.duplicate()).unwrap();
        assert_eq!(
            recovering.recovery_info().trailing_bytes,
            orphan_tail.len() as u64
        );
        let fail_on_sync = (!fail_truncate).then_some(1);
        let probe = AppendIoProbeGuard::install(None, fail_on_sync, fail_truncate);
        let result = recovering.append_segments(&[&b"after recovery"[..]]);
        let trace = probe.snapshot();
        drop(probe);
        assert!(matches!(result, Err(LineError::Io(_))));
        assert!(trace.writes.is_empty());
        assert_eq!(trace.truncations.len(), 1);
        assert_eq!(trace.truncations[0].length, committed_end);
        assert_eq!(trace.truncations[0].succeeded, !fail_truncate);
        if fail_truncate {
            assert!(trace.syncs.is_empty());
        } else {
            assert_eq!(trace.syncs.len(), 1);
            assert!(!trace.syncs[0].succeeded);
        }
        drop(recovering);

        let reopened = WorldLine::open(&path.0, dem.duplicate()).unwrap();
        assert_eq!(reopened.segment_count(), 1);
        assert_eq!(
            &reopened.open_segment(0).unwrap()[..],
            b"committed before recovery"
        );
        assert_eq!(
            reopened.recovery_info().trailing_bytes,
            if fail_truncate {
                orphan_tail.len() as u64
            } else {
                0
            }
        );
        assert_eq!(reopened.verify().unwrap().segments_verified, 1);
    }
}

#[test]
fn open_seeks_over_segment_bodies_but_verify_authenticates_them() {
    let path = temp_path("lazy-open");
    let salt = [0x34; 64];
    let (_keys, capsule, dem) = test_material(b"lazy open", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"old historical payload"[..]])
        .unwrap();
    line.append_segments(&[&b"newer payload"[..]]).unwrap();
    let body_start = line.segments[0].ciphertext_offset;
    let bytes = fs::read(&path.0).unwrap();
    let forbidden = line
        .segments
        .iter()
        .map(|descriptor| {
            descriptor.ciphertext_offset
                ..descriptor.ciphertext_offset + u64::from(descriptor.ciphertext_len)
        })
        .collect();

    let mut guarded = GuardedReader {
        cursor: Cursor::new(bytes),
        forbidden,
    };
    let header = LineHeader::read_from(&mut guarded).unwrap();
    let epoch = header.epoch_id();
    let scanned = scan_records(&mut guarded, &header, &epoch, &dem).unwrap();
    assert_eq!(scanned.segments.len(), 2);

    drop(line);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path.0)
        .unwrap();
    file.seek(SeekFrom::Start(body_start)).unwrap();
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 0x80;
    file.seek(SeekFrom::Start(body_start)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
    drop(file);

    // The metadata-only open still succeeds; the explicit full verifier catches the damaged
    // ciphertext when it faults the segment in.
    let reopened = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert!(matches!(
        reopened.verify(),
        Err(LineError::AuthenticationRejected)
    ));
}

#[test]
fn open_rejects_flipped_metadata_in_an_older_committed_segment() {
    let path = temp_path("historical-metadata-flip");
    let salt = [0x3B; 64];
    let (_keys, capsule, dem) = test_material(b"metadata flip", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"historical segment"[..]]).unwrap();
    line.append_segments(&[&b"newer segment"[..]]).unwrap();
    let tag_offset = line.segments[0].ciphertext_offset - 32;
    drop(line);

    let mut bytes = fs::read(&path.0).unwrap();
    bytes[tag_offset as usize] ^= 1;
    fs::write(&path.0, bytes).unwrap();
    assert!(matches!(
        WorldLine::open(&path.0, dem.duplicate()),
        Err(LineError::AuthenticationRejected)
    ));
}

#[test]
fn capsule_replay_across_files_is_rejected_by_the_envelope_epoch() {
    let path_a = temp_path("capsule-replay-a");
    let path_b = temp_path("capsule-replay-b");
    let salt = [0x35; 64];
    let keys = MasterKeys::derive(b"same passphrase", &salt, None, ParamsClass::Test).unwrap();
    let (capsule_a, dem_a) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let header_a = test_header(salt, capsule_a.clone());
    let header_b = test_header(salt, capsule_a);
    assert_ne!(header_a.file_id(), header_b.file_id());

    let mut line_a = WorldLine::create(&path_a.0, header_a, dem_a.duplicate()).unwrap();
    line_a.append_segments(&[&b"file A content"[..]]).unwrap();
    drop(line_a);

    // Replay file A's authenticated record chain under a second file ID that carries the exact
    // same capsule and KDF material. Only the envelope epoch differs, so witness open must fail.
    let bytes_a = fs::read(&path_a.0).unwrap();
    let mut replayed = header_b.encode();
    replayed.extend_from_slice(&bytes_a[crate::format::HEADER_BYTES..]);
    fs::write(&path_b.0, replayed).unwrap();

    assert!(matches!(
        WorldLine::open(&path_b.0, dem_a.duplicate()),
        Err(LineError::AuthenticationRejected)
    ));
}

#[test]
fn compaction_is_explicit_passphrase_confirmed_and_rebases_to_a_new_file() {
    let source = temp_path("compact-source");
    let destination = temp_path("compact-destination");
    let salt = [0x38; 64];
    let params = test_params();
    let keys = MasterKeys::derive(b"compact me", &salt, None, ParamsClass::Test).unwrap();
    let (old_capsule, old_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let old_header = test_header(salt, old_capsule);
    let old_file_id = *old_header.file_id();
    let mut line = WorldLine::create(&source.0, old_header, old_dem.duplicate()).unwrap();
    line.append_segments(&[&b"old draft"[..]]).unwrap();
    line.append_segments(&[&b"current draft"[..]]).unwrap();
    let source_bytes = fs::read(&source.0).unwrap();

    let approval = holonomy_seal::confirm_compaction(
        b"compact me",
        &salt,
        None,
        &params,
        keys.unwrap_key(),
        &old_file_id,
    )
    .unwrap();
    let (new_capsule, new_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let compacted = line
        .compact_to(
            &destination.0,
            &[1],
            test_header(salt, new_capsule),
            &new_dem,
            approval,
        )
        .unwrap();

    assert_eq!(line.segment_count(), 2); // The source remains untouched.
    assert_eq!(fs::read(&source.0).unwrap(), source_bytes);
    assert_eq!(compacted.segment_count(), 1);
    assert_eq!(&compacted.open_segment(0).unwrap()[..], b"current draft");
    assert!(compacted.verify().is_ok());
}

#[test]
fn compaction_rejects_wrong_file_approval_and_reused_identity() {
    let source = temp_path("compact-reject-source");
    let destination = temp_path("compact-reject-destination");
    let salt = [0x3C; 64];
    let params = test_params();
    let keys = MasterKeys::derive(b"confirm compaction", &salt, None, ParamsClass::Test).unwrap();
    let (capsule, dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let header = test_header(salt, capsule);
    let file_id = *header.file_id();
    let mut wrong_file_id = file_id;
    wrong_file_id[0] ^= 1;
    let mut line = WorldLine::create(&source.0, header, dem.duplicate()).unwrap();
    line.append_segments(&[&b"keep me"[..]]).unwrap();

    let wrong_file_approval = holonomy_seal::confirm_compaction(
        b"confirm compaction",
        &salt,
        None,
        &params,
        keys.unwrap_key(),
        &wrong_file_id,
    )
    .unwrap();
    let (new_capsule, new_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    assert!(matches!(
        line.compact_to(
            &destination.0,
            &[0],
            test_header(salt, new_capsule),
            &new_dem,
            wrong_file_approval,
        ),
        Err(LineError::CompactionConfirmationRequired)
    ));
    assert!(!destination.0.exists());

    let correct_approval = holonomy_seal::confirm_compaction(
        b"confirm compaction",
        &salt,
        None,
        &params,
        keys.unwrap_key(),
        &file_id,
    )
    .unwrap();
    assert!(matches!(
        line.compact_to(
            &destination.0,
            &[0],
            line.header().clone(),
            &dem,
            correct_approval,
        ),
        Err(LineError::CompactionIdentityReuse)
    ));
    assert!(!destination.0.exists());
}

#[test]
fn compaction_does_not_replace_an_existing_destination() {
    let source = temp_path("compact-no-replace-source");
    let destination = temp_path("compact-no-replace-destination");
    let salt = [0x41; 64];
    let params = test_params();
    let keys = MasterKeys::derive(b"preserve destination", &salt, None, ParamsClass::Test).unwrap();
    let (old_capsule, old_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let old_header = test_header(salt, old_capsule);
    let old_file_id = *old_header.file_id();
    let mut line = WorldLine::create(&source.0, old_header, old_dem.duplicate()).unwrap();
    line.append_segments(&[&b"keep source"[..]]).unwrap();
    let source_bytes = fs::read(&source.0).unwrap();

    let existing = b"do not replace this destination";
    fs::write(&destination.0, existing).unwrap();
    let approval = holonomy_seal::confirm_compaction(
        b"preserve destination",
        &salt,
        None,
        &params,
        keys.unwrap_key(),
        &old_file_id,
    )
    .unwrap();
    let (new_capsule, new_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let new_header = test_header(salt, new_capsule);
    let temporary = compaction_path(&destination.0, new_header.file_id());

    assert!(matches!(
        line.compact_to(&destination.0, &[0], new_header, &new_dem, approval,),
        Err(LineError::Io(_))
    ));
    assert_eq!(fs::read(&source.0).unwrap(), source_bytes);
    assert_eq!(fs::read(&destination.0).unwrap().as_slice(), existing);
    assert!(!temporary.exists());
}

#[test]
fn a_valid_prefix_rollback_cannot_be_detected_without_external_state() {
    // This test deliberately records RFC-002's offline rollback limit. The file authenticates
    // its chain but, without a trusted head stored elsewhere, a byte-perfect earlier prefix is
    // indistinguishable from a file that simply stopped at that witness.
    let path = temp_path("prefix-rollback");
    let salt = [0x36; 64];
    let (_keys, capsule, dem) = test_material(b"rollback", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"version one"[..]]).unwrap();
    let first_commit_end = line.committed_end;
    line.append_segments(&[&b"version two"[..]]).unwrap();
    drop(line);

    OpenOptions::new()
        .write(true)
        .open(&path.0)
        .unwrap()
        .set_len(first_commit_end)
        .unwrap();
    let rolled_back = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(rolled_back.segment_count(), 1);
    assert!(rolled_back.verify().is_ok());
}

#[cfg(unix)]
#[test]
fn open_refuses_symlink_paths() {
    use std::os::unix::fs::symlink;

    let path = temp_path("path-safety");
    let salt = [0x39; 64];
    let (_keys, capsule, dem) = test_material(b"path safety", salt);
    let line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    drop(line);

    let link = temp_path("symlink");
    symlink(&path.0, &link.0).unwrap();
    assert!(WorldLine::open(&link.0, dem.duplicate()).is_err());
}

#[test]
fn open_rejects_a_weak_or_unbounded_header_before_any_kdf_is_needed() {
    let path = temp_path("kdf-fence");
    let salt = [0x37; 64];
    let (_keys, capsule, dem) = test_material(b"kdf fence", salt);
    let header = test_header(salt, capsule);
    let mut line = WorldLine::create(&path.0, header, dem.duplicate()).unwrap();
    line.append_segments(&[&b"header"[..]]).unwrap();
    drop(line);

    let mut bytes = fs::read(&path.0).unwrap();
    let m_kib_offset = 8 + 2 + 2 + 16;
    bytes[m_kib_offset..m_kib_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&path.0, bytes).unwrap();
    assert!(matches!(
        LineHeader::read(&path.0),
        Err(LineError::KdfParametersRejected)
    ));
}

#[test]
fn manifest_persists_mid_segment_insert_cut_and_empty_document() {
    let path = temp_path("manifest-geometry");
    let salt = [0x51; 64];
    let (_keys, capsule, dem) = test_material(b"manifest geometry", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    assert!(!line.has_manifest());
    assert!(line.document_slabs().is_empty());

    line.append_segments(&[&b"hello world"[..]]).unwrap();
    assert!(!line.has_manifest());
    assert_eq!(
        line.document_slabs(),
        vec![LineSlab {
            segment: 0,
            start: 0,
            len: 11,
        }]
    );

    let inserted = [
        LineSlab {
            segment: 0,
            start: 0,
            len: 6,
        },
        LineSlab {
            segment: 1,
            start: 0,
            len: 5,
        },
        LineSlab {
            segment: 0,
            start: 6,
            len: 5,
        },
    ];
    line.append_segments_with_manifest(&[&b"dear "[..]], &inserted)
        .unwrap();
    assert!(line.has_manifest());
    assert_eq!(line.witness_count(), 3);
    drop(line);

    let mut reopened = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert!(reopened.has_manifest());
    assert_eq!(reopened.document_slabs(), inserted);
    assert_eq!(reopened.verify().unwrap().segments_verified, 2);

    // Extending an already-manifested world-line via plain `append_segments` preserves the
    // active geometry and appends the new full segment to the manifest.
    reopened.append_segments(&[&b"!"[..]]).unwrap();
    let mut expected = inserted.to_vec();
    expected.push(LineSlab {
        segment: 2,
        start: 0,
        len: 1,
    });
    assert_eq!(reopened.document_slabs(), expected);
    drop(reopened);

    // Emptying the entire document (`Select All` + `Delete`) persists a zero-slab manifest
    // rather than falling back to append order.
    let mut emptied = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(emptied.document_slabs(), expected);
    emptied.commit_manifest(&[]).unwrap();
    assert!(emptied.has_manifest());
    assert!(emptied.document_slabs().is_empty());
    drop(emptied);

    let final_open = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert!(final_open.has_manifest());
    assert!(final_open.document_slabs().is_empty());
    assert_eq!(final_open.segment_count(), 3);
    assert_eq!(final_open.verify().unwrap().segments_verified, 3);
}

#[test]
fn open_seeks_over_historical_manifests_and_verify_authenticates_them() {
    let path = temp_path("manifest-lazy-open");
    let salt = [0x52; 64];
    let (_keys, capsule, dem) = test_material(b"manifest lazy open", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"first"[..], &b"second"[..]])
        .unwrap();
    line.commit_manifest(&[
        LineSlab {
            segment: 1,
            start: 0,
            len: 6,
        },
        LineSlab {
            segment: 0,
            start: 0,
            len: 5,
        },
    ])
    .unwrap();
    line.commit_manifest(&[LineSlab {
        segment: 1,
        start: 0,
        len: 6,
    }])
    .unwrap();
    assert_eq!(line.manifests.len(), 2);

    let stale_manifest = line.manifests[0].clone();
    let active_manifest = line.manifests[1].clone();
    let mut forbidden: Vec<Range<u64>> = line
        .segments
        .iter()
        .map(|descriptor| {
            descriptor.ciphertext_offset
                ..descriptor.ciphertext_offset + u64::from(descriptor.ciphertext_len)
        })
        .collect();
    forbidden.push(
        stale_manifest.ciphertext_offset
            ..stale_manifest.ciphertext_offset + u64::from(stale_manifest.ciphertext_len),
    );
    drop(line);

    let baseline = fs::read(&path.0).unwrap();
    let mut guarded = GuardedReader {
        cursor: Cursor::new(baseline.clone()),
        forbidden,
    };
    let header = LineHeader::read_from(&mut guarded).unwrap();
    let epoch = header.epoch_id();
    let scanned = scan_records(&mut guarded, &header, &epoch, &dem).unwrap();
    assert_eq!(
        scanned.manifest,
        Some(LineManifest {
            slabs: vec![LineSlab {
                segment: 1,
                start: 0,
                len: 6,
            }],
            styles: Vec::new(),
            links: Vec::new(),
        })
    );

    // Flipping a byte in a superseded historical manifest does not break lazy open, and full
    // `verify()` catches the tampering.
    let mut historical_damaged = baseline.clone();
    historical_damaged[stale_manifest.ciphertext_offset as usize] ^= 0x40;
    fs::write(&path.0, &historical_damaged).unwrap();
    let reopened = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert!(matches!(
        reopened.verify(),
        Err(LineError::AuthenticationRejected)
    ));
    drop(reopened);

    // Flipping a byte in the active manifest is caught immediately on `open()`.
    let mut active_damaged = baseline;
    active_damaged[active_manifest.ciphertext_offset as usize] ^= 0x40;
    fs::write(&path.0, &active_damaged).unwrap();
    assert!(matches!(
        WorldLine::open(&path.0, dem.duplicate()),
        Err(LineError::AuthenticationRejected)
    ));
}

#[test]
fn short_writes_at_every_byte_of_manifest_commits_recover_cleanly() {
    let base_path = temp_path("manifest-short-write-base");
    let salt = [0x53; 64];
    let (_keys, capsule, dem) = test_material(b"manifest short write", salt);
    let mut base =
        WorldLine::create(&base_path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    base.append_segments(&[&b"hello world"[..]]).unwrap();
    let baseline = fs::read(&base_path.0).unwrap();

    let target_slabs = [
        LineSlab {
            segment: 0,
            start: 0,
            len: 6,
        },
        LineSlab {
            segment: 1,
            start: 0,
            len: 5,
        },
        LineSlab {
            segment: 0,
            start: 6,
            len: 5,
        },
    ];
    let probe = AppendIoProbeGuard::install(None, None, false);
    base.append_segments_with_manifest(&[&b"dear "[..]], &target_slabs)
        .unwrap();
    let total_bytes = probe.snapshot().bytes_written;
    drop(probe);
    drop(base);

    for cut in 0..total_bytes {
        let trial_path = temp_path("manifest-short-write-trial");
        fs::write(&trial_path.0, &baseline).unwrap();
        let mut line = WorldLine::open(&trial_path.0, dem.duplicate()).unwrap();
        let probe = AppendIoProbeGuard::install(Some(cut), None, false);
        let result = line.append_segments_with_manifest(&[&b"dear "[..]], &target_slabs);
        drop(probe);
        assert!(result.is_err(), "cut at {cut} unexpectedly succeeded");
        assert!(matches!(
            line.commit_manifest(&[]),
            Err(LineError::ReopenRequired)
        ));
        drop(line);

        let mut recovered = WorldLine::open(&trial_path.0, dem.duplicate()).unwrap();
        assert!(!recovered.has_manifest());
        assert_eq!(recovered.segment_count(), 1);
        assert_eq!(recovered.recovery_info().trailing_bytes, cut);
        assert_eq!(recovered.verify().unwrap().segments_verified, 1);

        // Next commit truncates the uncommitted tail and succeeds cleanly.
        recovered.commit_manifest(&[]).unwrap();
        assert_eq!(recovered.recovery_info().trailing_bytes, 0);
        assert!(recovered.document_slabs().is_empty());
        assert_eq!(recovered.verify().unwrap().segments_verified, 1);
    }
}

#[test]
fn multi_segment_styled_transaction_recovers_old_manifest_after_every_short_write() {
    let base_path = temp_path("atomic-replace-base");
    let salt = [0x5a; 64];
    let (_keys, capsule, dem) = test_material(b"atomic replace", salt);
    let mut base =
        WorldLine::create(&base_path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    base.append_segments(&[&b"hello world"[..]]).unwrap();
    let baseline = fs::read(&base_path.0).unwrap();
    let manifest = LineManifest {
        slabs: vec![
            LineSlab {
                segment: 0,
                start: 0,
                len: 6,
            },
            LineSlab {
                segment: 1,
                start: 0,
                len: 5,
            },
            LineSlab {
                segment: 2,
                start: 0,
                len: 5,
            },
            LineSlab {
                segment: 0,
                start: 6,
                len: 5,
            },
        ],
        styles: vec![LineStyleRun {
            start: 0,
            end: 21,
            flags: 1,
            family: 0,
            heading: 0,
            alignment: 0,
            list_kind: 0,
            list_depth: 0,
            blockquote: 0,
            size_milli: 1000,
            list_start: 0,
            link_id: 0,
        }],
        links: Vec::new(),
    };
    let chunks: &[&[u8]] = &[b"dear ", b"kind "];
    let probe = AppendIoProbeGuard::install(None, None, false);
    let before_witnesses = base.witness_count();
    base.append_segments_with_full_manifest(chunks, &manifest)
        .unwrap();
    let total_bytes = probe.snapshot().bytes_written;
    drop(probe);
    assert_eq!(base.witness_count(), before_witnesses + 1);
    drop(base);
    let reopened = WorldLine::open(&base_path.0, dem.duplicate()).unwrap();
    assert_eq!(reopened.document_manifest(), manifest);
    assert_eq!(reopened.segment_count(), 3);
    drop(reopened);

    for cut in 0..total_bytes {
        let trial_path = temp_path("atomic-replace-trial");
        fs::write(&trial_path.0, &baseline).unwrap();
        let mut line = WorldLine::open(&trial_path.0, dem.duplicate()).unwrap();
        let probe = AppendIoProbeGuard::install(Some(cut), None, false);
        let result = line.append_segments_with_full_manifest(chunks, &manifest);
        drop(probe);
        assert!(result.is_err(), "cut at {cut} unexpectedly succeeded");
        drop(line);
        let recovered = WorldLine::open(&trial_path.0, dem.duplicate()).unwrap();
        assert_eq!(recovered.segment_count(), 1, "cut at {cut}");
        assert!(!recovered.has_manifest(), "cut at {cut}");
        assert_eq!(&recovered.open_segment(0).unwrap()[..], b"hello world");
        assert_eq!(recovered.verify().unwrap().segments_verified, 1);
    }
}

#[test]
fn invalid_manifests_are_refused_without_poisoning_or_writing() {
    let path = temp_path("invalid-manifest");
    let salt = [0x54; 64];
    let (_keys, capsule, dem) = test_material(b"invalid manifest", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"five!"[..]]).unwrap();
    let before = fs::read(&path.0).unwrap();

    for bad in [
        LineSlab {
            segment: 0,
            start: 0,
            len: 0,
        },
        LineSlab {
            segment: 0,
            start: 2,
            len: 4,
        },
        LineSlab {
            segment: 1,
            start: 0,
            len: 1,
        },
        LineSlab {
            segment: 0,
            start: u32::MAX,
            len: 2,
        },
    ] {
        assert!(matches!(
            line.commit_manifest(&[bad]),
            Err(LineError::InvalidManifest(_))
        ));
    }
    assert_eq!(fs::read(&path.0).unwrap(), before);
    line.commit_manifest(&[LineSlab {
        segment: 0,
        start: 0,
        len: 4,
    }])
    .unwrap();
    assert_eq!(line.verify().unwrap().segments_verified, 1);
}

#[test]
fn manifest_validation_matches_the_style_decoder() {
    // D-99: the commit path accepted wire values the text-layer decoder refuses as a whole. Such
    // a manifest reopens with the entire run missing — authenticated bytes, less formatting — so
    // validation and decoding must agree before the witness is written.
    fn style_run(list_kind: u8, list_depth: u8, list_start: u32, link_id: u32) -> LineStyleRun {
        LineStyleRun {
            start: 0,
            end: 5,
            flags: 1,
            family: 0,
            heading: 0,
            alignment: 0,
            list_kind,
            list_depth,
            blockquote: 0,
            size_milli: 1_000,
            list_start,
            link_id,
        }
    }

    let path = temp_path("manifest-style-validation");
    let salt = [0x57; 64];
    let (_keys, capsule, dem) = test_material(b"manifest style validation", salt);
    let mut line = WorldLine::create(&path.0, test_header(salt, capsule), dem.duplicate()).unwrap();
    line.append_segments(&[&b"hello"[..]]).unwrap();

    let mut manifest = LineManifest {
        slabs: vec![LineSlab {
            segment: 0,
            start: 0,
            len: 5,
        }],
        styles: vec![style_run(1, 1, 0, 0)],
        links: Vec::new(),
    };
    line.commit_full_manifest(&manifest)
        .expect("a bullet at depth 1 is valid");
    manifest.styles = vec![style_run(2, 8, 3, 0)];
    line.commit_full_manifest(&manifest)
        .expect("an ordered item at depth 8 starting at 3 is valid");

    for bad in [
        style_run(1, 0, 0, 0), // bullet with no depth
        style_run(1, 1, 1, 0), // bullet carrying an ordinal
        style_run(2, 0, 1, 0), // ordered item with no depth
        style_run(0, 0, 1, 0), // plain text carrying an ordinal
        style_run(0, 0, 0, 1), // link id with an empty link table
    ] {
        manifest.styles = vec![bad];
        assert!(
            matches!(
                line.commit_full_manifest(&manifest),
                Err(LineError::InvalidManifest(_))
            ),
            "a wire value the decoder refuses must not be committed"
        );
    }

    // Links are 1-based: with one target, 1 is valid and 2 is one past the end.
    manifest.links = vec!["https://example.invalid/a".to_owned()];
    manifest.styles = vec![style_run(0, 0, 0, 1)];
    line.commit_full_manifest(&manifest)
        .expect("the first link target is valid");
    manifest.styles = vec![style_run(0, 0, 0, 2)];
    assert!(matches!(
        line.commit_full_manifest(&manifest),
        Err(LineError::InvalidManifest(_))
    ));

    assert_eq!(line.verify().unwrap().segments_verified, 1);
}

#[test]
fn compact_active_to_erases_deleted_bytes_and_resets_manifest() {
    let source = temp_path("compact-active-source");
    let destination = temp_path("compact-active-destination");
    let salt = [0x55; 64];
    let params = test_params();
    let keys = MasterKeys::derive(b"compact active", &salt, None, ParamsClass::Test).unwrap();
    let (old_capsule, old_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let old_header = test_header(salt, old_capsule);
    let old_file_id = *old_header.file_id();
    let mut line = WorldLine::create(&source.0, old_header, old_dem.duplicate()).unwrap();
    line.append_segments(&[&b"discarded draft -- kept prose"[..], &b"discardedseg"[..]])
        .unwrap();
    line.commit_manifest(&[LineSlab {
        segment: 0,
        start: 19,
        len: 10,
    }])
    .unwrap();

    let approval = holonomy_seal::confirm_compaction(
        b"compact active",
        &salt,
        None,
        &params,
        keys.unwrap_key(),
        &old_file_id,
    )
    .unwrap();
    let (new_capsule, new_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let new_header = test_header(salt, new_capsule);
    let compacted = line
        .compact_active_to(&destination.0, new_header, &new_dem, approval)
        .unwrap();

    assert!(!compacted.has_manifest());
    assert_eq!(compacted.segment_count(), 1);
    assert_eq!(compacted.witness_count(), 2);
    assert_eq!(&compacted.open_segment(0).unwrap()[..], b"kept prose");
    assert_eq!(compacted.verify().unwrap().plaintext_bytes_verified, 10);
}

#[test]
fn styled_manifest_roundtrips_and_survives_active_compaction() {
    let source = temp_path("styled-manifest-source");
    let destination = temp_path("styled-manifest-compacted");
    let salt = [0x66; 64];
    let params = test_params();
    let keys = MasterKeys::derive(b"styled manifest", &salt, None, ParamsClass::Test).unwrap();
    let (old_capsule, old_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let old_header = test_header(salt, old_capsule);
    let old_file_id = *old_header.file_id();
    let mut line = WorldLine::create(&source.0, old_header, old_dem.duplicate()).unwrap();

    let manifest = LineManifest {
        slabs: vec![LineSlab {
            segment: 0,
            start: 7,
            len: 11,
        }],
        styles: vec![
            LineStyleRun {
                start: 0,
                end: 5,
                flags: 0x05,
                family: 1,
                heading: 2,
                alignment: 1,
                list_kind: 0,
                list_depth: 0,
                blockquote: 0,
                size_milli: 1_550,
                list_start: 0,
                link_id: 0,
            },
            LineStyleRun {
                start: 6,
                end: 11,
                flags: 0x02,
                family: 0,
                heading: 0,
                alignment: 0,
                list_kind: 2,
                list_depth: 1,
                blockquote: 1,
                size_milli: 1_000,
                list_start: 1,
                link_id: 1,
            },
        ],
        links: vec!["https://example.invalid/spec".to_owned()],
    };
    line.append_segments_with_full_manifest(&[&b"draft: hello world"[..]], &manifest)
        .unwrap();

    let reopened = WorldLine::open(&source.0, old_dem.duplicate()).unwrap();
    assert_eq!(reopened.document_manifest(), manifest);
    assert_eq!(reopened.verify().unwrap().segments_verified, 1);

    let mut out_of_bounds = manifest.clone();
    out_of_bounds.styles[1].end = 12;
    assert!(matches!(
        line.commit_full_manifest(&out_of_bounds),
        Err(LineError::InvalidManifest(_))
    ));

    let approval = holonomy_seal::confirm_compaction(
        b"styled manifest",
        &salt,
        None,
        &params,
        keys.unwrap_key(),
        &old_file_id,
    )
    .unwrap();
    let (new_capsule, new_dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let new_header = test_header(salt, new_capsule);
    let compacted = reopened
        .compact_active_to(&destination.0, new_header, &new_dem, approval)
        .unwrap();
    let compacted_manifest = compacted.document_manifest();
    assert_eq!(compacted_manifest.styles, manifest.styles);
    assert_eq!(compacted_manifest.links, manifest.links);
    assert_eq!(&compacted.open_segment(0).unwrap()[..], b"hello world");
}

struct GuardedReader {
    cursor: Cursor<Vec<u8>>,
    forbidden: Vec<Range<u64>>,
}

impl Read for GuardedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let start = self.cursor.position();
        let count = self.cursor.read(buffer)?;
        let end = start + count as u64;
        assert!(
            !self
                .forbidden
                .iter()
                .any(|range| start < range.end && end > range.start),
            "open read segment ciphertext at {start}..{end}"
        );
        Ok(count)
    }
}

impl Seek for GuardedReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.cursor.seek(position)
    }
}

#[test]
fn witness_history_manifest_time_travel_and_bookmarks_survive_reopen_and_tamper() {
    let path = temp_path("history-bookmarks");
    let salt = [0x91; 64];
    let (_keys, capsule, dem) = test_material(b"history-bookmarks", salt);
    let header = test_header(salt, capsule);
    let mut line = WorldLine::create(&path.0, header, dem.duplicate()).unwrap();

    // Genesis witness 0: empty.
    let hist0 = line.witness_history();
    assert_eq!(hist0.len(), 1);
    assert_eq!(hist0[0].sequence, 0);
    assert!(line.manifest_at_witness(0).unwrap().slabs.is_empty());

    // Witness 1: append two implicit segments.
    line.append_segments(&[&b"first "[..], &b"draft"[..]])
        .unwrap();
    line.add_bookmark(1, 1_700_000_001, "Initial Draft")
        .unwrap();

    // Witness 2: cut & style via explicit manifest.
    let m2 = LineManifest {
        slabs: vec![LineSlab {
            segment: 0,
            start: 0,
            len: 5,
        }],
        styles: vec![LineStyleRun {
            start: 0,
            end: 5,
            flags: 0x01,
            family: 0,
            heading: 0,
            alignment: 0,
            list_kind: 0,
            list_depth: 0,
            blockquote: 0,
            size_milli: 1_000,
            list_start: 0,
            link_id: 0,
        }],
        links: Vec::new(),
    };
    line.commit_full_manifest(&m2).unwrap();
    line.add_bookmark(2, 1_700_000_099, "Trimmed Bold").unwrap();

    // Reject bookmark pointing past committed witness sequence.
    assert!(matches!(
        line.add_bookmark(99, 1_700_000_100, "Future"),
        Err(LineError::NoSuchWitness)
    ));
    // Reject empty or oversized bookmark names.
    assert!(matches!(
        line.add_bookmark(2, 0, ""),
        Err(LineError::InvalidBookmark(_))
    ));
    drop(line);

    let reopened = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(reopened.witness_history().len(), 3);
    assert_eq!(reopened.bookmarks().len(), 2);
    assert_eq!(reopened.bookmarks()[0].name, "Initial Draft");
    assert_eq!(reopened.bookmarks()[1].witness_sequence, 2);
    let at1 = reopened.manifest_at_witness(1).unwrap();
    assert_eq!(at1.slabs.len(), 2);
    assert_eq!(at1.slabs[0].segment, 0);
    assert_eq!(at1.slabs[1].segment, 1);
    let at2 = reopened.manifest_at_witness(2).unwrap();
    assert_eq!(at2, m2);
    assert!(matches!(
        reopened.manifest_at_witness(3),
        Err(LineError::NoSuchWitness)
    ));
    assert_eq!(reopened.verify().unwrap().witnesses_verified, 3);
    drop(reopened);

    // Truncating mid-bookmark record yields torn tail recovery without losing witness 2 or bookmark 0.
    let file_len = fs::metadata(&path.0).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(&path.0)
        .unwrap()
        .set_len(file_len - 4)
        .unwrap();
    let recovered = WorldLine::open(&path.0, dem.duplicate()).unwrap();
    assert_eq!(recovered.witness_history().len(), 3);
    assert_eq!(recovered.bookmarks().len(), 1);
    assert_eq!(recovered.bookmarks()[0].name, "Initial Draft");
    assert_eq!(recovered.document_manifest(), m2);
}

#[test]
fn streaming_tx_publishes_atomically_and_aborts_cleanly_without_corrupting_worldline() {
    let path = temp_path("streaming-tx");
    let salt = [0x92; 64];
    let (_keys, capsule, dem) = test_material(b"streaming-tx", salt);
    let header = test_header(salt, capsule);
    let mut line = WorldLine::create(&path.0, header, dem.duplicate()).unwrap();
    line.append_segments(&[&b"seed text"[..]]).unwrap();

    // Uncommitted / aborted stream leaves witness count unchanged and line usable.
    let mut aborted = line.start_streaming_tx().unwrap();
    aborted.push_segment(b"uncommitted one").unwrap();
    aborted.push_segment(b"uncommitted two").unwrap();
    line.abort_streaming_tx(aborted).unwrap();
    assert_eq!(line.witness_count(), 2);
    assert_eq!(line.segment_count(), 1);
    assert!(!line.recovery_info().needs_recovery());

    // Committed stream publishes multiple segments + explicit manifest under ONE witness.
    let mut tx = line.start_streaming_tx().unwrap();
    let s1 = tx.push_segment(b"alpha ").unwrap();
    let s2 = tx.push_segment(b"omega").unwrap();
    let manifest = LineManifest {
        slabs: vec![s1, s2],
        styles: Vec::new(),
        links: Vec::new(),
    };
    let receipt = line.finish_streaming_tx(tx, Some(&manifest)).unwrap();
    assert_eq!(receipt.witness_sequence, 2);
    assert_eq!(line.segment_count(), 3);
    assert_eq!(line.document_manifest(), manifest);
    assert_eq!(&line.open_segment(1).unwrap()[..], b"alpha ");
    assert_eq!(&line.open_segment(2).unwrap()[..], b"omega");
    drop(line);

    let reopened = WorldLine::open(&path.0, dem).unwrap();
    assert_eq!(reopened.witness_count(), 3);
    assert_eq!(reopened.document_manifest(), manifest);
    assert_eq!(reopened.verify().unwrap().segments_verified, 3);
}

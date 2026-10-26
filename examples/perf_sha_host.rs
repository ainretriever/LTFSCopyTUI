//! Host-only SHA benchmark. No tape access and no destination files.
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    time::Instant,
};
fn cpu() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) },
        0
    );
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}
fn main() {
    let path = std::env::args().nth(1).expect("NFS source file required");
    let expected = std::env::args()
        .nth(2)
        .expect("expected full SHA256 required");
    // Exercise exactly the sha2 dependency/API used by the application.
    assert_eq!(
        format!("{:x}", Sha256::digest(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let mut file = File::open(path).unwrap();
    let size = file.metadata().unwrap().len();
    let mut buffer = vec![0u8; 512 * 1024];
    file.read_exact(&mut buffer).unwrap();
    let mut prefix_hash = None;
    for round in 1..=3 {
        // Rotate order to reduce a fixed warm-up/order bias.
        let modes = if round % 2 == 1 {
            ["memory_sha", "read", "read_sha"]
        } else {
            ["read_sha", "read", "memory_sha"]
        };
        for mode in modes {
            file.seek(SeekFrom::Start(0)).unwrap();
            let started = Instant::now();
            let cpu_start = cpu();
            let mut hasher = Sha256::new();
            let mut bytes = 0u64;
            while bytes < 1024 * 1024 * 1024 {
                if mode != "memory_sha" {
                    file.read_exact(&mut buffer).unwrap();
                }
                if mode != "read" {
                    hasher.update(std::hint::black_box(&buffer));
                }
                bytes += buffer.len() as u64;
            }
            let seconds = started.elapsed().as_secs_f64();
            let cpu_seconds = cpu() - cpu_start;
            let digest = (mode != "read").then(|| format!("{:x}", hasher.finalize()));
            if mode == "read_sha" {
                if let Some(previous) = &prefix_hash {
                    assert_eq!(previous, digest.as_ref().unwrap());
                }
                prefix_hash = digest.clone();
            }
            println!(
                "{}",
                serde_json::json!({"mode":mode,"round":round,"bytes":bytes,"seconds":seconds,"cpu_seconds":cpu_seconds,"MB_s":bytes as f64/seconds/1e6,"sha256":digest})
            );
        }
    }
    file.seek(SeekFrom::Start(0)).unwrap();
    let started = Instant::now();
    let cpu_start = cpu();
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        bytes += n as u64;
    }
    let seconds = started.elapsed().as_secs_f64();
    let cpu_seconds = cpu() - cpu_start;
    let actual = format!("{:x}", hasher.finalize());
    let verified = actual == expected && bytes == size;
    println!(
        "{}",
        serde_json::json!({"mode":"full_read_sha","bytes":bytes,"seconds":seconds,"cpu_seconds":cpu_seconds,"MB_s":bytes as f64/seconds/1e6,"sha256":actual,"verified":verified})
    );
    assert!(verified, "full source SHA mismatch");
}

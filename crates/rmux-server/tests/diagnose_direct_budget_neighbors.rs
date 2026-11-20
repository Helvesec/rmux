//! L07.3: direct admission must include lossy UTF-8's decoded byte cost.
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rmux_server::config_paths::{ConfigPathExpansionError, ConfigPathMatchBudget};

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn check_decoded_boundary(bytes: Vec<u8>, debit: bool) {
    let fixture = Fixture(std::env::temp_dir().join(format!(
        "rmux-decoded-budget-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )));
    fs::create_dir(&fixture.0).unwrap();
    for group in ["A", "B"] {
        let dir = fixture.0.join(group);
        fs::create_dir(&dir).unwrap();
        for index in 0..128 {
            fs::write(dir.join(format!("{index:03}.conf")), "# valid\n").unwrap();
        }
    }
    let middle = fixture.0.join("middle.conf");
    fs::write(&middle, bytes).unwrap();
    let mut budget = ConfigPathMatchBudget::default();
    assert_eq!(
        budget.expand(&fixture.0.join("A/*.conf")).unwrap().len(),
        128
    );
    assert_eq!(budget.expand(&middle).unwrap(), vec![middle]);
    let tail = budget.expand(&fixture.0.join("B/*.conf"));
    if debit {
        assert_eq!(tail, Err(ConfigPathExpansionError::TooManyMatches));
        assert!(budget.is_exhausted());
    } else {
        assert_eq!(
            tail.unwrap().len(),
            128,
            "rejected direct input must cost zero"
        );
        assert!(!budget.is_exhausted());
    }
}

#[test]
fn raw_12_mib_invalid_utf8_rejected_after_decoding_costs_zero() {
    check_decoded_boundary(vec![0xff; 12 * 1024 * 1024], false);
}

#[test]
fn decoded_size_boundary_is_inclusive() {
    let mut bytes = vec![b'#'];
    bytes.extend(vec![0xff; (32 * 1024 * 1024 - 2) / 3]);
    bytes.push(b'\n');
    assert_eq!(String::from_utf8_lossy(&bytes).len(), 32 * 1024 * 1024);
    check_decoded_boundary(bytes.clone(), true);
    bytes.push(b'\n');
    assert_eq!(String::from_utf8_lossy(&bytes).len(), 32 * 1024 * 1024 + 1);
    check_decoded_boundary(bytes, false);
}

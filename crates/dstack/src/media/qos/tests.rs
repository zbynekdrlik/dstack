use super::*;

#[test]
fn the_request_writes_the_latency_as_the_kernel_reads_it_and_stays_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cpu_dma_latency");
    std::fs::write(&path, b"").unwrap();
    let held = hold(&path).expect("held");
    assert_eq!(std::fs::read(&path).unwrap(), HOLD_US.to_ne_bytes());
    drop(held);
    let f = request(&path, 55).unwrap();
    drop(f);
    assert_eq!(std::fs::read(&path).unwrap(), 55i32.to_ne_bytes());
}

#[test]
fn a_refused_device_is_none_and_nothing_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("none");
    assert!(hold(&path).is_none());
    assert!(!path.exists(), "the request never creates the device");
}

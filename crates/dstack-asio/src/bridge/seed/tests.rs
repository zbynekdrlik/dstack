// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn seed() -> Seed {
    Seed {
        dante_rate: 96_000,
        driver_rate: 48_000,
        to_asio_ppm: Some(7.5),
        from_asio_ppm: Some(-7.25),
    }
}

/// The file's text, and a text read back as the same seed.
#[test]
fn a_seed_writes_its_lines_and_reads_back() {
    let text = seed().text();
    assert_eq!(
        text,
        "# dstack-asio bridge: the ASRC's last locked rates (dstack #382)\n\
         dante_rate = 96000\ndriver_rate = 48000\nto_asio_ppm = 7.5\nfrom_asio_ppm = -7.25\n"
    );
    assert_eq!(Seed::parse(&text), Ok(seed()));
    let one = Seed {
        to_asio_ppm: None,
        ..seed()
    };
    assert!(!one.text().contains("to_asio_ppm"));
    assert_eq!(Seed::parse(&one.text()), Ok(one));
    // Blank lines, comments and spaces around the parts are fine.
    assert_eq!(
        Seed::parse("\n  # c\n driver_rate=48000 \ndante_rate =96000\n"),
        Ok(Seed {
            to_asio_ppm: None,
            from_asio_ppm: None,
            ..seed()
        })
    );
}

/// What is not a seed is refused with its reason.
#[test]
fn what_is_no_seed_is_refused() {
    let rates = "dante_rate = 96000\ndriver_rate = 48000\n";
    for (text, why) in [
        ("dante_rate = 96000\n", "no dante_rate or driver_rate"),
        ("driver_rate = 48000\n", "no dante_rate or driver_rate"),
        ("dante_rate 96000\n", "line 1: no `key = value`"),
        ("dante_rate = 0\n", "dante_rate: `0` is no rate"),
        ("driver_rate = x\n", "driver_rate: `x` is no rate"),
        ("latency = 1\n", "line 1: unknown key `latency`"),
    ] {
        assert_eq!(Seed::parse(text), Err(why.to_string()), "{text}");
    }
    for value in ["300.5", "-300.5", "NaN", "inf", "seven"] {
        let text = format!("{rates}to_asio_ppm = {value}\n");
        assert_eq!(
            Seed::parse(&text),
            Err(format!("to_asio_ppm: `{value}` is no rate within ±300 ppm"))
        );
        let text = format!("{rates}from_asio_ppm = {value}\n");
        assert!(Seed::parse(&text).is_err(), "{value}");
    }
    // The bound itself is a rate.
    let text = format!("{rates}to_asio_ppm = 300\nfrom_asio_ppm = -300\n");
    let s = Seed::parse(&text).unwrap();
    assert_eq!(
        (s.to_asio_ppm, s.from_asio_ppm),
        (Some(300.0), Some(-300.0))
    );
}

/// A seed counts only at the rates it was locked at.
#[test]
fn a_seed_is_for_its_own_rates_only() {
    assert_eq!(seed().for_rates(96_000, 48_000), Some(seed()));
    assert_eq!(seed().for_rates(48_000, 48_000), None);
    assert_eq!(seed().for_rates(96_000, 96_000), None);
}

/// The file: none yet, written whole, read back; a broken one is an error.
#[test]
fn the_seed_file_is_written_whole_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.seed");
    assert_eq!(Seed::read(&path), Ok(None), "no file yet");
    seed().write(&path).unwrap();
    assert_eq!(Seed::read(&path), Ok(Some(seed())));
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    assert!(!std::path::Path::new(&tmp).exists(), "renamed over");
    std::fs::write(&path, "garbage\n").unwrap();
    let err = Seed::read(&path).unwrap_err();
    assert!(err.ends_with("line 1: no `key = value`"), "{err}");
    // A directory where the file should be: no read, no write.
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    assert!(Seed::read(&sub).is_err());
    assert!(seed().write(&sub).is_err());
}

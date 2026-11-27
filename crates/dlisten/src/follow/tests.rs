// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn out(name: &str, default: bool) -> Output {
    Output {
        name: name.into(),
        default,
        config: "48000 Hz, 2 ch, f32".into(),
    }
}

/// The notebook of 26.11.2026: the speakers are the default; the
/// headphones are listed only while plugged in.
fn speakers() -> Output {
    out("Speakers (Realtek(R) Audio)", true)
}

fn phones() -> Output {
    out("Headphones (Realtek(R) Audio)", false)
}

fn named() -> Want {
    Want::of(Some("Headphones"))
}

#[test]
fn the_want_follows_the_command_line() {
    assert_eq!(Want::of(None), Want::Free);
    assert_eq!(
        Want::of(Some("Headphones")),
        Want::Named("Headphones".into())
    );
}

#[test]
fn a_named_output_missing_at_the_start_is_waited_for_never_the_default() {
    // Booted with the headphones out: no output plays, no error.
    assert_eq!(start(&[speakers()], &named(), None), Ok((None, None)));
    // Nothing listed at all: the same wait.
    assert_eq!(start(&[], &named(), None), Ok((None, None)));
    // A saved choice does not stand in for it either.
    assert_eq!(
        start(&[speakers()], &named(), Some("Speakers (Realtek(R) Audio)")),
        Ok((None, None))
    );
}

#[test]
fn a_named_output_there_at_the_start_plays() {
    assert_eq!(
        start(&[speakers(), phones()], &named(), None),
        Ok((Some(1), None))
    );
    assert_eq!(
        start(&[speakers(), phones()], &Want::of(Some("PHONES")), None),
        Ok((Some(1), None)),
        "any case"
    );
}

#[test]
fn an_exact_name_wins_over_one_that_holds_the_text() {
    let list = [out("Speakers (USB)", false), out("Speakers", true)];
    assert_eq!(
        start(&list, &Want::of(Some("speakers")), None),
        Ok((Some(1), None))
    );
    assert_eq!(
        next(&Want::of(Some("speakers")), None, &list),
        Step::Open(1)
    );
}

#[test]
fn the_named_output_is_opened_when_it_appears() {
    // Waiting (nothing plays): the headphones plugged in.
    assert_eq!(next(&named(), None, &[speakers()]), Step::Wait);
    assert_eq!(next(&named(), None, &[]), Step::Wait);
    assert_eq!(next(&named(), None, &[speakers(), phones()]), Step::Open(1));
}

#[test]
fn a_named_output_that_goes_away_plays_nothing_never_the_default() {
    // 26.11.2026 17:17:40: the headphones unplugged while playing.
    let p = phones();
    assert_eq!(next(&named(), Some(&p), &[speakers()]), Step::Wait);
    assert_eq!(next(&named(), Some(&p), &[]), Step::Wait);
    // 17:20:59: back.
    assert_eq!(next(&named(), None, &[speakers(), phones()]), Step::Open(1));
}

#[test]
fn the_named_output_playing_is_kept_by_its_exact_name() {
    let p = phones();
    // Moved in the list: kept, not opened again.
    assert_eq!(
        next(&named(), Some(&p), &[phones(), speakers()]),
        Step::Keep {
            index: 0,
            reopen: false
        }
    );
    // Its format changed: opened again.
    let mut other = phones();
    other.config = "44100 Hz, 2 ch, f32".into();
    assert_eq!(
        next(&named(), Some(&p), &[speakers(), other]),
        Step::Keep {
            index: 1,
            reopen: true
        }
    );
    // Gone, another output that holds the text is there: that one.
    let usb = out("Headphones (USB)", false);
    assert_eq!(next(&named(), Some(&p), &[speakers(), usb]), Step::Open(1));
}

#[test]
fn a_run_without_output_keeps_its_fallback_to_the_default() {
    // The start: the saved choice, else the default.
    assert_eq!(
        start(
            &[speakers(), phones()],
            &Want::Free,
            Some("Headphones (Realtek(R) Audio)")
        ),
        Ok((Some(1), None))
    );
    assert_eq!(
        start(&[speakers(), phones()], &Want::Free, None),
        Ok((Some(0), None))
    );
    assert!(start(&[], &Want::Free, None).is_err());
    let (index, note) = start(&[speakers()], &Want::Free, Some("Headphones")).unwrap();
    assert_eq!(index, Some(0));
    assert!(note.unwrap().contains("is not there"));
    // At run time: the output playing by name, the default when it goes.
    let p = phones();
    assert_eq!(
        next(&Want::Free, Some(&p), &[phones(), speakers()]),
        Step::Keep {
            index: 0,
            reopen: false
        }
    );
    assert_eq!(next(&Want::Free, Some(&p), &[speakers()]), Step::Open(0));
    assert_eq!(next(&Want::Free, Some(&p), &[]), Step::Wait);
    let mut none = vec![out("A", false), out("B", false)];
    assert_eq!(next(&Want::Free, Some(&p), &none), Step::Open(0));
    none[1].default = true;
    assert_eq!(next(&Want::Free, None, &none), Step::Open(1));
    assert_eq!(next(&Want::Free, None, &[]), Step::Wait);
}

#[test]
fn a_missing_output_warns_once_then_a_line_a_minute() {
    let mut log = MissingLog::new("Headphones");
    let warn = log.due(0).expect("a WARN at once");
    assert_eq!(
        warn,
        Line::Warn(
            "dlisten: no output holding \"Headphones\" is there (--output): playing nothing \
             until it comes, never on another output"
                .into()
        )
    );
    // Asked every 5 s: nothing until the minute.
    for s in (5..60).step_by(5) {
        assert_eq!(log.due(s), None, "{s} s");
    }
    let minute = |m: u64| {
        Some(Line::Info(format!(
            "dlisten: still waiting for an output holding \"Headphones\" (missing {m} min): \
             playing nothing"
        )))
    };
    assert_eq!(log.due(60), minute(1));
    assert_eq!(log.due(65), None);
    assert_eq!(log.due(119), None);
    assert_eq!(log.due(120), minute(2));
    // A late ask (the list read failed meanwhile): one line, not a burst.
    assert_eq!(log.due(250), minute(4));
    assert_eq!(log.due(255), None);
    assert_eq!(log.due(300), minute(5));
}

#[test]
fn the_found_line_says_how_long_it_was_missing() {
    let log = MissingLog::new("Headphones");
    assert_eq!(
        log.found("Headphones (Realtek(R) Audio)", 199),
        "dlisten: the output \"Headphones (Realtek(R) Audio)\" is there again after 3 min 19 s: \
         playing on it"
    );
    assert_eq!(
        log.found("Headphones (Realtek(R) Audio)", 59),
        "dlisten: the output \"Headphones (Realtek(R) Audio)\" is there again after 59 s: \
         playing on it"
    );
    assert_eq!(
        log.found("Headphones (Realtek(R) Audio)", 60),
        "dlisten: the output \"Headphones (Realtek(R) Audio)\" is there again after 1 min 0 s: \
         playing on it"
    );
}

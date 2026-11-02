use super::*;

const FQ_CODEL: &str =
    r#"[{"kind":"fq_codel","handle":"0:","root":true,"refcnt":2,"options":{"limit":10240}}]"#;

#[test]
fn the_root_qdisc_is_read_from_tc_s_answer() {
    assert_eq!(root_kind(FQ_CODEL).as_deref(), Some("fq_codel"));
    let mq = r#"[{"kind":"fq_codel","handle":"0:","parent":"1:1"},{"kind":"mq","handle":"1:","root":true}]"#;
    assert_eq!(
        root_kind(mq).as_deref(),
        Some("mq"),
        "the root, not a child"
    );
    assert_eq!(
        root_kind(r#"[{"kind":"noqueue","root":true}]"#).as_deref(),
        Some("noqueue")
    );
    for junk in [
        "",
        "[]",
        "{}",
        "[{\"kind\":\"noqueue\"}]",
        "[{\"root\":true}]",
        "not json",
    ] {
        assert_eq!(root_kind(junk), None, "{junk:?}");
    }
}

#[test]
fn only_no_queue_or_a_lockless_one_passes() {
    for k in ["noqueue", "pfifo_fast"] {
        assert_eq!(verdict(Some(k.into())), Verdict::Direct(k.into()));
    }
    for k in ["fq_codel", "mq", "fq", "pfifo"] {
        assert_eq!(verdict(Some(k.into())), Verdict::Queues(k.into()));
    }
    assert_eq!(verdict(None), Verdict::Unknown);
}

#[test]
fn an_interface_tc_does_not_know_is_unknown() {
    assert_eq!(check("zyxnone0"), Verdict::Unknown);
}

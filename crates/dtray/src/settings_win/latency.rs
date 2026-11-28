// SPDX-License-Identifier: MIT OR Apache-2.0

//! The window's latency texts (dstack #512): the spare buffers and the
//! latency the DAW is told, from the current tab's chosen values and its
//! device's status. Every text is [`told`]'s.

use super::*;

/// The status line of the device named `name` (its JSON), when it runs.
fn status_of<'a>(st: &'a State, name: &str) -> Option<&'a str> {
    st.readings
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .and_then(|(_, r)| r.as_deref().ok())
}

/// The current tab's spare buffers and told latency texts, from its
/// chosen values and its device's status ([`told`]).
pub(super) fn latency_texts(st: &State) -> Option<(String, String)> {
    let (_, new) = st.tabs.get(st.cur)?.fields.as_ref().ok()?;
    let status = status_of(st, &new.name);
    Some((told::spare_text(new, status), told::told_text(new, status)))
}

/// The spare buffers and told latency texts into their controls (only
/// when they changed: a static redrawn every second flickers).
pub(super) fn show_latency(h: HWND, texts: Option<(String, String)>) {
    let Some((spare, told)) = texts else {
        return;
    };
    for (id, text) in [(ID_SPARE, spare), (ID_TOLD, told)] {
        if control_text(h, id) != text {
            set_text(h, id, &text);
        }
    }
}

/// A new buffer pick: the app reserve list again from the least dstack
/// takes for that buffer, the reserve kept while it is taken, else raised
/// to it ([`dasio::check::reserve_for`]).
pub(super) fn refill_reserve(st: &mut State) {
    let h = st.hwnd;
    let Some(Ok((_, new))) = st.tabs.get_mut(st.cur).map(|t| t.fields.as_mut()) else {
        return;
    };
    new.work_margin = dasio::check::reserve_for(new);
    let list = choices::work_margins(
        new.work_margin,
        dasio::check::least_work_margin(new),
        new.sample_rate,
    );
    fill(
        h,
        ID_WMARGIN,
        &list,
        choices::index_of(&list, &new.work_margin),
    );
    st.lists.reserve = list;
}

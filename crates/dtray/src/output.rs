// SPDX-License-Identifier: MIT OR Apache-2.0

//! An app's Output menu (dstack #139, the owner's ROZHODNUTÉ of 6.11.2026:
//! dtray is the one tray for every Dante app on the PC). An app whose
//! status part (`app`) carries `outputs`, a list of names, and `output`,
//! the index of the one in use (dlisten: its sound output), gets an Output
//! menu in its device's submenu; choosing one sends `app output <index>` to
//! the device's local endpoint ([`crate::probe::ask`]), which hands it to
//! the app's own command hook. Pure, every platform.

use serde_json::Value;

use crate::model::app_line;

/// The most outputs a menu shows.
pub const MAX_OUTPUTS: usize = 64;

/// One output in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputChoice {
    /// What `app output <index>` names.
    pub index: usize,
    /// Its name as the menu shows it ([`app_line`]).
    pub label: String,
    /// The one in use.
    pub checked: bool,
}

/// The outputs of a status `line`'s app part: each name of `app.outputs`
/// (a non-text entry keeps its index but shows no item), the one at
/// `app.output` checked; none without such a list, at most
/// [`MAX_OUTPUTS`].
pub fn choices(line: &str) -> Vec<OutputChoice> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let app = v.get("app");
    let current = app
        .and_then(|a| a.get("output"))
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok());
    let Some(list) = app.and_then(|a| a.get("outputs")).and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .take(MAX_OUTPUTS)
        .enumerate()
        .filter_map(|(index, name)| {
            let label = app_line(name.as_str()?);
            (!label.is_empty()).then_some(OutputChoice {
                index,
                label,
                checked: current == Some(index),
            })
        })
        .collect()
}

/// The request that chooses output `index`.
pub fn request(index: usize) -> String {
    format!("app output {index}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(app: &str) -> String {
        format!(r#"{{"clock":{{"ready":1}},"app":{app}}}"#)
    }

    fn choice(index: usize, label: &str, checked: bool) -> OutputChoice {
        OutputChoice {
            index,
            label: label.into(),
            checked,
        }
    }

    #[test]
    fn an_app_with_outputs_gets_them_the_current_checked() {
        let line = status(r#"{"line":"x","outputs":["Speakers","HDMI 2"],"output":1}"#);
        assert_eq!(
            choices(&line),
            [choice(0, "Speakers", false), choice(1, "HDMI 2", true)]
        );
    }

    #[test]
    fn no_list_no_menu() {
        assert!(choices(&status(r#"{"line":"x"}"#)).is_empty());
        assert!(choices(&status(r#"{"outputs":"Speakers"}"#)).is_empty());
        assert!(choices(r#"{"clock":{"ready":1}}"#).is_empty());
        assert!(choices("not json").is_empty());
    }

    #[test]
    fn odd_entries_keep_the_indexes_and_a_missing_current_checks_none() {
        let line = status(r#"{"outputs":[3,"","Line\nOut",null,"USB"]}"#);
        assert_eq!(
            choices(&line),
            [choice(2, "Line Out", false), choice(4, "USB", false)]
        );
        let neg = status(r#"{"outputs":["A"],"output":-1}"#);
        assert_eq!(choices(&neg), [choice(0, "A", false)]);
    }

    #[test]
    fn a_long_list_and_long_names_are_bounded() {
        let names: Vec<String> = (0..100).map(|i| format!("\"{i}\"")).collect();
        let line = status(&format!(
            r#"{{"outputs":[{}],"output":70}}"#,
            names.join(",")
        ));
        let got = choices(&line);
        assert_eq!(got.len(), MAX_OUTPUTS);
        assert!(got.iter().all(|c| !c.checked), "70 is past the menu");
        let long = status(&format!(r#"{{"outputs":["{}"]}}"#, "x".repeat(100)));
        assert_eq!(choices(&long)[0].label.chars().count(), 64);
    }

    #[test]
    fn the_request_names_the_index() {
        assert_eq!(request(3), "app output 3");
    }
}

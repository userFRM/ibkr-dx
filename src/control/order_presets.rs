//! Requests and answers for the values in an account's order presets.

use crate::control::attached_presets::request_selector;
use crate::control::contracts::tag_sequence;

/// The answer to a request for one preset's values.
///
/// Fields remain in wire order. Price specifications repeat tag 4084, followed
/// by their quantity, unit, parent-price flag and price selector; a map would
/// discard all but the last specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetValues {
    /// The request this answers.
    pub request_key: String,
    /// The preset's selector, as a request for its values names it.
    pub key: String,
    /// The preset's variant, active flag and other attributes, as stated.
    pub attributes: String,
    /// The venue's error text. An error is not an empty preset.
    pub error: Option<String>,
    /// The values, including repeated price specifications.
    pub fields: Vec<(u32, String)>,
}

/// Build a request for the values behind a preset key returned by the list.
///
/// The key is rebuilt as a gateway rebuilds it. The enclosing user-message
/// header is supplied by the connection.
pub fn values_request(request_key: &str, key: &str) -> Vec<(u32, String)> {
    vec![
        (6556, request_key.into()),
        (6040, "193".into()),
        (8166, "G".into()),
        (8176, "1".into()),
        (8168, request_selector(key)),
    ]
}

/// Read a values answer without treating it as a preset list.
///
/// A values answer has no list count. Its selector is held as a request names
/// it. Spaces in value fields are decoded from `%20`.
pub fn parse_values(message: &[u8]) -> Option<PresetValues> {
    let fields = tag_sequence(message);
    let value = |tag| fields.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.as_str());
    if value(6040)? != "194" || value(8166)? != "G" {
        return None;
    }
    let request_key = value(6556)?.to_string();
    let key = request_selector(value(8168)?);
    let attributes = value(8169).unwrap_or_default().replace("%20", " ");
    let error = value(58).filter(|s| !s.is_empty()).map(str::to_string);
    Some(PresetValues {
        request_key,
        key,
        attributes,
        error,
        fields: fields
            .into_iter()
            .filter(|(tag, _)| {
                !matches!(
                    tag,
                    8 | 9
                        | 10
                        | 34
                        | 35
                        | 43
                        | 49
                        | 52
                        | 56
                        | 58
                        | 6040
                        | 6556
                        | 8166
                        | 8167
                        | 8168
                        | 8169
                        | 8170
                        | 8176
                        | 8349
                )
            })
            .map(|(tag, value)| (tag, value.replace("%20", " ")))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_are_rebuilt_without_escaping_existing_components_again() {
        for key in ["s=STK&tc=A B%26C%3DD%25", "tc=A%20B%26C%3DD%25&s=STK"] {
            let fields = values_request("OPR.3", key);
            assert_eq!(fields.last().unwrap(), &(8168, "s=STK&tc=A%20B%26C%3DD%25".into()));
            let answer =
                format!("6040=194\x018166=G\x016556=OPR.3\x018168={key}\x014083=A%20B\x01");
            let values = parse_values(answer.as_bytes()).unwrap();
            assert_eq!(values.key, "s=STK&tc=A%20B%26C%3DD%25");
            assert_eq!(values.fields, vec![(4083, "A B".into())]);
        }
    }

    #[test]
    fn a_values_answer_keeps_each_price_specification() {
        let answer = parse_values(b"35=U\x016040=194\x018166=G\x016556=OPR.3\x018168=s=STK\x018169=v=1&a=1\x014074=1\x014084=4077\x014085=2\x014086=1\x014084=4078\x014085=-1\x014088=11\x01").unwrap();
        assert_eq!(answer.key, "s=STK");
        assert_eq!(answer.attributes, "v=1&a=1");
        assert_eq!(answer.request_key, "OPR.3");
        assert_eq!(
            answer.fields,
            vec![
                (4074, "1".into()),
                (4084, "4077".into()),
                (4085, "2".into()),
                (4086, "1".into()),
                (4084, "4078".into()),
                (4085, "-1".into()),
                (4088, "11".into()),
            ]
        );
        assert!(answer.error.is_none());
    }

    #[test]
    fn an_error_is_preserved_without_erasing_the_preset_key() {
        let answer = parse_values(b"6040=194\x018166=G\x016556=OPR.4\x018168=s=STK&tc=A%20B\x0158=Not found\x014083=#\x01").unwrap();
        assert_eq!(answer.key, "s=STK&tc=A%20B");
        assert_eq!(answer.error.as_deref(), Some("Not found"));
        assert_eq!(answer.fields, vec![(4083, "#".into())]);
    }

    #[test]
    fn another_operation_and_an_unaddressed_answer_are_not_values() {
        for msg in [
            &b"6040=194\x018166=L\x016556=OPR.2\x018167=0\x01"[..],
            &b"6040=194\x018166=S\x016556=OPR.2\x018168=s=STK\x01"[..],
            &b"6040=194\x018166=G\x018168=s=STK\x01"[..],
            &b"6040=194\x018166=G\x016556=OPR.3\x01"[..],
        ] {
            assert!(parse_values(msg).is_none());
        }
    }

    #[test]
    fn session_fields_are_not_preset_values() {
        let answer = parse_values(b"35=U\x0143=Y\x016040=194\x018166=G\x016556=OPR.3\x018167=1\x018168=s=STK\x014074=1\x018349=signature\x01").unwrap();
        assert_eq!(answer.fields, vec![(4074, "1".into())]);
    }
}
